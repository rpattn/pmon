//! Thin wrapper over `sysinfo`.

use anyhow::{Result, anyhow};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, Uid, UpdateKind};

pub struct Sampler {
    sys: System,
}

impl Sampler {
    pub fn new() -> Result<Self> {
        let mut sys = System::new().map_err(|e| anyhow!("cannot read system information: {e}"))?;
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        Ok(Sampler { sys })
    }

    pub fn ncpu(&self) -> usize {
        self.sys.cpus().len().max(1)
    }

    /// System-wide `(cpu %, used bytes, total bytes)`.
    pub fn system(&mut self) -> (f32, u64, u64) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        (self.sys.global_cpu_usage(), self.sys.used_memory(), self.sys.total_memory())
    }

    /// `(cpu % of one core, rss bytes)` for one process, or `None` once it has
    /// exited (or the pid now belongs to a different process).
    pub fn process(&mut self, pid: u32, start_time: u64) -> Option<(f32, u64)> {
        let pid = Pid::from_u32(pid);
        let kind = ProcessRefreshKind::nothing().with_cpu().with_memory().without_tasks();
        self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
        let p = self.sys.process(pid)?;
        (p.start_time() == start_time).then(|| (p.cpu_usage(), p.memory()))
    }

    /// Refreshes every process (not threads) with cpu, memory and owner.
    pub fn refresh_processes(&mut self, with_cmd: bool) {
        let mut kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_user(UpdateKind::OnlyIfNotSet)
            .without_tasks();
        if with_cmd {
            kind = kind.with_cmd(UpdateKind::OnlyIfNotSet);
        }
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    }

    pub fn processes(&self) -> impl Iterator<Item = &Process> {
        self.sys.processes().values()
    }

    /// The user running pmon (a uid on Unix, a SID on Windows). Needs a prior
    /// `refresh_processes`.
    pub fn current_user(&self) -> Option<Uid> {
        let me = sysinfo::get_current_pid().ok()?;
        self.sys.process(me)?.user_id().cloned()
    }
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
