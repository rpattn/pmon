use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

fn scale(b: f64) -> (f64, usize) {
    let (mut v, mut i) = (b.max(0.0), 0);
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    (v, i)
}

/// "512 B", "5.20 GiB", "15.6 GiB", "312 MiB".
pub fn fmt_bytes(b: f64) -> String {
    let (v, i) = scale(b);
    match (i, v) {
        (0, _) => format!("{v:.0} B"),
        (_, v) if v < 10.0 => format!("{v:.2} {}", UNITS[i]),
        (_, v) if v < 100.0 => format!("{v:.1} {}", UNITS[i]),
        _ => format!("{v:.0} {}", UNITS[i]),
    }
}

/// Compact axis label: "0", "512M", "15.6G".
pub fn fmt_bytes_short(b: f64) -> String {
    if b <= 0.0 {
        return "0".into();
    }
    let (v, i) = scale(b);
    let u = &UNITS[i][..1];
    if v < 100.0 && v.fract() > 0.05 { format!("{v:.1}{u}") } else { format!("{v:.0}{u}") }
}

/// Rounds up to 1, 2, 2.5, 5 or 10 times a power of ten within the byte unit.
pub fn nice_ceil_bytes(b: f64) -> f64 {
    if b <= 0.0 {
        return 1024.0 * 1024.0;
    }
    let (v, i) = scale(b);
    let mag = 10f64.powf(v.log10().floor());
    let nice = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|&n| n >= v - 1e-9).unwrap_or(10.0 * mag);
    nice * 1024f64.powi(i as i32)
}

/// "45s", "2m", "2m30s", "1h", "1h30m", "2d3h".
pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    let (big, small, bu, su) = match s {
        0..60 => return format!("{s}s"),
        60..3600 => (s / 60, s % 60, "m", "s"),
        3600..86400 => (s / 3600, s % 3600 / 60, "h", "m"),
        _ => (s / 86400, s % 86400 / 3600, "d", "h"),
    };
    if small == 0 { format!("{big}{bu}") } else { format!("{big}{bu}{small}{su}") }
}
