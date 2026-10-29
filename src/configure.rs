//! `pmon --config`: pick display settings with a live preview, then save them.

use crate::app::{CHART_CHROME, ChartSpec, TopLine, render_chart, title};
use crate::settings::{self, MAX_TOP, Settings};
use crate::theme::theme;
use crate::util::{fmt_bytes, fmt_bytes_short};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use std::f64::consts::TAU;
use std::time::{Duration, Instant};

const RANGE: Duration = Duration::from_secs(120);
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const DEMO_TOTAL_MEM: f64 = 16.0 * GIB;
const DEMO_PROCS: [&str; MAX_TOP] = ["firefox", "code", "cargo", "rust-analyzer", "chrome", "slack", "node", "dockerd"];
const ROWS: [&str; 4] = ["Graph style", "Characters", "Theme", "Top processes"];

/// `note` is shown at the top, e.g. why the existing config couldn't be read.
pub fn run(start: Settings, note: Option<String>) -> Result<i32> {
    let mut st = start.clone();
    let mut row = 0usize;
    let began = Instant::now();
    let mut terminal = ratatui::init();
    let saved = loop {
        terminal.draw(|f| draw(f, &st, row, began.elapsed().as_secs_f64(), note.as_deref()))?;
        if !event::poll(Duration::from_millis(200))? {
            continue; // redraw to animate the preview
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        let change = |st: &mut Settings, by: isize| match row {
            0 => st.style = settings::step(&st.style, by),
            1 => st.charset = settings::step(&st.charset, by),
            2 => st.theme = settings::step(&st.theme, by),
            _ => st.top = (st.top as isize + by).rem_euclid(MAX_TOP as isize + 1) as usize,
        };
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Esc | KeyCode::Char('q') => break None,
            KeyCode::Enter | KeyCode::Char('w') => break Some(st.save()),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => row = (row + ROWS.len() - 1) % ROWS.len(),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => row = (row + 1) % ROWS.len(),
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

fn draw(f: &mut Frame, st: &Settings, row: usize, t: f64, note: Option<&str>) {
    let area = f.area();
    if area.width < 40 || area.height < 16 {
        f.render_widget(Paragraph::new("terminal too small (need 40x16)"), area);
        return;
    }
    let th = theme(st.theme);
    let [menu, cpu_area, mem_area, keys] =
        Layout::vertical([Constraint::Length(7), Constraint::Fill(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);

    // Settings list, the selected row with ◂ ▸ around its value.
    let values = [
        settings::name(&st.style),
        settings::name(&st.charset),
        settings::name(&st.theme),
        if st.top == 0 { "off".into() } else { st.top.to_string() },
    ];
    let mut lines: Vec<Line> = ROWS
        .iter()
        .zip(&values)
        .enumerate()
        .map(|(i, (label, value))| {
            if i == row {
                Line::from(vec![
                    Span::styled(format!(" ▸ {label:<15}"), Style::new().bold()),
                    Span::styled(format!("◂ {value} ▸"), Style::new().fg(th.accent).bold()),
                ])
            } else {
                Line::from(vec![Span::raw(format!("   {label:<15}")), Span::raw(format!("  {value}"))])
            }
        })
        .collect();
    let about = match row {
        0 => settings::help(&st.style),
        1 => settings::help(&st.charset),
        2 => settings::help(&st.theme),
        _ => "busiest processes drawn as extra lines in the system view".into(),
    };
    lines.push(Line::styled(format!("   {about}"), Style::new().fg(th.dim)));
    let mut block = Block::bordered().border_style(Style::new().fg(th.accent)).title(" pmon display settings ".bold());
    if let Some(n) = note {
        block = block.title_bottom(Span::styled(format!(" {n} "), Style::new().fg(th.warn)));
    }
    f.render_widget(Paragraph::new(lines).block(block), menu);

    // Preview with the same renderer as the real charts.
    // About one sample per column, at most one per 2s like the daemon.
    let cols = cpu_area.width.saturating_sub(CHART_CHROME).max(1) as usize;
    let points = cols.min(RANGE.as_secs() as usize / 2);
    let bucket = RANGE / points as u32;
    let demo = Demo::new(t, points, st.top, &th.palette);
    render_chart(
        f,
        cpu_area,
        ChartSpec {
            title: title("CPU", format!("{:.1}%", demo.cpu_now), th.cpu, "preview (sample data)", None, &th),
            main: &demo.cpu,
            color: th.cpu,
            top: &demo.cpu_top,
            y_top: 100.0,
            y_labels: [0.0, 50.0, 100.0].map(|v| format!("{v:.0}%")),
            gap: f64::INFINITY,
        },
        RANGE,
        bucket,
        st,
        &th,
    );
    let pct = demo.mem_now / DEMO_TOTAL_MEM * 100.0;
    render_chart(
        f,
        mem_area,
        ChartSpec {
            title: title(
                "RAM",
                fmt_bytes(demo.mem_now),
                th.mem,
                "preview (sample data)",
                Some(format!("of {} ({pct:.0}%)", fmt_bytes(DEMO_TOTAL_MEM))),
                &th,
            ),
            main: &demo.mem,
            color: th.mem,
            top: &demo.mem_top,
            y_top: DEMO_TOTAL_MEM,
            y_labels: [0.0, DEMO_TOTAL_MEM / 2.0, DEMO_TOTAL_MEM].map(fmt_bytes_short),
            gap: f64::INFINITY,
        },
        RANGE,
        bucket,
        st,
        &th,
    );

    let k = Style::new().bold();
    let d = Style::new().fg(th.dim);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ↑↓", k),
            Span::styled(" choose  ", d),
            Span::styled("←→", k),
            Span::styled(" change  ", d),
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

/// Made-up but plausible traffic that scrolls left over time, with spikes so
/// every style has something to show.
struct Demo {
    cpu: Vec<(f64, f64)>,
    mem: Vec<(f64, f64)>,
    cpu_top: Vec<TopLine>,
    mem_top: Vec<TopLine>,
    cpu_now: f64,
    mem_now: f64,
}

impl Demo {
    fn new(t: f64, points: usize, top: usize, palette: &[ratatui::style::Color]) -> Self {
        let range = RANGE.as_secs_f64();
        let xs: Vec<f64> = (0..points).map(|i| -range + (i + 1) as f64 * range / points as f64).collect();
        let total_cpu = |a: f64| {
            let spike = if a.rem_euclid(37.0) < 3.0 { 30.0 } else { 0.0 };
            (32.0 + 14.0 * (a * TAU / 45.0).sin() + 7.0 * (a * TAU / 11.0).sin() + spike).clamp(0.0, 100.0)
        };
        let total_mem = |a: f64| (6.5 + 1.2 * (a * TAU / 90.0).sin() + 0.3 * (a * TAU / 13.0).sin()) * GIB;
        // Share of the total for each fake process, wobbling at its own pace.
        let cpu_share = |i: usize, a: f64| (0.45 / (i + 1) as f64) * (0.75 + 0.25 * (a * TAU / (7.0 + 3.0 * i as f64) + i as f64).sin());
        let mem_of = |i: usize, a: f64| (1.6 / (i as f64 * 0.8 + 1.0)) * GIB * (1.0 + 0.05 * (a * TAU / (20.0 + 5.0 * i as f64)).sin());

        let series = |f: &dyn Fn(f64) -> f64| xs.iter().map(|&x| (x, f(t + x))).collect::<Vec<_>>();
        let line = |i: usize, pts: Vec<(f64, f64)>, value: String| TopLine {
            label: DEMO_PROCS[i].into(),
            value,
            color: palette[i % palette.len()],
            pts,
        };
        let cpu_top = (0..top)
            .map(|i| {
                let pts = series(&|a| total_cpu(a) * cpu_share(i, a));
                let v = format!("{:.1}%", pts.last().map_or(0.0, |p| p.1));
                line(i, pts, v)
            })
            .collect();
        let mem_top = (0..top)
            .map(|i| {
                let pts = series(&|a| mem_of(i, a));
                let v = fmt_bytes(pts.last().map_or(0.0, |p| p.1));
                line(i, pts, v)
            })
            .collect();
        Demo {
            cpu: series(&total_cpu),
            mem: series(&total_mem),
            cpu_top,
            mem_top,
            cpu_now: total_cpu(t),
            mem_now: total_mem(t),
        }
    }
}
