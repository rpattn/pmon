//! Thin wrapper over `sysinfo`.

use crate::agg::{self, METRICS};
use anyhow::{Result, anyhow};
use std::collections::HashSet;
use std::time::Instant;
use sysinfo::{
    DiskRefreshKind, Disks, Gpus, Networks, Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, Uid,
    UpdateKind,
};

pub struct Sampler {
    sys: System,
    disks: Disks,
    nets: Networks,
    /// `None` when no GPU can be read (no driver support, or e.g. WSL).
    gpus: Option<Gpus>,
    /// Seconds covered by the last system / all-process refresh, for rates.
    sys_secs: f64,
    procs_secs: f64,
    last_sys: Instant,
    last_procs: Instant,
    /// Processes seen before the latest refresh. A process's first disk
    /// reading is its whole lifetime's I/O, not a rate, so it's skipped.
    known: HashSet<(Pid, u64)>,
    fresh: HashSet<(Pid, u64)>,
}

/// Network interfaces that only carry traffic within the machine.
fn is_loopback(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_lowercase();
    name == "lo" || name.starts_with("loopback")
}

/// Virtual interfaces (container bridges and their veth pairs) whose traffic
/// also crosses a real interface, so counting them would double it.
fn is_virtual(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    ["veth", "br-", "docker", "virbr", "cni", "flannel", "vnet"].iter().any(|p| name.starts_with(p))
}

impl Sampler {
    pub fn new() -> Result<Self> {
        let err = |e: sysinfo::Error| anyhow!("cannot read system information: {e}");
        let mut sys = System::new().map_err(err)?;
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let disks = Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_io_usage()).map_err(err)?;
        let nets = Networks::new_with_refreshed_list().map_err(err)?;
        // Usage needs two readings on some platforms, so a listed GPU is enough.
        let gpus = Gpus::new_with_refreshed_list().ok().filter(|g| !g.is_empty());
        let now = Instant::now();
        Ok(Sampler {
            sys,
            disks,
            nets,
            gpus,
            sys_secs: 1.0,
            procs_secs: 1.0,
            last_sys: now,
            last_procs: now,
            known: HashSet::new(),
            fresh: HashSet::new(),
        })
    }

    pub fn ncpu(&self) -> usize {
        self.sys.cpus().len().max(1)
    }

    pub fn has_gpu(&self) -> bool {
        self.gpus.is_some()
    }

    /// One system-wide sample (indexed by the `agg` metric constants), plus
    /// total memory and total GPU memory in bytes.
    pub fn system(&mut self) -> ([f64; METRICS], u64, u64) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.disks.refresh_specifics(true, DiskRefreshKind::nothing().with_io_usage());
        self.nets.refresh(true);
        if let Some(g) = &mut self.gpus {
            g.refresh(false);
        }
        self.sys_secs = self.last_sys.elapsed().as_secs_f64().max(0.001);
        self.last_sys = Instant::now();

        let mut v = [0.0; METRICS];
        v[agg::CPU] = self.sys.global_cpu_usage() as f64;
        v[agg::MEM] = self.sys.used_memory() as f64;
        // Several mounts of one device (common on WSL) would count twice.
        let mut seen = HashSet::new();
        for d in self.disks.iter().filter(|d| seen.insert(d.name().to_owned())) {
            let u = d.usage();
            v[agg::DISK_R] += u.read_bytes as f64 / self.sys_secs;
            v[agg::DISK_W] += u.written_bytes as f64 / self.sys_secs;
        }
        for (_, n) in self.nets.iter().filter(|(name, _)| !is_loopback(name) && !is_virtual(name)) {
            v[agg::NET_RX] += n.received() as f64 / self.sys_secs;
            v[agg::NET_TX] += n.transmitted() as f64 / self.sys_secs;
        }
        let mut gpu_total = 0;
        if let Some(g) = &self.gpus {
            v[agg::GPU] = g.iter().filter_map(|g| g.usage()).fold(0.0, f32::max) as f64;
            v[agg::GPU_MEM] = g.iter().filter_map(|g| g.used_memory()).sum::<u64>() as f64;
            gpu_total = g.iter().filter_map(|g| g.total_memory()).sum();
        }
        (v, self.sys.total_memory(), gpu_total)
    }

    /// Per-interface `(name, received, sent)` in bytes/s over the last
    /// sample, busiest first.
    pub fn interfaces(&self) -> Vec<(String, f64, f64)> {
        let mut out: Vec<_> = self
            .nets
            .iter()
            .filter(|(name, _)| !is_loopback(name))
            .map(|(name, n)| {
                (name.to_string_lossy().into_owned(), n.received() as f64 / self.sys_secs, n.transmitted() as f64 / self.sys_secs)
            })
            .collect();
        out.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)));
        out
    }

    fn proc_kind(&self) -> ProcessRefreshKind {
        let kind = ProcessRefreshKind::nothing().with_cpu().with_memory().with_disk_usage().without_tasks();
        if self.gpus.is_some() { kind.with_gpu_usage().with_gpu_memory() } else { kind }
    }

    /// One process's metrics (CPU as % of one core; no network), or `None`
    /// once it has exited or the pid now belongs to a different process.
    pub fn process(&mut self, pid: u32, start_time: u64) -> Option<[f64; METRICS]> {
        let pid = Pid::from_u32(pid);
        let kind = self.proc_kind();
        self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
        let p = self.sys.process(pid)?;
        if p.start_time() != start_time {
            return None;
        }
        let first = self.known.insert((pid, start_time));
        Some(process_metrics(p, self.sys_secs, first))
    }

    /// Refreshes every process (not threads) with cpu, memory, disk I/O,
    /// GPU (if available) and owner.
    pub fn refresh_processes(&mut self, with_cmd: bool) {
        let mut kind = self.proc_kind().with_user(UpdateKind::OnlyIfNotSet);
        if with_cmd {
            kind = kind.with_cmd(UpdateKind::OnlyIfNotSet);
        }
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        self.procs_secs = self.last_procs.elapsed().as_secs_f64().max(0.001);
        self.last_procs = Instant::now();
        let now: HashSet<(Pid, u64)> = self.sys.processes().values().map(|p| (p.pid(), p.start_time())).collect();
        self.fresh = now.difference(&self.known).copied().collect();
        self.known = now;
    }

    pub fn processes(&self) -> impl Iterator<Item = &Process> {
        self.sys.processes().values()
    }

    /// A process's metrics as of the last `refresh_processes`.
    pub fn metrics(&self, p: &Process) -> [f64; METRICS] {
        process_metrics(p, self.procs_secs, self.fresh.contains(&(p.pid(), p.start_time())))
    }

    /// The user running pmon (a uid on Unix, a SID on Windows). Needs a prior
    /// `refresh_processes`.
    pub fn current_user(&self) -> Option<Uid> {
        let me = sysinfo::get_current_pid().ok()?;
        self.sys.process(me)?.user_id().cloned()
    }
}

/// `first`: the first time this process is seen, when its disk counters are
/// totals rather than deltas.
fn process_metrics(p: &Process, secs: f64, first: bool) -> [f64; METRICS] {
    let mut v = [0.0; METRICS];
    let io = p.disk_usage();
    v[agg::CPU] = p.cpu_usage() as f64;
    v[agg::MEM] = p.memory() as f64;
    if !first {
        v[agg::DISK_R] = io.read_bytes as f64 / secs;
        v[agg::DISK_W] = io.written_bytes as f64 / secs;
    }
    v[agg::GPU] = p.gpu_usage().unwrap_or(0.0) as f64;
    v[agg::GPU_MEM] = p.gpu_memory().unwrap_or(0) as f64;
    v
}

/// Kills `pid`, but only if it is still the process that started at
/// `start_time` (so a reused pid is never hit). On Unix sends SIGTERM, or
/// SIGKILL with `force`; on Windows always terminates. Waits briefly and
/// returns whether the process has actually exited.
pub fn kill(pid: u32, start_time: u64, force: bool) -> Result<bool, String> {
    if pid == std::process::id() {
        return Err("that's pmon itself".into());
    }
    let mut sys = System::new().map_err(|e| e.to_string())?;
    let p = Pid::from_u32(pid);
    let alive = |sys: &mut System| {
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[p]), true, ProcessRefreshKind::nothing());
        sys.process(p).is_some_and(|pr| pr.start_time() == start_time)
    };
    if !alive(&mut sys) {
        return Err("no longer running".into());
    }
    #[cfg(unix)]
    {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        if unsafe { libc::kill(pid as libc::pid_t, sig) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = force;
        if !sys.process(p).is_some_and(|pr| pr.kill()) {
            return Err("access denied".into());
        }
    }
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if !alive(&mut sys) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn owned_by(p: &Process, user: &Uid) -> bool {
    p.user_id() == Some(user)
}
