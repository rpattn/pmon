//! `pmon -d`, `--stop` and `--status`.

use crate::sampler::{Sampler, owned_by};
use crate::store::{self, PROC_FILE, ProcRec, Ring, SYS_FILE, SysRec};
use crate::util::{fmt_bytes, fmt_dur, now_ms};
use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const INTERVAL: Duration = Duration::from_secs(2);
pub const DEFAULT_RETAIN: Duration = Duration::from_secs(3600);
const PIDFILE: &str = "pmon.pid";
const LOGFILE: &str = "daemon.log";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn open_pidfile(dir: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o644).open(dir.join(PIDFILE))
}

/// The pidfile is `flock`ed for the daemon's whole lifetime, so a stale file
/// left behind by a crash never blocks a new daemon.
fn try_lock(f: &File) -> bool {
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn read_pid(f: &File) -> Option<i32> {
    let mut b = [0u8; 32];
    let n = f.read_at(&mut b, 0).ok()?;
    std::str::from_utf8(&b[..n]).ok()?.trim().parse().ok()
}

fn write_pid(f: &File, pid: i32) -> io::Result<()> {
    f.set_len(0)?;
    f.write_all_at(format!("{pid}\n").as_bytes(), 0)
}

/// `Some(pid)` if a daemon holds the lock (pid 0 if it could not be read).
fn daemon_pid(dir: &Path) -> Option<i32> {
    let f = File::open(dir.join(PIDFILE)).ok()?;
    if try_lock(&f) {
        return None; // dropping `f` releases the lock
    }
    Some(read_pid(&f).unwrap_or(0))
}

pub fn start(retain: Duration, foreground: bool) -> Result<i32> {
    if retain < INTERVAL * 2 {
        bail!("--retain must be at least {}", fmt_dur(INTERVAL * 2));
    }
    let dir = store::state_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let pidf = open_pidfile(&dir).context("opening pidfile")?;
    if !try_lock(&pidf) {
        eprintln!("pmon: daemon already running (pid {})", read_pid(&pidf).unwrap_or(0));
        return Ok(1);
    }

    // Everything that can fail is set up before forking so errors reach the terminal.
    let uid = unsafe { libc::getuid() };
    let mut sampler = Sampler::new()?;
    sampler.refresh_processes(false);
    let n_user = sampler.processes().filter(|p| owned_by(p, uid)).count() as u64;
    let slots = (retain.as_millis().div_ceil(INTERVAL.as_millis()) as u64).max(2);
    // Room for twice today's process count per sample, at least 128.
    let per_sample = (n_user * 2).next_power_of_two().max(128);
    let interval_ms = INTERVAL.as_millis() as u32;
    let sys_ring = Ring::<SysRec>::open_writer(&dir.join(SYS_FILE), slots, interval_ms, |c| c == slots)?;
    let want = slots * per_sample;
    let proc_ring =
        Ring::<ProcRec>::open_writer(&dir.join(PROC_FILE), want, interval_ms, |c| c >= want && c <= want * 4)?;

    if foreground {
        write_pid(&pidf, std::process::id() as i32)?;
        eprintln!("pmon daemon running in foreground (pid {}), writing to {}", std::process::id(), dir.display());
    } else if let Some(pid) = daemonize(&pidf, &dir)? {
        println!(
            "pmon daemon started (pid {pid}): sampling every {}, keeping {}, data in {}",
            fmt_dur(INTERVAL),
            fmt_dur(retain),
            dir.display()
        );
        return Ok(0);
    }

    unsafe {
        let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    run_loop(sampler, uid, sys_ring, proc_ring);
    let _ = pidf.set_len(0);
    Ok(0)
}

/// Classic double fork. Returns `Some(daemon pid)` in the original process and
/// `None` in the daemon.
fn daemonize(pidf: &File, dir: &Path) -> Result<Option<i32>> {
    io::stdout().flush().ok();
    io::stderr().flush().ok();
    unsafe {
        match libc::fork() {
            -1 => bail!("fork failed: {}", io::Error::last_os_error()),
            0 => {
                libc::setsid();
                match libc::fork() {
                    -1 => libc::_exit(1),
                    0 => {}
                    pid => {
                        let code = if write_pid(pidf, pid).is_ok() { 0 } else { 1 };
                        libc::_exit(code);
                    }
                }
                libc::chdir(c"/".as_ptr());
                if let Ok(null) = File::open("/dev/null") {
                    libc::dup2(null.as_raw_fd(), 0);
                }
                if let Ok(log) = File::create(dir.join(LOGFILE)) {
                    libc::dup2(log.as_raw_fd(), 1);
                    libc::dup2(log.as_raw_fd(), 2);
                }
                Ok(None)
            }
            child => {
                let mut status = 0;
                libc::waitpid(child, &mut status, 0);
                if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
                    bail!("failed to start daemon");
                }
                Ok(Some(read_pid(pidf).unwrap_or(0)))
            }
        }
    }
}

fn run_loop(mut sampler: Sampler, uid: u32, mut sys_ring: Ring<SysRec>, mut proc_ring: Ring<ProcRec>) {
    let mut buf = Vec::new();
    let mut procs: Vec<ProcRec> = Vec::new();
    // Prime cpu counters; the first real sample comes one interval later.
    sampler.system();
    let mut next = Instant::now() + INTERVAL;
    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now < next {
            std::thread::sleep((next - now).min(Duration::from_millis(250)));
            continue;
        }
        next += INTERVAL;
        if next <= now {
            next = now + INTERVAL;
        }

        let ts_ms = now_ms();
        let (cpu, mem_used, mem_total) = sampler.system();
        sampler.refresh_processes(false);
        procs.clear();
        for p in sampler.processes().filter(|p| owned_by(p, uid)) {
            let mut r = ProcRec {
                ts_ms,
                start_time: p.start_time(),
                rss: p.memory(),
                pid: p.pid().as_u32(),
                cpu: p.cpu_usage(),
                name: [0; 16],
            };
            r.set_name(p.name().as_encoded_bytes());
            procs.push(r);
        }
        let res = sys_ring
            .append(&[SysRec { ts_ms, cpu, mem_used, mem_total }], &mut buf)
            .and_then(|_| proc_ring.append(&procs, &mut buf));
        if let Err(e) = res {
            eprintln!("pmon: writing history failed: {e}");
        }
    }
}

pub fn stop() -> Result<i32> {
    let dir = store::state_dir();
    let Some(pid) = daemon_pid(&dir) else {
        eprintln!("pmon: daemon is not running");
        return Ok(1);
    };
    if pid <= 0 {
        bail!("daemon is running but its pid could not be read from {}", dir.join(PIDFILE).display());
    }
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        bail!("cannot signal pid {pid}: {}", io::Error::last_os_error());
    }
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if daemon_pid(&dir).is_none() {
            println!("pmon daemon stopped (pid {pid})");
            return Ok(0);
        }
    }
    bail!("daemon (pid {pid}) did not exit within 5s")
}

pub fn status() -> Result<i32> {
    let dir = store::state_dir();
    let running = daemon_pid(&dir);
    match running {
        Some(pid) => println!("daemon:     running (pid {pid})"),
        None => println!("daemon:     not running"),
    }
    println!("data:       {}", dir.display());

    let now = now_ms();
    let ago = |ts: u64| fmt_dur(Duration::from_millis(now.saturating_sub(ts)));
    let span = |a: u64, b: u64| fmt_dur(Duration::from_millis(b.saturating_sub(a)));
    let mut disk = 0;
    match Ring::<SysRec>::open_reader(&dir.join(SYS_FILE))? {
        None => println!("history:    none stored (start with `pmon -d`)"),
        Some(r) => {
            disk += r.file().metadata()?.blocks() * 512;
            let interval = Duration::from_millis(r.interval_ms as u64);
            println!("interval:   {}, retention {}", fmt_dur(interval), fmt_dur(interval * r.capacity() as u32));
            match r.span()? {
                Some((old, new)) => println!(
                    "history:    {} samples covering {} (last sample {} ago)",
                    r.stored()?,
                    span(old, new),
                    ago(new)
                ),
                None => println!("history:    empty"),
            }
        }
    }
    if let Some(r) = Ring::<ProcRec>::open_reader(&dir.join(PROC_FILE))? {
        disk += r.file().metadata()?.blocks() * 512;
        if let Some((old, new)) = r.span()? {
            println!(
                "processes:  {} records (capacity {}) covering {}",
                r.stored()?,
                r.capacity(),
                span(old, new)
            );
        }
    }
    if disk > 0 {
        println!("disk:       {}", fmt_bytes(disk as f64));
    }
    Ok(if running.is_some() { 0 } else { 1 })
}
