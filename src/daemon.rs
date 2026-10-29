//! `pmon -d`, `--stop` and `--status`.
//!
//! Works the same on Unix and Windows: `pmon -d` re-launches itself detached
//! with `--foreground`, the running daemon holds an exclusive lock on
//! `pmon.lock` for its whole lifetime, and `--stop` asks it to exit by creating
//! a `stop` file that it polls for (Ctrl-C / SIGTERM also work).

use crate::sampler::{Sampler, owned_by};
use crate::store::{self, PROC_FILE, ProcRec, Ring, SYS_FILE, SysRec};
use crate::util::{fmt_bytes, fmt_dur, now_ms};
use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const INTERVAL: Duration = Duration::from_secs(2);
pub const DEFAULT_RETAIN: Duration = Duration::from_secs(3600);
const LOCKFILE: &str = "pmon.lock";
const PIDFILE: &str = "pmon.pid";
const STOPFILE: &str = "stop";
const LOGFILE: &str = "daemon.log";

fn read_pid(dir: &Path) -> Option<u32> {
    fs::read_to_string(dir.join(PIDFILE)).ok()?.trim().parse().ok()
}

/// `Some(pid)` while a daemon holds the lock (pid 0 if it isn't written yet).
/// A stale pidfile left by a crash therefore never counts as running.
fn daemon_pid(dir: &Path) -> Option<u32> {
    let f = File::open(dir.join(LOCKFILE)).ok()?;
    match f.try_lock() {
        Err(TryLockError::WouldBlock) => Some(read_pid(dir).unwrap_or(0)),
        _ => None, // got it (released on drop), or locking unsupported
    }
}

pub fn is_running() -> bool {
    daemon_pid(&store::state_dir()).is_some()
}

/// `--retain` if given, else whatever the stored history was recorded with
/// (a different retention recreates the files, losing the history), else 1h.
fn resolve_retain(dir: &Path, retain: Option<Duration>) -> Result<Duration> {
    let stored = || {
        let r = Ring::<SysRec>::open_reader(&dir.join(SYS_FILE)).ok().flatten()?;
        Some(Duration::from_millis(r.interval_ms as u64 * r.capacity()))
    };
    let retain = retain.or_else(stored).unwrap_or(DEFAULT_RETAIN);
    if retain < INTERVAL * 2 {
        bail!("--retain must be at least {}", fmt_dur(INTERVAL * 2));
    }
    Ok(retain)
}

pub fn start(retain: Option<Duration>, foreground: bool) -> Result<i32> {
    let dir = store::state_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    if let Some(pid) = daemon_pid(&dir) {
        eprintln!("pmon: daemon already running (pid {pid})");
        return Ok(1);
    }
    let retain = resolve_retain(&dir, retain)?;
    if foreground {
        return run_foreground(&dir, retain);
    }
    let pid = spawn_background(&dir, retain)?;
    println!(
        "pmon daemon started (pid {pid}): sampling every {}, keeping {}, data in {}",
        fmt_dur(INTERVAL),
        fmt_dur(retain),
        dir.display()
    );
    Ok(0)
}

/// Starts the daemon from inside the TUI (nothing printed). Returns its pid
/// and retention.
pub fn start_quiet() -> Result<(u32, Duration)> {
    let dir = store::state_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    if let Some(pid) = daemon_pid(&dir) {
        bail!("daemon already running (pid {pid})");
    }
    let retain = resolve_retain(&dir, None)?;
    Ok((spawn_background(&dir, retain)?, retain))
}

/// Starts `pmon --daemon --foreground` detached from this terminal and waits
/// until it has taken the lock, so start-up errors are still reported here.
fn spawn_background(dir: &Path, retain: Duration) -> Result<u32> {
    let log_path = dir.join(LOGFILE);
    let log = File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;
    let mut cmd = Command::new(std::env::current_exe().context("locating the pmon executable")?);
    cmd.args(["--daemon", "--foreground", "--retain"])
        .arg(humantime::format_duration(retain).to_string())
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = spawn_detached(&mut cmd).context("starting the daemon")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            bail!("daemon exited during start-up ({status})\n{}", log.trim_end());
        }
        if daemon_pid(dir) == Some(child.id()) {
            return Ok(child.id());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("daemon did not come up within 5s; see {}", log_path.display())
}

#[cfg(unix)]
fn spawn_detached(cmd: &mut Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    // New session: no controlling terminal, so closing it doesn't hang us up.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
}

#[cfg(windows)]
fn spawn_detached(cmd: &mut Command) -> std::io::Result<Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
        fn SetHandleInformation(h: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    // Windows hands every inheritable handle to the child, including our own
    // stdin/stdout/stderr. If those are pipes, whoever reads them would wait
    // for the daemon to exit. We're about to exit anyway, so stop sharing them.
    for which in [-10i32, -11, -12] {
        unsafe {
            let h = GetStdHandle(which as u32);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // Leave the terminal's job object if allowed, so closing the window
    // doesn't take the daemon with it; some jobs forbid that, so retry.
    cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|_| cmd.creation_flags(flags).spawn())
}

fn run_foreground(dir: &Path, retain: Duration) -> Result<i32> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCKFILE))
        .context("opening lock file")?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            eprintln!("pmon: daemon already running (pid {})", read_pid(dir).unwrap_or(0));
            return Ok(1);
        }
        Err(TryLockError::Error(e)) => bail!("cannot lock {}: {e}", dir.join(LOCKFILE).display()),
    }
    let _ = fs::remove_file(dir.join(STOPFILE));

    let mut sampler = Sampler::new()?;
    sampler.refresh_processes(false);
    let user = sampler.current_user().context("cannot determine the current user")?;
    let n_user = sampler.processes().filter(|p| owned_by(p, &user)).count() as u64;
    let slots = (retain.as_millis().div_ceil(INTERVAL.as_millis()) as u64).max(2);
    // Room for twice today's process count per sample, at least 128.
    let per_sample = (n_user * 2).next_power_of_two().max(128);
    let interval_ms = INTERVAL.as_millis() as u32;
    let sys_ring = Ring::<SysRec>::open_writer(&dir.join(SYS_FILE), slots, interval_ms, |c| c == slots)?;
    let want = slots * per_sample;
    let proc_ring =
        Ring::<ProcRec>::open_writer(&dir.join(PROC_FILE), want, interval_ms, |c| c >= want && c <= want * 4)?;

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst)).context("installing signal handler")?;
    fs::write(dir.join(PIDFILE), format!("{}\n", std::process::id())).context("writing pidfile")?;
    eprintln!("pmon daemon running (pid {}), writing to {}", std::process::id(), dir.display());

    run_loop(sampler, &user, sys_ring, proc_ring, &stop, &dir.join(STOPFILE));

    let _ = fs::remove_file(dir.join(PIDFILE));
    eprintln!("pmon daemon stopped");
    drop(lock);
    Ok(0)
}

fn run_loop(
    mut sampler: Sampler,
    user: &sysinfo::Uid,
    mut sys_ring: Ring<SysRec>,
    mut proc_ring: Ring<ProcRec>,
    stop: &AtomicBool,
    stop_file: &Path,
) {
    let mut buf = Vec::new();
    let mut procs: Vec<ProcRec> = Vec::new();
    // Prime cpu counters; the first real sample comes one interval later.
    sampler.system();
    let mut next = Instant::now() + INTERVAL;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if stop_file.exists() {
            let _ = fs::remove_file(stop_file);
            break;
        }
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
        for p in sampler.processes().filter(|p| owned_by(p, user)) {
            let mut r = ProcRec {
                ts_ms,
                start_time: p.start_time(),
                rss: p.memory(),
                pid: p.pid().as_u32(),
                cpu: p.cpu_usage(),
                name: [0; 32],
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
    fs::write(dir.join(STOPFILE), b"").context("writing stop request")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if daemon_pid(&dir).is_none() {
            println!("pmon daemon stopped (pid {pid})");
            return Ok(0);
        }
    }
    let _ = fs::remove_file(dir.join(STOPFILE));
    bail!("daemon (pid {pid}) did not exit within 5s")
}

fn disk_usage(f: &File) -> u64 {
    let Ok(m) = f.metadata() else { return 0 };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.blocks() * 512 // ring files are sparse until they fill
    }
    #[cfg(not(unix))]
    m.len()
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
            disk += disk_usage(r.file());
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
        disk += disk_usage(r.file());
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
