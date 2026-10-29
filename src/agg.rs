//! Time bucketing for the charts.

use std::time::Duration;

/// Indices into [`Point::v`].
pub const CPU: usize = 0;
pub const MEM: usize = 1;
pub const DISK_R: usize = 2;
pub const DISK_W: usize = 3;
pub const NET_RX: usize = 4;
pub const NET_TX: usize = 5;
pub const GPU: usize = 6;
pub const GPU_MEM: usize = 7;
pub const METRICS: usize = 8;

/// One sample of every metric: CPU %, memory bytes, disk and network
/// bytes/s, GPU % and GPU memory bytes. Unavailable metrics are 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct Point {
    pub ts_ms: u64,
    pub v: [f64; METRICS],
}

impl Point {
    pub fn new(ts_ms: u64, v: [f64; METRICS]) -> Self {
        Point { ts_ms, v }
    }
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
pub fn aggregate(pts: &[Point], now_ms: u64, range: Duration, bucket: Duration) -> [Xy; METRICS] {
    let start = now_ms.saturating_sub(range.as_millis() as u64);
    let b = (bucket.as_millis() as u64).max(1);
    let first = pts.partition_point(|p| p.ts_ms < start);
    let mut out: [Xy; METRICS] = Default::default();
    let rel = |ts: u64| (ts.min(now_ms) as f64 - now_ms as f64) / 1000.0;
    let mut last_id: Option<u64> = None;
    let mut emit = |id: u64, first_ts: u64, v: &[f64; METRICS]| {
        let end = rel((id + 1) * b);
        let start = rel(first_ts);
        let lead = last_id.is_none_or(|l| l + 1 != id) && start < end;
        for (series, &y) in out.iter_mut().zip(v) {
            if lead {
                series.push((start, y));
            }
            series.push((end, y));
        }
        last_id = Some(id);
    };
    // (bucket id, first sample's ts, per-metric max)
    let mut cur: Option<(u64, u64, [f64; METRICS])> = None;
    for p in &pts[first..] {
        let id = p.ts_ms / b;
        cur = match cur {
            Some((cid, t0, mut v)) if cid == id => {
                for (m, &x) in v.iter_mut().zip(&p.v) {
                    *m = m.max(x);
                }
                Some((cid, t0, v))
            }
            prev => {
                if let Some((cid, t0, v)) = prev {
                    emit(cid, t0, &v);
                }
                Some((id, p.ts_ms, p.v))
            }
        };
    }
    if let Some((cid, t0, v)) = cur {
        emit(cid, t0, &v);
    }
    out
}

/// Mean and maximum of one metric over the points at or after `start_ms`.
pub fn mean_max(pts: &[Point], start_ms: u64, metric: usize) -> Option<(f64, f64)> {
    let pts = &pts[pts.partition_point(|p| p.ts_ms < start_ms)..];
    if pts.is_empty() {
        return None;
    }
    let sum: f64 = pts.iter().map(|p| p.v[metric]).sum();
    let max = pts.iter().map(|p| p.v[metric]).fold(0.0, f64::max);
    Some((sum / pts.len() as f64, max))
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
            .map(|&(ts_ms, cpu)| {
                let mut v = [0.0; METRICS];
                v[CPU] = cpu;
                v[MEM] = cpu * 2.0;
                Point::new(ts_ms, v)
            })
            .collect();
        let out = aggregate(&pts, 10_000, Duration::from_secs(10), Duration::from_secs(5));
        // The first bucket also gets a point at its first sample (t=1s).
        assert_eq!(out[CPU], vec![(-9.0, 90.0), (-5.0, 90.0), (0.0, 5.0)]);
        assert_eq!(out[MEM][0].1, 180.0);
        assert_eq!(mean_max(&pts, 2000, CPU), Some((115.0 / 3.0, 90.0)));
    }

    #[test]
    fn open_bucket_spans_from_first_sample_to_now() {
        // One sample 0.3s ago in a 2s bucket that is still open.
        let mut v = [0.0; METRICS];
        v[CPU] = 7.0;
        let out = aggregate(&[Point::new(9_700, v)], 10_000, Duration::from_secs(120), Duration::from_secs(2));
        assert_eq!(out[CPU], vec![(-0.3, 7.0), (0.0, 7.0)]);
    }
}
