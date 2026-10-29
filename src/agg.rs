//! Time bucketing for the charts.

use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub ts_ms: u64,
    pub cpu: f64,
    pub mem: f64,
}

/// Chart series: `(seconds relative to now, value)`.
pub type Xy = Vec<(f64, f64)>;

const STEPS: &[u64] = &[1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400];

/// Smallest "nice" bucket such that `range` fits in `cols` columns, and never
/// finer than the sample interval.
pub fn auto_bucket(range: Duration, cols: u16, interval: Duration) -> Duration {
    let raw = range.as_secs_f64() / cols.max(1) as f64;
    let secs = STEPS.iter().copied().find(|&s| s as f64 >= raw).unwrap_or(raw.ceil() as u64);
    Duration::from_secs(secs).max(interval)
}

/// Buckets the points that fall within `range` before `now_ms`, taking the
/// **max** of each bucket so short spikes stay visible. Returns `(cpu, mem)`
/// series with x in seconds relative to now (<= 0). Buckets are aligned to
/// wall-clock multiples of `bucket` so they don't jitter as time advances.
///
/// Each bucket is plotted at its end (or now, if still open). A bucket that
/// starts a run (the first, or after missing buckets) also gets a point at
/// its first sample, so a new series begins where its data begins instead of
/// one whole bucket earlier, and grows from there.
pub fn aggregate(pts: &[Point], now_ms: u64, range: Duration, bucket: Duration) -> (Xy, Xy) {
    let start = now_ms.saturating_sub(range.as_millis() as u64);
    let b = (bucket.as_millis() as u64).max(1);
    let first = pts.partition_point(|p| p.ts_ms < start);
    let (mut cpu, mut mem) = (Vec::new(), Vec::new());
    let rel = |ts: u64| (ts.min(now_ms) as f64 - now_ms as f64) / 1000.0;
    let mut last_id: Option<u64> = None;
    let mut emit = |id: u64, first_ts: u64, c: f64, m: f64| {
        let end = rel((id + 1) * b);
        let start = rel(first_ts);
        if last_id.is_none_or(|l| l + 1 != id) && start < end {
            cpu.push((start, c));
            mem.push((start, m));
        }
        cpu.push((end, c));
        mem.push((end, m));
        last_id = Some(id);
    };
    // (bucket id, first sample's ts, max cpu, max mem)
    let mut cur: Option<(u64, u64, f64, f64)> = None;
    for p in &pts[first..] {
        let id = p.ts_ms / b;
        cur = match cur {
            Some((cid, t0, c, m)) if cid == id => Some((cid, t0, c.max(p.cpu), m.max(p.mem))),
            prev => {
                if let Some((cid, t0, c, m)) = prev {
                    emit(cid, t0, c, m);
                }
                Some((id, p.ts_ms, p.cpu, p.mem))
            }
        };
    }
    if let Some((cid, t0, c, m)) = cur {
        emit(cid, t0, c, m);
    }
    (cpu, mem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_fits_width() {
        let b = auto_bucket(Duration::from_secs(600), 70, Duration::from_secs(2));
        assert_eq!(b, Duration::from_secs(10));
        let b = auto_bucket(Duration::from_secs(60), 70, Duration::from_secs(2));
        assert_eq!(b, Duration::from_secs(2));
    }

    #[test]
    fn takes_max_per_bucket() {
        let pts: Vec<Point> = [(1000, 10.0), (2000, 90.0), (3000, 20.0), (6000, 5.0)]
            .iter()
            .map(|&(ts_ms, cpu)| Point { ts_ms, cpu, mem: cpu * 2.0 })
            .collect();
        let (cpu, mem) = aggregate(&pts, 10_000, Duration::from_secs(10), Duration::from_secs(5));
        // The first bucket also gets a point at its first sample (t=1s).
        assert_eq!(cpu, vec![(-9.0, 90.0), (-5.0, 90.0), (0.0, 5.0)]);
        assert_eq!(mem[0].1, 180.0);
    }

    #[test]
    fn open_bucket_spans_from_first_sample_to_now() {
        // One sample 0.3s ago in a 2s bucket that is still open.
        let pts = [Point { ts_ms: 9_700, cpu: 7.0, mem: 1.0 }];
        let (cpu, _) = aggregate(&pts, 10_000, Duration::from_secs(120), Duration::from_secs(2));
        assert_eq!(cpu, vec![(-0.3, 7.0), (0.0, 7.0)]);
    }
}
