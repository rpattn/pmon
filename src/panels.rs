//! Arranging panels on screen and drawing each one: its chart plus an
//! optional stats box. Used by the monitor and by the `--config` preview.

use crate::agg::Xy;
use crate::chart::{self, CHART_CHROME, ChartSpec, TopLine};
use crate::settings::{Arrangement, LayoutKind, PanelKind, Settings};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use std::time::Duration;

/// Width of the stats box, and the narrowest panel that gets one.
pub const STATS_WIDTH: u16 = 26;
const STATS_MIN_PANEL: u16 = 64;
/// Smallest panel that can hold a chart (borders, a few plot rows, axis).
const MIN_CHART_HEIGHT: u16 = 6;
const MIN_CHART_WIDTH: u16 = 24;

/// Everything needed to draw one panel.
pub struct Panel {
    pub title: Line<'static>,
    pub main: Xy,
    pub color: Color,
    pub extra: Vec<(Xy, Color)>,
    pub top: Vec<TopLine>,
    pub legend: Vec<(String, Color)>,
    pub y_top: f64,
    pub y_labels: [String; 3],
    pub stats: Option<Stats>,
}

/// A ranked list: `(name, value, swatch colour)`; the colour matches the
/// item's line in the chart, if it has one.
pub type StatList = Vec<(String, String, Option<Color>)>;

/// The time window a panel shows.
pub struct Window {
    pub range: Duration,
    pub bucket: Duration,
    /// A jump between points wider than this (seconds) is a gap in the data.
    pub gap: f64,
}

/// The stats box: a few `label value` rows, then a ranked list.
pub struct Stats {
    pub rows: Vec<(&'static str, String)>,
    pub list_title: &'static str,
    pub list: StatList,
}

/// Places the visible `kinds` in `area`, returning each with its rectangle.
/// In a grid with an odd number of panels, `focus` (if visible) gets the
/// full-width top row; otherwise the last panel spans the bottom row.
pub fn layout(area: Rect, kinds: &[PanelKind], arr: &Arrangement, focus: Option<PanelKind>) -> Vec<(PanelKind, Rect)> {
    let n = kinds.len();
    let rows: Vec<Vec<PanelKind>> = match arr {
        // A template: drop hidden panels (row-mates widen), drop empty rows,
        // and add visible panels the template leaves out as a grid at the end.
        Arrangement::Rows(template) => {
            let mut rows: Vec<Vec<PanelKind>> = template
                .iter()
                .map(|r| r.iter().copied().filter(|p| kinds.contains(p)).collect::<Vec<_>>())
                .filter(|r| !r.is_empty())
                .collect();
            let rest: Vec<PanelKind> = kinds.iter().copied().filter(|p| !template.iter().flatten().any(|q| q == p)).collect();
            rows.extend(rest.chunks(2).map(<[_]>::to_vec));
            rows
        }
        Arrangement::Preset(kind) => match kind {
            LayoutKind::Columns => vec![kinds.to_vec()],
            LayoutKind::Rows => kinds.iter().map(|&k| vec![k]).collect(),
            // Stack while each panel gets a usable height, else a grid.
            LayoutKind::Auto if n <= 2 || area.height as usize >= n * 9 => kinds.iter().map(|&k| vec![k]).collect(),
            LayoutKind::Auto | LayoutKind::Grid => match focus.filter(|f| n % 2 == 1 && kinds.contains(f)) {
                Some(f) => {
                    let rest: Vec<PanelKind> = kinds.iter().copied().filter(|&k| k != f).collect();
                    std::iter::once(vec![f]).chain(rest.chunks(2).map(<[_]>::to_vec)).collect()
                }
                // Two columns; an odd panel out takes the whole last row.
                None => kinds.chunks(2).map(<[_]>::to_vec).collect(),
            },
        },
    };
    let row_rects = Layout::vertical(vec![Constraint::Fill(1); rows.len()]).split(area);
    rows.iter()
        .zip(row_rects.iter())
        .flat_map(|(row, &rect)| {
            let cells = Layout::horizontal(vec![Constraint::Fill(1); row.len()]).split(rect);
            row.iter().copied().zip(cells.iter().copied()).collect::<Vec<_>>()
        })
        .collect()
}

/// The part of a panel's rectangle the chart gets (the rest is stats).
pub fn chart_rect(rect: Rect, stats: bool) -> Rect {
    if stats && rect.width >= STATS_MIN_PANEL {
        Rect { width: rect.width - STATS_WIDTH, ..rect }
    } else {
        rect
    }
}

/// Plot columns of the narrowest chart, for choosing the bucket size.
pub fn plot_cols(placed: &[(PanelKind, Rect)], stats: bool) -> u16 {
    placed.iter().map(|&(_, r)| chart_rect(r, stats).width.saturating_sub(CHART_CHROME)).min().unwrap_or(1).max(1)
}

pub fn render(f: &mut Frame, rect: Rect, p: &Panel, w: &Window, st: &Settings, th: &Theme) {
    let chart_area = chart_rect(rect, st.stats && p.stats.is_some());
    if chart_area.height < MIN_CHART_HEIGHT || chart_area.width < MIN_CHART_WIDTH {
        // Too small for a chart: just the title, so the value is still visible.
        f.render_widget(Block::bordered().border_style(Style::new().fg(th.dim)).title(p.title.clone()), rect);
        return;
    }
    let spec = ChartSpec {
        title: p.title.clone(),
        main: &p.main,
        color: p.color,
        extra: &p.extra,
        top: &p.top,
        legend: &p.legend,
        y_top: p.y_top,
        y_labels: p.y_labels.clone(),
        gap: w.gap,
    };
    chart::render_chart(f, chart_area, spec, w.range, w.bucket, st, th);
    if let Some(stats) = p.stats.as_ref().filter(|_| chart_area.width < rect.width) {
        let area = Rect { x: chart_area.right(), width: rect.width - chart_area.width, ..rect };
        render_stats(f, area, stats, th);
    }
}

fn render_stats(f: &mut Frame, area: Rect, s: &Stats, th: &Theme) {
    let dim = Style::new().fg(th.dim);
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = s
        .rows
        .iter()
        .map(|(label, value)| {
            let pad = inner_w.saturating_sub(label.len() + 1 + value.chars().count());
            Line::from(vec![Span::styled(format!(" {label}"), dim), Span::raw(format!("{}{value}", " ".repeat(pad)))])
        })
        .collect();
    if !s.list.is_empty() {
        lines.push(Line::styled(format!(" {}", s.list_title), dim));
        for (name, value, color) in &s.list {
            let swatch = Span::styled(" ■ ", Style::new().fg(color.unwrap_or(th.dim)));
            let room = inner_w.saturating_sub(3 + 1 + value.chars().count());
            let name: String = name.chars().take(room).collect();
            let pad = room.saturating_sub(name.chars().count());
            lines.push(Line::from(vec![swatch, Span::raw(format!("{name}{} {value}", " ".repeat(pad)))]));
        }
    }
    let block = Block::bordered().border_style(dim).title(" stats ".bold());
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::parse_layout;

    use PanelKind::*;

    fn widths(area: Rect, kinds: &[PanelKind], arr: &Arrangement) -> Vec<(PanelKind, u16)> {
        layout(area, kinds, arr, None).into_iter().map(|(k, r)| (k, r.width)).collect()
    }

    #[test]
    fn focus_takes_the_top_row_of_an_odd_grid() {
        let grid = Arrangement::Preset(LayoutKind::Grid);
        let area = Rect::new(0, 0, 80, 24);
        let placed = |kinds: &[PanelKind], focus| {
            layout(area, kinds, &grid, focus).into_iter().map(|(k, r)| (k, r.y, r.width)).collect::<Vec<_>>()
        };
        let all = [Cpu, Ram, Disk, Net, Gpu];
        assert_eq!(placed(&all, Some(Net))[0], (Net, 0, 80));
        assert_eq!(placed(&all, Some(Net))[1..].iter().map(|p| p.2).collect::<Vec<_>>(), [40, 40, 40, 40]);
        // Even counts, or a hidden focus panel: plain grid.
        assert!(placed(&[Cpu, Ram, Disk, Net], Some(Cpu)).iter().all(|p| p.2 == 40));
        assert_eq!(placed(&[Cpu, Ram, Disk], Some(Gpu))[2], (Disk, 12, 80));
    }

    #[test]
    fn presets() {
        let all = [Cpu, Ram, Disk, Net, Gpu];
        let small = Rect::new(0, 0, 80, 24);
        let grid = Arrangement::Preset(LayoutKind::Grid);
        assert_eq!(widths(small, &all, &grid), vec![(Cpu, 40), (Ram, 40), (Disk, 40), (Net, 40), (Gpu, 80)]);
        // Auto stacks when every panel gets 9+ rows, otherwise uses the grid.
        let auto = Arrangement::Preset(LayoutKind::Auto);
        assert_eq!(widths(Rect::new(0, 0, 80, 50), &all, &auto)[0], (Cpu, 80));
        assert_eq!(widths(small, &all, &auto)[0], (Cpu, 40));
    }

    #[test]
    fn templates_drop_hidden_panels_and_append_missing_ones() {
        let area = Rect::new(0, 0, 80, 30);
        let t = parse_layout("cpu / ram disk / net gpu").unwrap();
        assert_eq!(widths(area, &[Cpu, Ram, Disk, Net, Gpu], &t), vec![(Cpu, 80), (Ram, 40), (Disk, 40), (Net, 40), (Gpu, 40)]);
        // No GPU: net takes the whole last row.
        assert_eq!(widths(area, &[Cpu, Ram, Disk, Net], &t)[3], (Net, 80));
        // Panels not in the template still appear, after it.
        let t = parse_layout("cpu / ram").unwrap();
        assert_eq!(widths(area, &[Cpu, Ram, Disk, Net], &t), vec![(Cpu, 80), (Ram, 80), (Disk, 40), (Net, 40)]);
    }
}
