//! The TUI: two stacked charts plus a status line.

use crate::agg::{self, Point};
use crate::picker::{self, Action, PickItem, Picker, ProcKey};
use crate::sampler::Sampler;
use crate::store::{self, PROC_FILE, ProcRec, Ring, SYS_FILE, SysRec};
use crate::util::{fmt_bytes, fmt_bytes_short, fmt_dur, nice_ceil_bytes, now_ms};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const LIVE_INTERVAL: Duration = Duration::from_secs(1);
const LIVE_WINDOW: Duration = Duration::from_secs(120);
const HISTORY_WINDOW: Duration = Duration::from_secs(600);
const MIN_WINDOW: Duration = Duration::from_secs(30);
/// Borders plus the y-axis label column.
const CHART_CHROME: u16 = 2 + 7;

const CPU_COLOR: Color = Color::Cyan;
const MEM_COLOR: Color = Color::LightMagenta;

pub struct Options {
    pub history: bool,
    pub process: Option<Option<String>>,
    pub since: Option<Duration>,
    pub agg: Option<Duration>,
}

#[derive(Default)]
struct Series {
    pts: Vec<Point>,
}

impl Series {
    fn push(&mut self, p: Point) {
        if self.pts.last().is_none_or(|l| l.ts_ms <= p.ts_ms) {
            self.pts.push(p);
        }
    }
    fn last(&self) -> Option<&Point> {
        self.pts.last()
    }
    fn trim_before(&mut self, ts_ms: u64) {
        let n = self.pts.partition_point(|p| p.ts_ms < ts_ms);
        if n > 0 {
            self.pts.drain(..n);
        }
    }
}

struct ProcView {
    key: ProcKey,
    series: Series,
    gone: bool,
}

enum Source {
    Live(Box<Sampler>),
    History { sys: Ring<SysRec>, procs: Option<Ring<ProcRec>>, sys_cur: u64, proc_cur: u64 },
}

struct App {
    source: Source,
    interval: Duration,
    window: Duration,
    agg: Option<Duration>,
    ncpu: usize,
    mem_total: u64,
    system: Series,
    proc: Option<ProcView>,
    picker: Option<Picker>,
    note: Option<String>,
    quit: bool,
}

pub fn run(opts: Options) -> Result<i32> {
    let (source, interval, ncpu) = if opts.history {
        let dir = store::state_dir();
        let sys = match Ring::<SysRec>::open_reader(&dir.join(SYS_FILE))? {
            Some(r) if r.stored()? > 0 => r,
            _ => {
                eprintln!("pmon: no history found in {}", dir.display());
                eprintln!("Start the background sampler first with `pmon -d`, then try again in a few seconds.");
                return Ok(1);
            }
        };
        let procs = Ring::<ProcRec>::open_reader(&dir.join(PROC_FILE))?;
        let interval = Duration::from_millis(sys.interval_ms.max(1) as u64);
        let ncpu = std::thread::available_parallelism().map_or(1, |n| n.get());
        (Source::History { sys, procs, sys_cur: 0, proc_cur: 0 }, interval, ncpu)
    } else {
        let s = Sampler::new()?;
        let ncpu = s.ncpu();
        (Source::Live(Box::new(s)), LIVE_INTERVAL, ncpu)
    };
    let default_window = if opts.history { HISTORY_WINDOW } else { LIVE_WINDOW };
    let mut app = App {
        source,
        interval,
        window: opts.since.unwrap_or(default_window).max(interval * 2),
        agg: opts.agg,
        ncpu,
        mem_total: 0,
        system: Series::default(),
        proc: None,
        picker: None,
        note: None,
        quit: false,
    };
    if opts.history {
        app.tick();
    }
    if let Some(query) = opts.process {
        let items = app.pick_items();
        match query.as_deref().and_then(|q| picker::resolve(&items, q)) {
            Some(i) => {
                let key = items[i].key.clone();
                app.select(key);
            }
            None => app.picker = Some(Picker::new(items, query.unwrap_or_default(), app.info_label())),
        }
    }

    let mut terminal = ratatui::init();
    let res = app.event_loop(&mut terminal);
    ratatui::restore();
    res.map(|_| 0)
}

impl App {
    fn is_history(&self) -> bool {
        matches!(self.source, Source::History { .. })
    }

    fn info_label(&self) -> &'static str {
        if self.is_history() { "SEEN" } else { "COMMAND" }
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // First live sample shortly after start so the screen isn't empty for long.
        let mut next_tick = Instant::now() + if self.is_history() { self.interval.min(LIVE_INTERVAL) } else { Duration::from_millis(300) };
        while !self.quit {
            let now = Instant::now();
            if now >= next_tick {
                self.tick();
                // History mode just polls the daemon's file; once a second is plenty.
                next_tick = now + self.interval.min(LIVE_INTERVAL);
            }
            terminal.draw(|f| self.draw(f))?;
            if event::poll(next_tick.saturating_duration_since(Instant::now()))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
                    _ => {} // resize etc: just redraw
                }
            }
        }
        Ok(())
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(p) = &mut self.picker {
            match p.on_key(k) {
                Action::None => {}
                Action::Cancel => self.picker = None,
                Action::Select(key) => {
                    self.picker = None;
                    self.select(key);
                }
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('/') => {
                let items = self.pick_items();
                self.picker = Some(Picker::new(items, String::new(), self.info_label()));
            }
            KeyCode::Esc => self.proc = None,
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.note = None;
        match &mut self.source {
            Source::Live(s) => {
                let ts_ms = now_ms();
                let (cpu, used, total) = s.system();
                self.mem_total = total;
                self.system.push(Point { ts_ms, cpu: cpu as f64, mem: used as f64 });
                if let Some(pv) = self.proc.as_mut().filter(|pv| !pv.gone) {
                    match s.process(pv.key.pid, pv.key.start_time) {
                        Some((cpu, rss)) => pv.series.push(Point { ts_ms, cpu: cpu as f64, mem: rss as f64 }),
                        None => pv.gone = true,
                    }
                }
            }
            Source::History { sys, procs, sys_cur, proc_cur } => {
                let (system, mem_total) = (&mut self.system, &mut self.mem_total);
                match sys.read_from(*sys_cur, |r| {
                    *mem_total = r.mem_total;
                    system.push(Point { ts_ms: r.ts_ms, cpu: r.cpu as f64, mem: r.mem_used as f64 });
                }) {
                    Ok(c) => *sys_cur = c,
                    Err(e) => self.note = Some(format!("read error: {e}")),
                }
                if let (Some(pv), Some(pr)) = (self.proc.as_mut(), procs.as_ref()) {
                    match read_proc(pr, *proc_cur, &pv.key, &mut pv.series) {
                        Ok(c) => *proc_cur = c,
                        Err(e) => self.note = Some(format!("read error: {e}")),
                    }
                }
                // Drop what the daemon has already overwritten.
                let retention = sys.interval_ms as u64 * sys.capacity();
                let newest = self.system.last().map_or(0, |p| p.ts_ms);
                self.system.trim_before(newest.saturating_sub(retention));
                if let Some(pv) = &mut self.proc {
                    let last = pv.series.last().map_or(0, |p| p.ts_ms);
                    pv.gone = newest > last + 3 * sys.interval_ms as u64;
                }
                let stale = now_ms().saturating_sub(newest);
                if stale > 5 * sys.interval_ms as u64 {
                    self.note = Some(format!("no new samples for {} — daemon stopped?", fmt_dur(Duration::from_millis(stale))));
                }
            }
        }
        // Keep a little more than the window (live mode would otherwise grow forever).
        let keep = (self.window + self.window / 4 + Duration::from_secs(60)).as_millis() as u64;
        let cutoff = now_ms().saturating_sub(keep);
        self.system.trim_before(cutoff);
        if let Some(pv) = &mut self.proc {
            pv.series.trim_before(cutoff);
        }
    }

    fn select(&mut self, key: ProcKey) {
        let mut pv = ProcView { key, series: Series::default(), gone: false };
        if let Source::History { procs: Some(pr), proc_cur, .. } = &mut self.source {
            match read_proc(pr, 0, &pv.key, &mut pv.series) {
                Ok(c) => *proc_cur = c,
                Err(e) => self.note = Some(format!("read error: {e}")),
            }
        }
        self.proc = Some(pv);
    }

    fn pick_items(&mut self) -> Vec<PickItem> {
        match &mut self.source {
            Source::Live(s) => {
                s.refresh_processes(true);
                let mut items: Vec<PickItem> = s
                    .processes()
                    .map(|p| {
                        let key = ProcKey {
                            pid: p.pid().as_u32(),
                            start_time: p.start_time(),
                            name: p.name().to_string_lossy().into_owned(),
                        };
                        let cmd = p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
                        PickItem::new(key, p.cpu_usage(), p.memory(), cmd, true)
                    })
                    .collect();
                items.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
                items
            }
            Source::History { procs: Some(pr), .. } => {
                let mut latest: HashMap<(u32, u64), ProcRec> = HashMap::new();
                let mut newest = 0;
                let res = pr.read_from(0, |r| {
                    newest = newest.max(r.ts_ms);
                    latest.insert((r.pid, r.start_time), r);
                });
                if let Err(e) = res {
                    self.note = Some(format!("read error: {e}"));
                }
                let now = now_ms();
                let live_after = newest.saturating_sub(3 * pr.interval_ms as u64);
                let mut recs: Vec<ProcRec> = latest.into_values().collect();
                // Running first (busiest first), then exited (most recently seen first).
                recs.sort_by(|a, b| {
                    let (a_live, b_live) = (a.ts_ms >= live_after, b.ts_ms >= live_after);
                    b_live.cmp(&a_live).then(if a_live { b.cpu.total_cmp(&a.cpu) } else { b.ts_ms.cmp(&a.ts_ms) })
                });
                recs.into_iter()
                    .map(|r| {
                        let alive = r.ts_ms >= live_after;
                        let seen = if alive {
                            "running".to_string()
                        } else {
                            format!("exited, last seen {} ago", fmt_dur(Duration::from_millis(now.saturating_sub(r.ts_ms))))
                        };
                        let key = ProcKey { pid: r.pid, start_time: r.start_time, name: r.name_str() };
                        PickItem::new(key, r.cpu, r.rss, seen, alive)
                    })
                    .collect()
            }
            Source::History { procs: None, .. } => Vec::new(),
        }
    }

    /// Effective time range and whether it was capped by stored history.
    fn range(&self, now: u64) -> (Duration, bool) {
        if !self.is_history() {
            return (self.window, false);
        }
        let first = self.system.pts.first().map_or(now, |p| p.ts_ms);
        let stored = Duration::from_millis(now.saturating_sub(first));
        if stored + self.interval < self.window {
            (stored.max(MIN_WINDOW).min(self.window), true)
        } else {
            (self.window, false)
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < 30 || area.height < 8 {
            f.render_widget(Paragraph::new("terminal too small"), area);
            return;
        }
        let [cpu_area, mem_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        let now = now_ms();
        let (range, capped) = self.range(now);
        let cols = cpu_area.width.saturating_sub(CHART_CHROME).max(1);
        let bucket = self.agg.unwrap_or_else(|| agg::auto_bucket(range, cols, self.interval)).max(self.interval);

        let (series, scope) = match &self.proc {
            Some(pv) => (&pv.series, format!("{} [{}]{}", pv.key.name, pv.key.pid, if pv.gone { " (exited)" } else { "" })),
            None => (&self.system, "system".to_string()),
        };
        let (cpu_pts, mem_pts) = agg::aggregate(&series.pts, now, range, bucket);
        let last = series.last().copied();
        let is_proc = self.proc.is_some();

        // CPU: system is 0–100% of the whole machine; a process is top-style
        // (100% = one core) on an axis that grows in 100% steps up to N*100%.
        let cpu_top = if is_proc {
            let peak = cpu_pts.iter().map(|p| p.1).fold(0.0, f64::max);
            ((peak / 100.0).ceil() * 100.0).clamp(100.0, self.ncpu as f64 * 100.0)
        } else {
            100.0
        };
        let cpu_val = last.map_or("--".into(), |p| format!("{:.1}%", p.cpu));
        let cpu_title = title("CPU", cpu_val, CPU_COLOR, &scope, None);
        let cpu_labels = [0.0, cpu_top / 2.0, cpu_top].map(|v| format!("{v:.0}%"));
        render_chart(f, cpu_area, cpu_title, &cpu_pts, CPU_COLOR, range, cpu_top, cpu_labels);

        let (mem_top, mem_val, extra) = if is_proc {
            let peak = mem_pts.iter().map(|p| p.1).fold(0.0, f64::max);
            let val = last.map_or("--".into(), |p| fmt_bytes(p.mem));
            (nice_ceil_bytes(peak * 1.1), val, Some(format!("peak {}", fmt_bytes(peak))))
        } else {
            let total = self.mem_total as f64;
            let val = last.map_or("--".into(), |p| fmt_bytes(p.mem));
            let pct = last.filter(|_| total > 0.0).map(|p| format!("of {} ({:.0}%)", fmt_bytes(total), p.mem / total * 100.0));
            (total.max(1.0), val, pct)
        };
        let mem_title = title(if is_proc { "RSS" } else { "RAM" }, mem_val, MEM_COLOR, &scope, extra);
        let mem_labels = [0.0, mem_top / 2.0, mem_top].map(fmt_bytes_short);
        render_chart(f, mem_area, mem_title, &mem_pts, MEM_COLOR, range, mem_top, mem_labels);

        self.draw_status(f, status_area, bucket, range, capped);

        if let Some(p) = &mut self.picker {
            p.draw(f, area);
        }
    }

    fn draw_status(&self, f: &mut Frame, area: Rect, bucket: Duration, range: Duration, capped: bool) {
        let dim = Style::new().fg(Color::DarkGray);
        let key = Style::new().bold();
        let mode = Span::styled(if self.is_history() { " HIST " } else { " LIVE " }, Style::new().reversed().bold());
        let keys = [
            Span::styled("│ ", dim),
            Span::styled("q", key),
            Span::styled(" quit ", dim),
            Span::styled("/", key),
            Span::styled(" pick ", dim),
            Span::styled("Esc", key),
            Span::styled(" reset ", dim),
        ];
        let mut warnings = Vec::new();
        if capped {
            warnings.push(format!("only {} stored", fmt_dur(range)));
        }
        warnings.extend(self.note.clone());
        let warnings = warnings.into_iter().flat_map(|w| [Span::styled("│ ", dim), Span::styled(w + " ", Style::new().fg(Color::Yellow))]);
        let (iv, bk) = (fmt_dur(self.interval), fmt_dur(bucket));
        // Most detailed variant that fits; warnings and keys always stay.
        let infos = [
            format!(" every {iv} · bucket {bk} max · {} ", fmt_dur(range)),
            format!(" every {iv} · bucket {bk} max "),
            format!(" {iv}/{bk} max "),
        ];
        let rest: Vec<Span> = keys.into_iter().chain(warnings).collect();
        let rest_w: usize = rest.iter().map(|s| s.width()).sum::<usize>() + mode.width();
        let info = infos.iter().find(|i| i.chars().count() + rest_w <= area.width as usize).unwrap_or(&infos[2]);
        let mut spans = vec![mode, Span::raw(info.clone())];
        spans.extend(rest);
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn read_proc(ring: &Ring<ProcRec>, from: u64, key: &ProcKey, series: &mut Series) -> std::io::Result<u64> {
    ring.read_from(from, |r| {
        if r.pid == key.pid && r.start_time == key.start_time {
            series.push(Point { ts_ms: r.ts_ms, cpu: r.cpu as f64, mem: r.rss as f64 });
        }
    })
}

fn title(label: &str, value: String, color: Color, scope: &str, extra: Option<String>) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {label} "), Style::new().bold()),
        Span::styled(value, Style::new().fg(color).bold()),
    ];
    if let Some(e) = extra {
        spans.push(Span::raw(format!(" {e}")));
    }
    spans.push(Span::styled(" · ", Style::new().fg(Color::DarkGray)));
    spans.push(Span::raw(format!("{scope} ")));
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn render_chart(
    f: &mut Frame,
    area: Rect,
    title: Line<'static>,
    pts: &[(f64, f64)],
    color: Color,
    range: Duration,
    y_top: f64,
    y_labels: [String; 3],
) {
    let axis_style = Style::new().fg(Color::DarkGray);
    let r = range.as_secs_f64();
    let x_labels = [format!("-{}", fmt_dur(range)), format!("-{}", fmt_dur(range / 2)), "now".to_string()];
    // Same label width on both charts keeps their time axes aligned.
    let y_labels = y_labels.map(|l| format!("{l:>5}"));
    let dataset = Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(color)).data(pts);
    let chart = Chart::new(vec![dataset])
        .block(Block::bordered().border_style(axis_style).title(title))
        .x_axis(Axis::default().style(axis_style).bounds([-r, 0.0]).labels(x_labels))
        .y_axis(Axis::default().style(axis_style).bounds([0.0, y_top]).labels(y_labels));
    f.render_widget(chart, area);
}
