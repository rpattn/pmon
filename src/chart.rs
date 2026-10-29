//! Drawing one chart: the total (and any secondary series), top-process
//! lines or stacked bands, gaps for missing data, and a colour key.

use crate::agg;
use crate::settings::{self, GraphStyle, Settings};
use crate::theme::Theme;
use crate::util::fmt_dur;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType};
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

/// Borders plus the y-axis label column.
pub const CHART_CHROME: u16 = 2 + 7;

/// A top-process line ready to draw.
pub struct TopLine {
    pub label: String,
    pub value: String,
    pub color: Color,
    pub pts: agg::Xy,
}

pub struct ChartSpec<'a> {
    pub title: Line<'static>,
    pub main: &'a [(f64, f64)],
    pub color: Color,
    /// More series drawn as lines over the main one (disk write, network
    /// send, GPU memory).
    pub extra: &'a [(agg::Xy, Color)],
    pub top: &'a [TopLine],
    /// Colour key for the bottom border.
    pub legend: &'a [(String, Color)],
    pub y_top: f64,
    pub y_labels: [String; 3],
    /// A jump between points wider than this (seconds) is missing data, drawn
    /// as a gap rather than a line across it.
    pub gap: f64,
}

/// Running totals of the top-process series at each of the total's points:
/// entry `k` is the sum of processes `0..=k`, capped at the total (RSS
/// double-counts shared memory, and per-bucket maxima needn't add up).
/// Every series comes from the same buckets, so equal x values line up exactly.
fn stack(main: &[(f64, f64)], tops: &[TopLine]) -> Vec<agg::Xy> {
    let by_x: Vec<HashMap<u64, f64>> = tops.iter().map(|t| t.pts.iter().map(|&(x, y)| (x.to_bits(), y)).collect()).collect();
    let mut out = vec![Vec::with_capacity(main.len()); tops.len()];
    for &(x, total) in main {
        let mut cum = 0.0;
        for (k, m) in by_x.iter().enumerate() {
            cum += m.get(&x.to_bits()).copied().unwrap_or(0.0);
            out[k].push((x, cum.min(total)));
        }
    }
    out
}

/// A terminal cell has one colour, so where two bands meet inside a cell the
/// band drawn last takes the whole cell and the other's dots vanish, leaving
/// gaps. This moves every band's top onto a cell boundary, keeping the bands
/// strictly below the cell holding the total's outline. [`band_columns`] then
/// does the same horizontally.
///
/// Ratatui maps value `v` to dot row `round((top - v) * (R - 1) / top)` from
/// the top, with `R = rows * dots`, and a bar fills from there to the bottom;
/// a band whose top is the first dot row of cell `c` therefore fills cells
/// `c..rows` exactly. Empty bands get a negative level.
fn snap_bands(main: &[(f64, f64)], bands: &mut [agg::Xy], y_top: f64, rows: usize, dots: usize) {
    let r = (rows * dots) as f64 - 1.0;
    if r < 1.0 || y_top <= 0.0 {
        return;
    }
    let dot_of = |v: f64| (y_top - v.clamp(0.0, y_top)) * r / y_top;
    // Cell edge nearest to v (edge c = top of cell c; c == rows = the bottom).
    let edge = |v: f64| ((dot_of(v) / dots as f64).round() as usize).min(rows);
    let level = |c: usize| if c >= rows { -1.0 } else { y_top - (c * dots) as f64 * y_top / r };
    for i in 0..main.len() {
        // The total slopes between samples and a cell can straddle a bucket
        // edge, so stay under the lowest of the neighbouring totals.
        let low = main[i.saturating_sub(1)..(i + 2).min(main.len())].iter().map(|p| p.1).fold(f64::MAX, f64::min);
        let first_free = dot_of(low).round() as usize / dots + 1;
        let mut below = rows;
        for band in bands.iter_mut() {
            let c = edge(band[i].1).max(first_free).min(below);
            band[i].1 = level(c);
            below = c;
        }
    }
}

/// Turns a snapped band into one bar per dot column, with every column of a
/// terminal cell at the level of the bucket under the cell's centre, so a
/// step never splits a cell. `cols` is the plot width in cells and `dots` the
/// dot columns per cell; x maps to dot column `round((x + range) * (C - 1) / range)`.
fn band_columns(band: &[(f64, f64)], range: f64, bucket: f64, cols: usize, dots: usize) -> agg::Xy {
    let c = (cols * dots) as f64 - 1.0;
    if c < 1.0 {
        return vec![];
    }
    let x_of = |dot: f64| -range + dot * range / c;
    // The total's fill starts at its first point; don't draw bands before it.
    let start = band.first().map_or(0.0, |p| p.0);
    let mut out = Vec::with_capacity(cols * dots);
    let mut j = 0;
    for cell in 0..cols {
        let centre = x_of((cell * dots) as f64 + (dots - 1) as f64 / 2.0);
        while j < band.len() && band[j].0 < centre {
            j += 1;
        }
        // Points sit at their bucket's end: bucket j covers (x - bucket, x].
        let Some(&(px, level)) = band.get(j) else { break };
        if centre < start || px - bucket >= centre || level < 0.0 {
            continue;
        }
        out.extend((0..dots).map(|d| (x_of((cell * dots + d) as f64), level)));
    }
    out
}

/// For bar charts: one bar per horizontal dot, each taking the value of the
/// bucket it falls in, so bars form a solid histogram instead of sparse spikes.
/// Points sit at their bucket's end, so a bucket covers `(x - bucket, x]`.
fn bars(pts: &[(f64, f64)], range: f64, bucket: f64, slots: usize) -> agg::Xy {
    let mut out = Vec::with_capacity(slots);
    let mut j = 0;
    for i in 0..slots {
        let x = -range + (i as f64 + 0.5) * range / slots as f64;
        while j < pts.len() && pts[j].0 < x {
            j += 1;
        }
        if let Some(&(px, y)) = pts.get(j)
            && px - bucket < x
        {
            out.push((x, y));
        }
    }
    out
}

/// Splits a series where data is missing (the daemon was stopped, the
/// machine slept), so the outage shows as a gap instead of a straight line
/// drawn across it.
fn split_gaps(pts: &[(f64, f64)], gap: f64) -> Vec<&[(f64, f64)]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..pts.len() {
        if pts[i].0 - pts[i - 1].0 > gap {
            out.push(&pts[start..i]);
            start = i;
        }
    }
    if start < pts.len() {
        out.push(&pts[start..]);
    }
    out
}

/// A line needs two points to show at all; draw a lone point (the first
/// bucket of a new series, or a short run between gaps) as a flat segment
/// one bucket wide.
fn widen_lone(pts: &[(f64, f64)], bucket: f64) -> Cow<'_, [(f64, f64)]> {
    match pts {
        [(x, y)] => Cow::Owned(vec![(x - bucket, *y), (*x, *y)]),
        _ => Cow::Borrowed(pts),
    }
}

pub fn render_chart(f: &mut Frame, area: Rect, spec: ChartSpec, range: Duration, bucket: Duration, st: &Settings, th: &Theme) {
    let b = bucket.as_secs_f64();
    let r = range.as_secs_f64();
    let axis_style = Style::new().fg(th.dim);
    let x_labels = [format!("-{}", fmt_dur(range)), format!("-{}", fmt_dur(range / 2)), "now".to_string()];
    // Same label width on both charts keeps their time axes aligned.
    let y_labels = spec.y_labels.map(|l| format!("{l:>5}"));
    let marker = st.charset.marker();
    let dataset = |color: Color, kind: GraphType, pts| {
        Dataset::default().marker(marker).graph_type(kind).style(Style::new().fg(color)).data(pts)
    };
    let segments = |pts| split_gaps(pts, spec.gap).into_iter().map(|seg| widen_lone(seg, b)).collect::<Vec<_>>();

    // The total: bars are resampled per column (and skip columns without
    // data); everything else is drawn per gap-free segment.
    let main_bars;
    let main_segs = if st.style == GraphStyle::Bars {
        use settings::Charset::*;
        let per_cell = if matches!(st.charset, Braille | Octant | Sextant | Quadrant) { 2 } else { 1 };
        let slots = area.width.saturating_sub(CHART_CHROME).max(1) as usize * per_cell;
        main_bars = bars(spec.main, r, b, slots);
        vec![Cow::Borrowed(&main_bars[..])]
    } else {
        segments(spec.main)
    };
    let main = main_segs.iter().map(|seg| dataset(spec.color, st.style.graph_type(), seg));

    // A cell shows only the last dataset drawn into it, so overlaid lines
    // would punch holes in a filled area. In area mode the processes become
    // stacked bands instead: biggest first (at the bottom), the total's colour
    // filling the rest. Bands only fill cells whose bucket has data, so gaps
    // stay empty.
    let mut stacked = if st.style == GraphStyle::Area { stack(spec.main, spec.top) } else { vec![] };
    if !stacked.is_empty() {
        // Plot size in cells: minus borders, x-axis line and labels (height);
        // minus borders, the 5-wide y labels and the y-axis line (width).
        let rows = area.height.saturating_sub(4) as usize;
        let cols = area.width.saturating_sub(8) as usize;
        let (dx, dy) = st.charset.dots_per_cell();
        snap_bands(spec.main, &mut stacked, spec.y_top, rows, dy);
        for band in &mut stacked {
            *band = band_columns(band, r, b, cols, dx);
        }
    }
    let top_segs: Vec<Vec<_>> =
        if st.style == GraphStyle::Area { vec![] } else { spec.top.iter().map(|t| segments(&t.pts)).collect() };
    // Secondary series are always plain lines, drawn over everything else.
    let extra_kind = if st.style == GraphStyle::Area || st.style == GraphStyle::Bars { GraphType::Line } else { st.style.graph_type() };
    let extra_segs: Vec<(Vec<_>, Color)> = spec.extra.iter().map(|(pts, c)| (segments(pts), *c)).collect();
    let extra = extra_segs.iter().flat_map(|(segs, c)| segs.iter().map(move |seg| dataset(*c, extra_kind, seg)));
    let datasets: Vec<Dataset> = if st.style == GraphStyle::Area {
        let bands = spec.top.iter().zip(&stacked).rev().map(|(t, pts)| dataset(t.color, GraphType::Bar, pts));
        main.chain(bands).chain(extra).collect()
    } else {
        let lines = spec.top.iter().zip(&top_segs).flat_map(|(t, segs)| {
            segs.iter().map(|seg| dataset(t.color, st.style.graph_type(), seg)).collect::<Vec<_>>()
        });
        // The total goes last so it stays visible on top of the process lines.
        lines.chain(extra).chain(main).collect()
    };

    let mut block = Block::bordered().border_style(axis_style).title(spec.title);
    if !spec.legend.is_empty() {
        let swatch = if st.charset == settings::Charset::Ascii { "# " } else { "■ " };
        let mut spans = vec![Span::raw(" ")];
        for (text, color) in spec.legend {
            spans.push(Span::styled(swatch, Style::new().fg(*color)));
            spans.push(Span::raw(format!("{text}  ")));
        }
        block = block.title_bottom(Line::from(spans));
    }
    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(Axis::default().style(axis_style).bounds([-r, 0.0]).labels(x_labels))
        .y_axis(Axis::default().style(axis_style).bounds([0.0, spec.y_top]).labels(y_labels));
    f.render_widget(chart, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pts: &[(f64, f64)]) -> TopLine {
        TopLine { label: String::new(), value: String::new(), color: Color::Reset, pts: pts.to_vec() }
    }

    #[test]
    fn stacks_and_caps_at_total() {
        let total = [(-2.0, 50.0), (-1.0, 50.0), (0.0, 50.0)];
        // The second process has no sample in the first bucket.
        let tops = [line(&[(-2.0, 20.0), (-1.0, 30.0), (0.0, 20.0)]), line(&[(-1.0, 30.0), (0.0, 10.0)])];
        let s = stack(&total, &tops);
        assert_eq!(s[0], vec![(-2.0, 20.0), (-1.0, 30.0), (0.0, 20.0)]);
        assert_eq!(s[1], vec![(-2.0, 20.0), (-1.0, 50.0), (0.0, 30.0)]);
    }
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    #[test]
    fn splits_only_on_real_gaps() {
        let pts = [(-10.0, 1.0), (-8.0, 1.0), (-4.0, 1.0), (-2.0, 1.0), (0.0, 1.0)];
        // Bucket 2s: a 4s jump (one missing bucket) is tolerated with a 6s threshold...
        assert_eq!(split_gaps(&pts, 6.0).len(), 1);
        // ...but not with a tighter one.
        let segs = split_gaps(&pts, 3.0);
        assert_eq!(segs, vec![&pts[..2], &pts[2..]]);
        assert!(split_gaps(&[], 3.0).is_empty());
    }
}
