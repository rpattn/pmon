//! Thin wrapper over `sysinfo`.

use anyhow::{Result, anyhow};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

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
}

pub fn owned_by(p: &Process, uid: u32) -> bool {
    p.user_id().is_some_and(|u| **u == uid)
}
