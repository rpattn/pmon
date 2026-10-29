//! Fixed-size ring buffer files written by the daemon and read by `pmon -H`.
//!
//! Layout (all little endian):
//!
//! ```text
//! header (64 bytes)
//!   0  magic        [u8; 8]  "PMONRING"
//!   8  version      u32
//!  12  record_size  u32
//!  16  capacity     u64      number of record slots
//!  24  begin_count  u64      records claimed by the writer (bumped before writing)
//!  32  end_count    u64      records fully written (bumped after writing)
//!  40  interval_ms  u32      daemon sample interval
//! records: capacity * record_size bytes; record `i` lives in slot `i % capacity`
//! ```
//!
//! The two counters let a reader detect slots the writer overwrote while it was
//! reading: anything with an index below `begin_count - capacity` (re-read after
//! the data) is discarded.

use crate::agg::{self, METRICS};
use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

pub const SYS_FILE: &str = "system.ring";
pub const PROC_FILE: &str = "procs.ring";

const MAGIC: &[u8; 8] = b"PMONRING";
const VERSION: u32 = 3;
const HEADER_LEN: u64 = 64;
const OFF_BEGIN: u64 = 24;

/// Positional reads and writes that don't touch a shared file cursor.
#[cfg(unix)]
mod pio {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::FileExt;

    pub fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
        f.read_exact_at(buf, off)
    }
    pub fn write_all_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
        f.write_all_at(buf, off)
    }
}

#[cfg(windows)]
mod pio {
    use std::fs::File;
    use std::io;
    use std::os::windows::fs::FileExt;

    pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match f.seek_read(buf, off)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => {
                    buf = &mut buf[n..];
                    off += n as u64;
                }
            }
        }
        Ok(())
    }
    pub fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match f.seek_write(buf, off)? {
                0 => return Err(io::ErrorKind::WriteZero.into()),
                n => {
                    buf = &buf[n..];
                    off += n as u64;
                }
            }
        }
        Ok(())
    }
}

use pio::{read_exact_at, write_all_at};

/// `%LOCALAPPDATA%\pmon` on Windows.
#[cfg(windows)]
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("pmon")
}

/// `$XDG_STATE_HOME/pmon`, falling back to `~/.local/state/pmon`.
#[cfg(not(windows))]
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
            PathBuf::from(home).join(".local/state")
        });
    base.join("pmon")
}

pub trait Record: Copy {
    const SIZE: usize;
    fn encode(&self, out: &mut [u8]);
    fn decode(b: &[u8]) -> Self;
    fn ts_ms(&self) -> u64;
}

/// One system-wide sample: every metric (see `agg`), plus the totals that
/// scale the memory charts.
#[derive(Clone, Copy, Debug)]
pub struct SysRec {
    pub ts_ms: u64,
    pub v: [f64; METRICS],
    pub mem_total: u64,
    pub gpu_mem_total: u64,
}

/// One sample of one process. Processes are keyed by `(pid, start_time)` so a
/// reused pid never merges with an earlier process. CPU is % of one core
/// (top-style; may exceed 100); there is no per-process network.
#[derive(Clone, Copy, Debug)]
pub struct ProcRec {
    pub ts_ms: u64,
    pub start_time: u64,
    pub pid: u32,
    pub v: [f64; METRICS],
    pub name: [u8; 32],
}

impl ProcRec {
    pub fn name_str(&self) -> String {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        String::from_utf8_lossy(&self.name[..end]).into_owned()
    }

    pub fn set_name(&mut self, name: &[u8]) {
        let n = name.len().min(self.name.len());
        self.name = [0; 32];
        self.name[..n].copy_from_slice(&name[..n]);
    }
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Record for SysRec {
    // ts | cpu f32 | gpu f32 | mem u64 | mem_total u64 | disk r/w, net rx/tx f32 | gpu_mem u64 | gpu_mem_total u64
    const SIZE: usize = 64;
    fn encode(&self, o: &mut [u8]) {
        let v = &self.v;
        o[0..8].copy_from_slice(&self.ts_ms.to_le_bytes());
        o[8..12].copy_from_slice(&(v[agg::CPU] as f32).to_le_bytes());
        o[12..16].copy_from_slice(&(v[agg::GPU] as f32).to_le_bytes());
        o[16..24].copy_from_slice(&(v[agg::MEM] as u64).to_le_bytes());
        o[24..32].copy_from_slice(&self.mem_total.to_le_bytes());
        for (k, m) in [agg::DISK_R, agg::DISK_W, agg::NET_RX, agg::NET_TX].into_iter().enumerate() {
            o[32 + 4 * k..36 + 4 * k].copy_from_slice(&(v[m] as f32).to_le_bytes());
        }
        o[48..56].copy_from_slice(&(v[agg::GPU_MEM] as u64).to_le_bytes());
        o[56..64].copy_from_slice(&self.gpu_mem_total.to_le_bytes());
    }
    fn decode(b: &[u8]) -> Self {
        let mut v = [0.0; METRICS];
        v[agg::CPU] = f32_at(b, 8) as f64;
        v[agg::GPU] = f32_at(b, 12) as f64;
        v[agg::MEM] = u64_at(b, 16) as f64;
        for (k, m) in [agg::DISK_R, agg::DISK_W, agg::NET_RX, agg::NET_TX].into_iter().enumerate() {
            v[m] = f32_at(b, 32 + 4 * k) as f64;
        }
        v[agg::GPU_MEM] = u64_at(b, 48) as f64;
        SysRec { ts_ms: u64_at(b, 0), v, mem_total: u64_at(b, 24), gpu_mem_total: u64_at(b, 56) }
    }
    fn ts_ms(&self) -> u64 {
        self.ts_ms
    }
}

impl Record for ProcRec {
    // ts | start u64 | rss u64 | pid u32 | cpu, disk r/w, gpu f32 | gpu_mem KiB u32 | name [u8; 32]
    const SIZE: usize = 80;
    fn encode(&self, o: &mut [u8]) {
        let v = &self.v;
        o[0..8].copy_from_slice(&self.ts_ms.to_le_bytes());
        o[8..16].copy_from_slice(&self.start_time.to_le_bytes());
        o[16..24].copy_from_slice(&(v[agg::MEM] as u64).to_le_bytes());
        o[24..28].copy_from_slice(&self.pid.to_le_bytes());
        for (k, m) in [agg::CPU, agg::DISK_R, agg::DISK_W, agg::GPU].into_iter().enumerate() {
            o[28 + 4 * k..32 + 4 * k].copy_from_slice(&(v[m] as f32).to_le_bytes());
        }
        o[44..48].copy_from_slice(&((v[agg::GPU_MEM] / 1024.0) as u32).to_le_bytes());
        o[48..80].copy_from_slice(&self.name);
    }
    fn decode(b: &[u8]) -> Self {
        let mut v = [0.0; METRICS];
        v[agg::MEM] = u64_at(b, 16) as f64;
        for (k, m) in [agg::CPU, agg::DISK_R, agg::DISK_W, agg::GPU].into_iter().enumerate() {
            v[m] = f32_at(b, 28 + 4 * k) as f64;
        }
        v[agg::GPU_MEM] = u32_at(b, 44) as f64 * 1024.0;
        ProcRec { ts_ms: u64_at(b, 0), start_time: u64_at(b, 8), pid: u32_at(b, 24), v, name: b[48..80].try_into().unwrap() }
    }
    fn ts_ms(&self) -> u64 {
        self.ts_ms
    }
}

struct Header {
    record_size: u32,
    cap: u64,
    begin: u64,
    end: u64,
    interval_ms: u32,
}

fn read_header(file: &File) -> io::Result<Option<Header>> {
    let mut b = [0u8; HEADER_LEN as usize];
    match read_exact_at(file, &mut b, 0) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        r => r?,
    }
    if &b[0..8] != MAGIC || u32_at(&b, 8) != VERSION {
        return Ok(None);
    }
    Ok(Some(Header {
        record_size: u32_at(&b, 12),
        cap: u64_at(&b, 16),
        begin: u64_at(&b, 24),
        end: u64_at(&b, 32),
        interval_ms: u32_at(&b, 40),
    }))
}

pub struct Ring<R> {
    file: File,
    cap: u64,
    /// Writer only: next record index.
    end: u64,
    pub interval_ms: u32,
    _r: PhantomData<R>,
}

impl<R: Record> Ring<R> {
    /// Opens (or creates) a ring for writing. Existing data is kept when the
    /// layout matches and `keep_cap` accepts the stored capacity; otherwise the
    /// file is recreated with `cap` slots.
    pub fn open_writer(path: &Path, cap: u64, interval_ms: u32, keep_cap: impl Fn(u64) -> bool) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        if let Some(h) = read_header(&file)?
            && h.record_size as usize == R::SIZE
            && h.interval_ms == interval_ms
            && h.begin == h.end
            && keep_cap(h.cap)
            && len == HEADER_LEN + h.cap * R::SIZE as u64
        {
            return Ok(Ring { file, cap: h.cap, end: h.end, interval_ms, _r: PhantomData });
        }
        file.set_len(0)?;
        file.set_len(HEADER_LEN + cap * R::SIZE as u64)?;
        let mut b = [0u8; HEADER_LEN as usize];
        b[0..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&VERSION.to_le_bytes());
        b[12..16].copy_from_slice(&(R::SIZE as u32).to_le_bytes());
        b[16..24].copy_from_slice(&cap.to_le_bytes());
        b[40..44].copy_from_slice(&interval_ms.to_le_bytes());
        write_all_at(&file, &b, 0)?;
        Ok(Ring { file, cap, end: 0, interval_ms, _r: PhantomData })
    }

    /// Opens an existing ring read-only; `None` if it does not exist.
    pub fn open_reader(path: &Path) -> Result<Option<Self>> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
        };
        let Some(h) = read_header(&file)? else {
            bail!("{} is not a pmon history file (or an incompatible version)", path.display())
        };
        if h.record_size as usize != R::SIZE || h.cap == 0 {
            bail!("{} has an unexpected record layout", path.display());
        }
        Ok(Some(Ring { file, cap: h.cap, end: 0, interval_ms: h.interval_ms, _r: PhantomData }))
    }

    pub fn capacity(&self) -> u64 {
        self.cap
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    fn counts(&self) -> io::Result<(u64, u64)> {
        let mut b = [0u8; 16];
        read_exact_at(&self.file, &mut b, OFF_BEGIN)?;
        Ok((u64_at(&b, 0), u64_at(&b, 8)))
    }

    /// Visits slots for record indices `start..start+n`, split at the wrap point.
    fn segments(&self, start: u64, n: u64, mut f: impl FnMut(u64, usize, usize) -> io::Result<()>) -> io::Result<()> {
        let (mut idx, mut done) = (start, 0u64);
        while done < n {
            let slot = idx % self.cap;
            let run = (self.cap - slot).min(n - done);
            let off = HEADER_LEN + slot * R::SIZE as u64;
            f(off, (done as usize) * R::SIZE, (run as usize) * R::SIZE)?;
            idx += run;
            done += run;
        }
        Ok(())
    }

    pub fn append(&mut self, recs: &[R], buf: &mut Vec<u8>) -> io::Result<()> {
        let recs = &recs[recs.len().saturating_sub(self.cap as usize)..];
        if recs.is_empty() {
            return Ok(());
        }
        let start = self.end;
        let new_end = start + recs.len() as u64;
        write_all_at(&self.file, &new_end.to_le_bytes(), OFF_BEGIN)?;
        buf.clear();
        buf.resize(recs.len() * R::SIZE, 0);
        for (r, chunk) in recs.iter().zip(buf.chunks_exact_mut(R::SIZE)) {
            r.encode(chunk);
        }
        self.segments(start, recs.len() as u64, |off, at, len| write_all_at(&self.file, &buf[at..at + len], off))?;
        write_all_at(&self.file, &new_end.to_le_bytes(), OFF_BEGIN + 8)?;
        self.end = new_end;
        Ok(())
    }

    /// Calls `f` for every intact record with index `>= from`, oldest first,
    /// and returns the cursor to pass next time.
    pub fn read_from(&self, from: u64, mut f: impl FnMut(R)) -> io::Result<u64> {
        let (_, end) = self.counts()?;
        let start = from.max(end.saturating_sub(self.cap));
        if start >= end {
            return Ok(end);
        }
        let n = end - start;
        let mut buf = vec![0u8; n as usize * R::SIZE];
        self.segments(start, n, |off, at, len| read_exact_at(&self.file, &mut buf[at..at + len], off))?;
        // Slots the writer may have touched since we read `end`.
        let (begin, _) = self.counts()?;
        let safe = begin.saturating_sub(self.cap);
        for (i, chunk) in buf.chunks_exact(R::SIZE).enumerate() {
            if start + i as u64 >= safe {
                let r = R::decode(chunk);
                if r.ts_ms() != 0 {
                    f(r);
                }
            }
        }
        Ok(end)
    }

    /// Number of records currently stored.
    pub fn stored(&self) -> io::Result<u64> {
        Ok(self.counts()?.1.min(self.cap))
    }

    /// Timestamps of the oldest and newest stored records.
    pub fn span(&self) -> io::Result<Option<(u64, u64)>> {
        let (_, end) = self.counts()?;
        if end == 0 {
            return Ok(None);
        }
        let mut b = vec![0u8; R::SIZE];
        let mut ts_at = |idx: u64| -> io::Result<u64> {
            read_exact_at(&self.file, &mut b, HEADER_LEN + (idx % self.cap) * R::SIZE as u64)?;
            Ok(R::decode(&b).ts_ms())
        };
        // Skip the oldest slot: it is the next one to be overwritten.
        let oldest = end.saturating_sub(self.cap - 1).min(end - 1);
        Ok(Some((ts_at(oldest)?, ts_at(end - 1)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let mut v = [0.0; METRICS];
        v[agg::CPU] = 12.5;
        v[agg::MEM] = 3.0e9;
        v[agg::DISK_R] = 1024.0;
        v[agg::NET_TX] = 2048.0;
        v[agg::GPU] = 40.0;
        v[agg::GPU_MEM] = 8.0 * 1024.0 * 1024.0;
        let s = SysRec { ts_ms: 42, v, mem_total: 16, gpu_mem_total: 8 };
        let mut buf = [0u8; SysRec::SIZE];
        s.encode(&mut buf);
        let d = SysRec::decode(&buf);
        assert_eq!((d.ts_ms, d.v, d.mem_total, d.gpu_mem_total), (42, v, 16, 8));

        v[agg::NET_TX] = 0.0; // not recorded per process
        let mut p = ProcRec { ts_ms: 7, start_time: 9, pid: 3, v, name: [0; 32] };
        p.set_name(b"firefox");
        let mut buf = [0u8; ProcRec::SIZE];
        p.encode(&mut buf);
        let d = ProcRec::decode(&buf);
        assert_eq!((d.ts_ms, d.start_time, d.pid, d.v, d.name_str()), (7, 9, 3, v, "firefox".to_string()));
    }
}
