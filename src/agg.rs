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
pub fn aggregate(pts: &[Point], now_ms: u64, range: Duration, bucket: Duration) -> (Xy, Xy) {
    let start = now_ms.saturating_sub(range.as_millis() as u64);
    let b = (bucket.as_millis() as u64).max(1);
    let first = pts.partition_point(|p| p.ts_ms < start);
    let (mut cpu, mut mem) = (Vec::new(), Vec::new());
    let mut emit = |id: u64, c: f64, m: f64| {
        let x = (((id + 1) * b).min(now_ms) as f64 - now_ms as f64) / 1000.0;
        cpu.push((x, c));
        mem.push((x, m));
    };
    let mut cur: Option<(u64, f64, f64)> = None;
    for p in &pts[first..] {
        let id = p.ts_ms / b;
        cur = match cur {
            Some((cid, c, m)) if cid == id => Some((cid, c.max(p.cpu), m.max(p.mem))),
            prev => {
                if let Some((cid, c, m)) = prev {
                    emit(cid, c, m);
                }
                Some((id, p.cpu, p.mem))
            }
        };
    }
    if let Some((cid, c, m)) = cur {
        emit(cid, c, m);
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
        assert_eq!(cpu, vec![(-5.0, 90.0), (0.0, 5.0)]);
        assert_eq!(mem[0].1, 180.0);
    }
}
