//! `pmon --config`: pick display settings with a live preview, then save them.

use crate::agg::Xy;
use crate::app::chart_title;
use crate::chart::TopLine;
use crate::panels::{self, Panel, StatList, Stats, Window};
use crate::settings::{self, Arrangement, MAX_TOP, PanelKind, Settings, parse_layout};
use crate::theme::{Theme, theme};
use crate::util::{fmt_bytes, fmt_bytes_short};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use std::f64::consts::TAU;
use std::time::{Duration, Instant};

const RANGE: Duration = Duration::from_secs(120);
const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * MIB;
const DEMO_TOTAL_MEM: f64 = 16.0 * GIB;
const DEMO_VRAM: f64 = 8.0 * GIB;
const DEMO_PROCS: [&str; MAX_TOP] = ["firefox", "code", "cargo", "rust-analyzer", "chrome", "slack", "node", "dockerd"];
/// Settings by id (the `row` values below).
const ROWS: [&str; 10] = ["Style", "Characters", "Theme", "Top lines", "Show", "Layout", "Stats", "Group apps", "Inline", "Focus"];
/// Display order, with a group label where each group starts.
const ORDER: [(usize, &str); 10] =
    [(4, "Panels"), (5, ""), (9, ""), (6, ""), (3, "Processes"), (7, ""), (0, "Look"), (1, ""), (2, ""), (8, "Window")];
const FOCUS_ROW: usize = 9;
const PANELS_ROW: usize = 4;
const LAYOUT_ROW: usize = 5;
const TEMPLATE_HINT: &str = "rows split by /, e.g. cpu / ram disk / net gpu";
/// Choices for the inline row (`None` = full screen).
const INLINE: [Option<u16>; 6] = [None, Some(16), Some(20), Some(24), Some(30), Some(40)];

/// `note` is shown at the top, e.g. why the existing config couldn't be read.
pub fn run(start: Settings, note: Option<String>) -> Result<i32> {
    let mut st = start.clone();
    let mut row = ORDER[0].0;
    // Highlighted panel within the Panels row.
    let mut panel_cur = 0usize;
    // A layout template from the config (or typed here) stays in the ←→ cycle.
    let mut custom = matches!(start.layout, Arrangement::Rows(_)).then(|| start.layout.clone());
    // Layout template being typed, if any.
    let mut edit: Option<String> = None;
    let began = Instant::now();
    let mut terminal = ratatui::init();
    let saved = loop {
        terminal.draw(|f| draw(f, &st, row, panel_cur, edit.as_deref(), began.elapsed().as_secs_f64(), note.as_deref()))?;
        if !event::poll(Duration::from_millis(200))? {
            continue; // redraw to animate the preview
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        if let Some(text) = &mut edit {
            match k.code {
                KeyCode::Esc => edit = None,
                KeyCode::Enter => {
                    if let Ok(a) = parse_layout(text) {
                        if matches!(a, Arrangement::Rows(_)) {
                            custom = Some(a.clone());
                        }
                        st.layout = a;
                        edit = None;
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
                _ => {}
            }
            continue;
        }
        let change = |st: &mut Settings, by: isize| match row {
            0 => st.style = settings::step(&st.style, by),
            1 => st.charset = settings::step(&st.charset, by),
            2 => st.theme = settings::step(&st.theme, by),
            3 => st.top = (st.top as isize + by).rem_euclid(MAX_TOP as isize + 1) as usize,
            LAYOUT_ROW => st.layout = st.layout.step(custom.as_ref(), by),
            6 => st.stats = !st.stats,
            7 => st.group = !st.group,
            FOCUS_ROW => {
                // none, then each panel.
                let choices: Vec<Option<PanelKind>> = std::iter::once(None).chain(PanelKind::ALL.map(Some)).collect();
                let i = choices.iter().position(|&c| c == st.focus).unwrap_or(0) as isize;
                st.focus = choices[(i + by).rem_euclid(choices.len() as isize) as usize];
            }
            8 => {
                let i = INLINE.iter().position(|&v| v == st.inline).unwrap_or(0) as isize;
                st.inline = INLINE[(i + by).rem_euclid(INLINE.len() as isize) as usize];
            }
            _ => {}
        };
        let toggle_panel = |st: &mut Settings| {
            let kind = PanelKind::ALL[panel_cur];
            match st.panels.iter().position(|&p| p == kind) {
                Some(i) => {
                    st.panels.remove(i);
                }
                None => {
                    st.panels.push(kind);
                    st.panels.sort();
                }
            }
        };
        let n = PanelKind::ALL.len();
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Esc | KeyCode::Char('q') => break None,
            KeyCode::Enter | KeyCode::Char('w') => break Some(st.save()),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => row = step_row(row, -1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => row = step_row(row, 1),
            KeyCode::Left | KeyCode::Char('h') if row == PANELS_ROW => panel_cur = (panel_cur + n - 1) % n,
            KeyCode::Right | KeyCode::Char('l') if row == PANELS_ROW => panel_cur = (panel_cur + 1) % n,
            KeyCode::Char(' ') if row == PANELS_ROW => toggle_panel(&mut st),
            KeyCode::Char('e') if row == LAYOUT_ROW => {
                // Start from the current template, or the grid as a template.
                edit = Some(match &st.layout {
                    Arrangement::Rows(_) => st.layout.text(),
                    Arrangement::Preset(_) => {
                        Arrangement::Rows(st.panels.chunks(2).map(<[_]>::to_vec).collect()).text()
                    }
                });
            }
            KeyCode::Left | KeyCode::Char('h') => change(&mut st, -1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => change(&mut st, 1),
            KeyCode::Char('r') => st = start.clone(),
            _ => {}
        }
    };
    ratatui::restore();
    match saved {
        Some(Ok(path)) => println!("Saved to {}", path.display()),
        Some(Err(e)) => return Err(e),
        None => println!("Not saved."),
    }
    Ok(0)
}

/// The setting `by` places from `row` in display order, wrapping around.
fn step_row(row: usize, by: isize) -> usize {
    let pos = ORDER.iter().position(|&(r, _)| r == row).unwrap_or(0) as isize;
    ORDER[(pos + by).rem_euclid(ORDER.len() as isize) as usize].0
}

fn draw(f: &mut Frame, st: &Settings, row: usize, panel_cur: usize, edit: Option<&str>, t: f64, note: Option<&str>) {
    let area = f.area();
    if area.width < 50 || area.height < 18 {
        f.render_widget(Paragraph::new("terminal too small (need 50x18)"), area);
        return;
    }
    let th = theme(st.theme);
    let [body, keys] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    // Menu beside the preview on wide terminals, above it otherwise.
    let menu_h = ROWS.len() as u16 + 3;
    let (menu, preview) = if body.width >= 120 {
        let [m, p] = Layout::horizontal([Constraint::Length(58), Constraint::Fill(1)]).areas(body);
        (Rect { height: menu_h.min(m.height), ..m }, p)
    } else {
        let [m, p] = Layout::vertical([Constraint::Length(menu_h), Constraint::Fill(1)]).areas(body);
        (m, p)
    };

    draw_menu(f, menu, st, row, panel_cur, edit, &th, note);

    // The preview: the real panel renderer and layout, fed sample data. A
    // template being typed is previewed as soon as it parses.
    let layout = edit.and_then(|t| parse_layout(t).ok()).unwrap_or_else(|| st.layout.clone());
    let kinds = &st.panels;
    let placed = panels::layout(preview, kinds, &layout, st.focus);
    let points = (panels::plot_cols(&placed, st.stats) as usize).clamp(8, RANGE.as_secs() as usize / 2);
    let bucket = RANGE / points as u32;
    let demo = Demo::new(t, points);
    let window = Window { range: RANGE, bucket, gap: f64::INFINITY };
    for &(kind, rect) in &placed {
        let panel = demo.panel(kind, st, &th);
        panels::render(f, rect, &panel, &window, st, &th);
    }
    if kinds.is_empty() {
        f.render_widget(Paragraph::new(" (no panels selected)").style(Style::new().fg(th.dim)), preview);
    }

    let k = Style::new().bold();
    let d = Style::new().fg(th.dim);
    if edit.is_some() {
        let line = Line::from(vec![
            Span::styled(" type the layout  ", d),
            Span::styled("Enter", k),
            Span::styled(" apply  ", d),
            Span::styled("Esc", k),
            Span::styled(" cancel", d),
        ]);
        f.render_widget(Paragraph::new(line), keys);
        return;
    }
    let hint = match (row, edit.is_some()) {
        (PANELS_ROW, _) => " ←→ pick  Space toggle  ",
        (LAYOUT_ROW, _) => " ←→ change  e edit template  ",
        _ => " ←→ change  ",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ↑↓", k),
            Span::styled(" choose ", d),
            Span::styled(hint, d),
            Span::styled("Enter", k),
            Span::styled(" save  ", d),
            Span::styled("r", k),
            Span::styled(" reset  ", d),
            Span::styled("Esc", k),
            Span::styled(" cancel", d),
        ])),
        keys,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_menu(f: &mut Frame, area: Rect, st: &Settings, row: usize, panel_cur: usize, edit: Option<&str>, th: &Theme, note: Option<&str>) {
    let accent = Style::new().fg(th.accent).bold();
    let value = |i: usize| -> String {
        match i {
            0 => settings::name(&st.style),
            1 => settings::name(&st.charset),
            2 => settings::name(&st.theme),
            3 => if st.top == 0 { "off".into() } else { st.top.to_string() },
            LAYOUT_ROW => st.layout.text(),
            6 => (if st.stats { "on" } else { "off" }).into(),
            7 => (if st.group { "on" } else { "off" }).into(),
            8 => st.inline.map_or("off (full screen)".into(), |n| format!("{n} rows")),
            FOCUS_ROW => settings::focus_str(st.focus),
            _ => String::new(),
        }
    };
    let mut lines: Vec<Line> = ORDER
        .iter()
        .map(|&(i, group)| {
            let label = ROWS[i];
            let selected = i == row;
            // Group labels in a gutter: related settings read as one block
            // without spending rows on headings.
            let gutter = Span::styled(format!(" {group:<10}"), Style::new().fg(th.accent));
            let marker = if selected { Span::styled("▸ ", Style::new().bold()) } else { Span::raw("  ") };
            let name = if selected { Span::styled(format!("{label:<11}"), Style::new().bold()) } else { Span::raw(format!("{label:<11}")) };
            let head = Line::from(vec![gutter, marker, name]);
            if i == PANELS_ROW {
                // One toggle per panel; the highlighted one is what Space flips.
                let mut spans = head.spans;
                for (j, kind) in PanelKind::ALL.iter().enumerate() {
                    let on = st.panels.contains(kind);
                    let text = format!("{}{}", if on { "●" } else { "○" }, settings::name(kind));
                    let style = match (selected && j == panel_cur, on) {
                        (true, _) => accent.reversed(),
                        (false, true) => Style::new(),
                        (false, false) => Style::new().fg(th.dim),
                    };
                    spans.push(Span::styled(text, style));
                    spans.push(Span::raw(" "));
                }
                Line::from(spans)
            } else if let (true, LAYOUT_ROW, Some(text)) = (selected, i, edit) {
                head.spans.into_iter().chain([Span::styled(format!("{text}▏"), accent)]).collect()
            } else if selected {
                head.spans.into_iter().chain([Span::styled(format!("◂ {} ▸", value(i)), accent)]).collect()
            } else {
                head.spans.into_iter().chain([Span::raw(format!("  {}", value(i)))]).collect()
            }
        })
        .collect();
    let about = match row {
        0 => settings::help(&st.style),
        1 => settings::help(&st.charset),
        2 => settings::help(&st.theme),
        3 => "busiest processes drawn as extra lines in the system view".into(),
        PANELS_ROW => "Space shows/hides the highlighted panel (GPU hides itself without a GPU)".into(),
        LAYOUT_ROW => match (edit.map(parse_layout), &st.layout) {
            (Some(Err(e)), _) => format!("{e:#}"),
            (Some(Ok(_)), _) => TEMPLATE_HINT.into(),
            (None, Arrangement::Preset(k)) => format!("{} (e: write your own, {TEMPLATE_HINT})", settings::help(k)),
            (None, Arrangement::Rows(_)) => TEMPLATE_HINT.into(),
        },
        6 => "now/avg/peak and top consumers beside each chart, if wide enough".into(),
        7 => "count a browser's (or any app's) many processes as one, in lists and the picker".into(),
        FOCUS_ROW => "in a grid with an odd number of panels, this one gets the full-width top row".into(),
        _ => "draw below the prompt and leave the last frame on screen after quitting".into(),
    };
    lines.push(Line::styled(format!(" {:<10}  {about}", ""), Style::new().fg(th.dim)));
    let mut block = Block::bordered().border_style(Style::new().fg(th.accent)).title(" pmon display settings ".bold());
    if let Some(n) = note {
        block = block.title_bottom(Span::styled(format!(" {n} "), Style::new().fg(th.warn)));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Made-up but plausible traffic that scrolls left over time, with spikes so
/// every style has something to show.
struct Demo {
    xs: Vec<f64>,
    t: f64,
}

fn total_cpu(a: f64) -> f64 {
    let spike = if a.rem_euclid(37.0) < 3.0 { 30.0 } else { 0.0 };
    (32.0 + 14.0 * (a * TAU / 45.0).sin() + 7.0 * (a * TAU / 11.0).sin() + spike).clamp(0.0, 100.0)
}
fn total_mem(a: f64) -> f64 {
    (6.5 + 1.2 * (a * TAU / 90.0).sin() + 0.3 * (a * TAU / 13.0).sin()) * GIB
}
/// Share of a total for fake process `i`, wobbling at its own pace.
fn share(i: usize, a: f64) -> f64 {
    (0.45 / (i + 1) as f64) * (0.75 + 0.25 * (a * TAU / (7.0 + 3.0 * i as f64) + i as f64).sin())
}
fn proc_mem(i: usize, a: f64) -> f64 {
    (1.6 / (i as f64 * 0.8 + 1.0)) * GIB * (1.0 + 0.05 * (a * TAU / (20.0 + 5.0 * i as f64)).sin())
}
/// Bursty traffic: mostly quiet, with periodic transfers.
fn bursts(a: f64, period: f64, peak: f64) -> f64 {
    let phase = a.rem_euclid(period) / period;
    let burst = if phase < 0.2 { (phase / 0.2 * std::f64::consts::PI).sin() } else { 0.0 };
    (0.04 + 0.02 * (a * TAU / 7.0).sin().abs() + burst) * peak
}
fn gpu_use(a: f64) -> f64 {
    (25.0 + 20.0 * (a * TAU / 30.0).sin() + 10.0 * (a * TAU / 6.0).sin().abs()).clamp(0.0, 100.0)
}

impl Demo {
    fn new(t: f64, points: usize) -> Self {
        let range = RANGE.as_secs_f64();
        Demo { xs: (0..points).map(|i| -range + (i + 1) as f64 * range / points as f64).collect(), t }
    }

    fn series(&self, f: impl Fn(f64) -> f64) -> Xy {
        self.xs.iter().map(|&x| (x, f(self.t + x))).collect()
    }

    fn now(&self, f: impl Fn(f64) -> f64) -> f64 {
        f(self.t)
    }

    /// now / avg / peak rows for one series.
    fn rows(&self, xy: &Xy, fmt: impl Fn(f64) -> String) -> Vec<(&'static str, String)> {
        let avg = xy.iter().map(|p| p.1).sum::<f64>() / xy.len().max(1) as f64;
        let max = xy.iter().map(|p| p.1).fold(0.0, f64::max);
        vec![("now", fmt(xy.last().map_or(0.0, |p| p.1))), ("avg", fmt(avg)), ("peak", fmt(max))]
    }

    fn panel(&self, kind: PanelKind, st: &Settings, th: &Theme) -> Panel {
        let top_n = st.top;
        let pct = |v: f64| format!("{v:.1}%");
        let rate = |v: f64| format!("{}/s", fmt_bytes(v));
        let scope = "preview (sample data)";
        // Top lines (when on) and the stats list, from the fake processes.
        let procs = |f: &dyn Fn(usize, f64) -> f64, fmt: &dyn Fn(f64) -> String| -> (Vec<TopLine>, StatList) {
            let lines: Vec<TopLine> = (0..top_n)
                .map(|i| {
                    let pts = self.series(|a| f(i, a));
                    let value = fmt(pts.last().map_or(0.0, |p| p.1));
                    TopLine { label: DEMO_PROCS[i].into(), value, color: th.palette[i % th.palette.len()], pts }
                })
                .collect();
            let list = (0..5)
                .map(|i| (DEMO_PROCS[i].to_string(), fmt(self.now(|a| f(i, a))), (i < top_n).then(|| th.palette[i % th.palette.len()])))
                .collect();
            (lines, list)
        };
        let legend = |top: &[TopLine]| top.iter().map(|t| (format!("{} {}", t.label, t.value), t.color)).collect::<Vec<_>>();
        let stats = |rows, list, title| Some(Stats { rows, list_title: title, list });
        match kind {
            PanelKind::Cpu => {
                let main = self.series(total_cpu);
                let (top, list) = procs(&|i, a| total_cpu(a) * share(i, a), &pct);
                Panel {
                    title: chart_title("CPU", pct(self.now(total_cpu)), th.cpu, scope, None, th),
                    stats: stats(self.rows(&main, pct), list, "top processes"),
                    main,
                    color: th.cpu,
                    extra: vec![],
                    legend: legend(&top),
                    top,
                    y_top: 100.0,
                    y_labels: [0.0, 50.0, 100.0].map(|v| format!("{v:.0}%")),
                }
            }
            PanelKind::Ram => {
                let main = self.series(total_mem);
                let (top, list) = procs(&proc_mem, &fmt_bytes);
                let now = self.now(total_mem);
                let share = format!("of {} ({:.0}%)", fmt_bytes(DEMO_TOTAL_MEM), now / DEMO_TOTAL_MEM * 100.0);
                Panel {
                    title: chart_title("RAM", fmt_bytes(now), th.mem, scope, Some(share), th),
                    stats: stats(self.rows(&main, fmt_bytes), list, "top processes"),
                    main,
                    color: th.mem,
                    extra: vec![],
                    legend: legend(&top),
                    top,
                    y_top: DEMO_TOTAL_MEM,
                    y_labels: [0.0, DEMO_TOTAL_MEM / 2.0, DEMO_TOTAL_MEM].map(fmt_bytes_short),
                }
            }
            PanelKind::Disk => {
                let read = |a: f64| bursts(a, 40.0, 180.0 * MIB);
                let write = |a: f64| bursts(a + 15.0, 25.0, 90.0 * MIB);
                let (_, list) = procs(&|i, a| (read(a) + write(a)) * share(i, a), &rate);
                let y_top = 256.0 * MIB;
                Panel {
                    title: chart_title("Disk", format!("R {}  W {}", rate(self.now(read)), rate(self.now(write))), th.read, scope, None, th),
                    stats: stats(vec![("read", rate(self.now(read))), ("write", rate(self.now(write))), ("peak", rate(270.0 * MIB))], list, "top processes"),
                    main: self.series(read),
                    color: th.read,
                    extra: vec![(self.series(write), th.write)],
                    legend: vec![("read".into(), th.read), ("write".into(), th.write)],
                    top: vec![],
                    y_top,
                    y_labels: [0.0, y_top / 2.0, y_top].map(fmt_bytes_short),
                }
            }
            PanelKind::Net => {
                let rx = |a: f64| bursts(a, 30.0, 40.0 * MIB);
                let tx = |a: f64| bursts(a + 9.0, 45.0, 8.0 * MIB);
                let y_top = 50.0 * MIB;
                let list = [("eth0", 1.0), ("wlan0", 0.2), ("docker0", 0.05)]
                    .iter()
                    .map(|&(n, k)| {
                        (n.to_string(), format!("↓{} ↑{}", fmt_bytes_short(self.now(rx) * k), fmt_bytes_short(self.now(tx) * k)), None)
                    })
                    .collect();
                Panel {
                    title: chart_title("Net", format!("↓ {}  ↑ {}", rate(self.now(rx)), rate(self.now(tx))), th.read, scope, None, th),
                    stats: stats(vec![("recv", rate(self.now(rx))), ("sent", rate(self.now(tx))), ("peak", rate(46.0 * MIB))], list, "interfaces"),
                    main: self.series(rx),
                    color: th.read,
                    extra: vec![(self.series(tx), th.write)],
                    legend: vec![("received".into(), th.read), ("sent".into(), th.write)],
                    top: vec![],
                    y_top,
                    y_labels: [0.0, y_top / 2.0, y_top].map(fmt_bytes_short),
                }
            }
            PanelKind::Gpu => {
                let main = self.series(gpu_use);
                let vram = |a: f64| 3.1 * GIB + 0.4 * GIB * (a * TAU / 50.0).sin();
                let (top, list) = procs(&|i, a| gpu_use(a) * share(i, a), &pct);
                let mut leg = legend(&top);
                if leg.is_empty() {
                    leg = vec![("usage".into(), th.gpu), ("VRAM".into(), th.mem)];
                }
                let vram_now = format!("VRAM {} of {}", fmt_bytes(self.now(vram)), fmt_bytes(DEMO_VRAM));
                Panel {
                    title: chart_title("GPU", pct(self.now(gpu_use)), th.gpu, scope, Some(vram_now), th),
                    stats: stats(self.rows(&main, pct), list, "top processes"),
                    main,
                    color: th.gpu,
                    extra: vec![(self.series(|a| vram(a) / DEMO_VRAM * 100.0), th.mem)],
                    legend: leg,
                    top,
                    y_top: 100.0,
                    y_labels: [0.0, 50.0, 100.0].map(|v| format!("{v:.0}%")),
                }
            }
        }
    }
}
