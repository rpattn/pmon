//! The TUI: two stacked charts plus a status line.

use crate::agg::{self, Point};
use crate::picker::{self, Action, PickItem, Picker, ProcKey};
use crate::sampler::{self, Sampler};
use crate::settings::{self, DEFAULT_TOP, GraphStyle, MAX_TOP, Settings};
use crate::theme::{Theme, theme};
use crate::store::{self, PROC_FILE, ProcRec, Ring, SYS_FILE, SysRec};
use crate::util::{fmt_bytes, fmt_bytes_short, fmt_dur, nice_ceil_bytes, now_ms};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Clear, Dataset, GraphType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const LIVE_INTERVAL: Duration = Duration::from_secs(1);
const LIVE_WINDOW: Duration = Duration::from_secs(120);
const HISTORY_WINDOW: Duration = Duration::from_secs(600);
const MIN_WINDOW: Duration = Duration::from_secs(30);
/// Borders plus the y-axis label column.
pub const CHART_CHROME: u16 = 2 + 7;

/// Live mode records the busiest this-many processes (by CPU and by memory)
/// each second as candidates for the top-process lines.
const TRACK: usize = 16;

pub struct Options {
    pub history: bool,
    pub process: Option<Option<String>>,
    pub since: Option<Duration>,
    pub agg: Option<Duration>,
    pub settings: Settings,
    /// A message to show briefly in the status line at start-up.
    pub flash: Option<String>,
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

/// Samples of one process for the top-process lines.
struct Tracked {
    name: String,
    series: Series,
}

/// A top-process line ready to draw.
pub struct TopLine {
    pub label: String,
    pub value: String,
    pub color: Color,
    pub pts: agg::Xy,
}

struct ProcView {
    key: ProcKey,
    series: Series,
    gone: bool,
}

enum Source {
    Live(Box<Sampler>),
    History { sys: Ring<SysRec>, procs: Option<Ring<ProcRec>>, sys_cur: u64, proc_cur: u64, top_cur: u64 },
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
    /// Short-lived message, e.g. after saving settings.
    flash: Option<(String, Instant)>,
    quit: bool,
    settings: Settings,
    theme: Theme,
    /// Top-line count to restore when `t` turns them back on.
    top_last: usize,
    tracked: HashMap<(u32, u64), Tracked>,
    help: bool,
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
        (Source::History { sys, procs, sys_cur: 0, proc_cur: 0, top_cur: 0 }, interval, ncpu)
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
        flash: opts.flash.map(|m| (m, Instant::now())),
        quit: false,
        theme: theme(opts.settings.theme),
        top_last: if opts.settings.top > 0 { opts.settings.top } else { DEFAULT_TOP },
        settings: opts.settings,
        tracked: HashMap::new(),
        help: false,
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
                Action::Kill(key, force) => p.kill_result(&key, force, sampler::kill(key.pid, key.start_time, force)),
            }
            return;
        }
        if self.help && matches!(k.code, KeyCode::Esc | KeyCode::Char('?')) {
            self.help = false;
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('/') => {
                self.help = false;
                let items = self.pick_items();
                self.picker = Some(Picker::new(items, String::new(), self.info_label()));
            }
            KeyCode::Esc => self.proc = None,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('s') => self.settings.style = settings::next(&self.settings.style),
            KeyCode::Char('g') => self.settings.charset = settings::next(&self.settings.charset),
            KeyCode::Char('c') => {
                self.settings.theme = settings::next(&self.settings.theme);
                self.theme = theme(self.settings.theme);
            }
            KeyCode::Char('t') => self.set_top(if self.settings.top > 0 { 0 } else { self.top_last }),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_top((self.settings.top.max(1) + 1).min(MAX_TOP)),
            KeyCode::Char('-') => self.set_top(self.settings.top.saturating_sub(1)),
            KeyCode::Char('w') => {
                let msg = match self.settings.save() {
                    Ok(path) => format!("saved defaults to {}", path.display()),
                    Err(e) => format!("save failed: {e:#}"),
                };
                self.flash = Some((msg, Instant::now()));
            }
            _ => {}
        }
    }

    fn set_top(&mut self, n: usize) {
        if n == 0 {
            if self.settings.top > 0 {
                self.top_last = self.settings.top;
            }
            // Drop the samples; turning lines back on starts afresh (live) or
            // re-reads the daemon's file (history).
            self.tracked.clear();
            if let Source::History { top_cur, .. } = &mut self.source {
                *top_cur = 0;
            }
        }
        self.settings.top = n;
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
                if self.settings.top > 0 {
                    s.refresh_processes(false);
                    let mut all: Vec<_> = s.processes().collect();
                    let mut keep = HashSet::new();
                    all.sort_by(|a, b| b.cpu_usage().total_cmp(&a.cpu_usage()));
                    keep.extend(all.iter().take(TRACK).filter(|p| p.cpu_usage() > 0.0).map(|p| p.pid()));
                    all.sort_by_key(|p| std::cmp::Reverse(p.memory()));
                    keep.extend(all.iter().take(TRACK).map(|p| p.pid()));
                    for p in all.into_iter().filter(|p| keep.contains(&p.pid())) {
                        let t = self.tracked.entry((p.pid().as_u32(), p.start_time())).or_insert_with(|| Tracked {
                            name: p.name().to_string_lossy().into_owned(),
                            series: Series::default(),
                        });
                        t.series.push(Point { ts_ms, cpu: p.cpu_usage() as f64, mem: p.memory() as f64 });
                    }
                }
            }
            Source::History { sys, procs, sys_cur, proc_cur, top_cur } => {
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
                if let Some(pr) = procs.as_ref().filter(|_| self.settings.top > 0) {
                    let tracked = &mut self.tracked;
                    let res = pr.read_from(*top_cur, |r| {
                        let t = tracked
                            .entry((r.pid, r.start_time))
                            .or_insert_with(|| Tracked { name: r.name_str(), series: Series::default() });
                        t.series.push(Point { ts_ms: r.ts_ms, cpu: r.cpu as f64, mem: r.rss as f64 });
                    });
                    match res {
                        Ok(c) => *top_cur = c,
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
        let first = self.system.pts.first().map_or(cutoff, |p| p.ts_ms.max(cutoff));
        self.tracked.retain(|_, t| {
            t.series.trim_before(first);
            !t.series.pts.is_empty()
        });
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
        let (cpu_top_lines, mem_top_lines) =
            if is_proc || self.settings.top == 0 { (vec![], vec![]) } else { self.top_lines(now, range, bucket) };
        let th = &self.theme;

        // CPU: system is 0–100% of the whole machine; a process is top-style
        // (100% = one core) on an axis that grows in 100% steps up to N*100%.
        let cpu_top = if is_proc {
            let peak = cpu_pts.iter().map(|p| p.1).fold(0.0, f64::max);
            ((peak / 100.0).ceil() * 100.0).clamp(100.0, self.ncpu as f64 * 100.0)
        } else {
            100.0
        };
        let cpu_val = last.map_or("--".into(), |p| format!("{:.1}%", p.cpu));
        render_chart(
            f,
            cpu_area,
            ChartSpec {
                title: title("CPU", cpu_val, th.cpu, &scope, None, th),
                main: &cpu_pts,
                color: th.cpu,
                top: &cpu_top_lines,
                y_top: cpu_top,
                y_labels: [0.0, cpu_top / 2.0, cpu_top].map(|v| format!("{v:.0}%")),
            },
            range,
            bucket,
            &self.settings,
            th,
        );

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
        render_chart(
            f,
            mem_area,
            ChartSpec {
                title: title(if is_proc { "RSS" } else { "RAM" }, mem_val, th.mem, &scope, extra, th),
                main: &mem_pts,
                color: th.mem,
                top: &mem_top_lines,
                y_top: mem_top,
                y_labels: [0.0, mem_top / 2.0, mem_top].map(fmt_bytes_short),
            },
            range,
            bucket,
            &self.settings,
            th,
        );

        self.draw_status(f, status_area, bucket, range, capped);

        if self.help {
            self.draw_help(f, area);
        }
        if let Some(p) = &mut self.picker {
            p.draw(f, area, &self.theme);
        }
    }

    /// The `n` busiest processes in view, by mean CPU and by mean memory.
    /// CPU is scaled to the whole machine so the lines sit under the total.
    fn top_lines(&self, now: u64, range: Duration, bucket: Duration) -> (Vec<TopLine>, Vec<TopLine>) {
        let n = self.settings.top;
        let start = now.saturating_sub(range.as_millis() as u64);
        let mut stats: Vec<(&(u32, u64), &Tracked, f64, f64)> = self
            .tracked
            .iter()
            .filter_map(|(k, t)| {
                let pts = &t.series.pts[t.series.pts.partition_point(|p| p.ts_ms < start)..];
                (!pts.is_empty()).then(|| {
                    let len = pts.len() as f64;
                    (k, t, pts.iter().map(|p| p.cpu).sum::<f64>() / len, pts.iter().map(|p| p.mem).sum::<f64>() / len)
                })
            })
            .collect();
        let mut colors: HashMap<(u32, u64), Color> = HashMap::new();
        let palette = &self.theme.palette;
        let ncpu = self.ncpu as f64;
        let mut build = |stats: &[(&(u32, u64), &Tracked, f64, f64)], cpu: bool| -> Vec<TopLine> {
            stats
                .iter()
                .map(|&(k, t, _, _)| {
                    let next = palette[colors.len() % palette.len()];
                    let color = *colors.entry(*k).or_insert(next);
                    let (c, m) = agg::aggregate(&t.series.pts, now, range, bucket);
                    let last = t.series.last().copied().unwrap_or(Point { ts_ms: 0, cpu: 0.0, mem: 0.0 });
                    let (pts, value) = if cpu {
                        (c.into_iter().map(|(x, y)| (x, y / ncpu)).collect(), format!("{:.1}%", last.cpu / ncpu))
                    } else {
                        (m, fmt_bytes(last.mem))
                    };
                    TopLine { label: t.name.chars().take(14).collect(), value, color, pts }
                })
                .collect()
        };
        stats.sort_by(|a, b| b.2.total_cmp(&a.2));
        // Skip idle processes (under 0.1% of a core on average).
        let by_cpu: Vec<_> = stats.iter().filter(|s| s.2 >= 0.1).take(n).copied().collect();
        let cpu_lines = build(&by_cpu, true);
        stats.sort_by(|a, b| b.3.total_cmp(&a.3));
        let by_mem: Vec<_> = stats.iter().take(n).copied().collect();
        let mem_lines = build(&by_mem, false);
        (cpu_lines, mem_lines)
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let st = &self.settings;
        let top = if st.top > 0 { format!("on ({})", st.top) } else { "off".into() };
        let rows: [(&str, &str, String); 11] = [
            ("q", "quit", String::new()),
            ("/", "pick a process (Del kills)", String::new()),
            ("Esc", "back to system-wide", String::new()),
            ("t", "top-process lines", top),
            ("+ -", "more / fewer top lines", String::new()),
            ("s", "graph style", settings::name(&st.style)),
            ("g", "characters", settings::name(&st.charset)),
            ("c", "theme", settings::name(&st.theme)),
            ("w", "save these as defaults", String::new()),
            ("", "", String::new()),
            ("Esc ?", "close help", String::new()),
        ];
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|(k, what, val)| {
                Line::from(vec![
                    Span::styled(format!(" {k:<6}"), Style::new().bold()),
                    Span::raw(format!("{what:<28}")),
                    Span::styled(val, Style::new().fg(th.accent).bold()),
                ])
            })
            .collect();
        let [popup] = Layout::horizontal([Constraint::Length(50.min(area.width))]).flex(Flex::Center).areas(area);
        let [popup] =
            Layout::vertical([Constraint::Length((lines.len() as u16 + 2).min(area.height))]).flex(Flex::Center).areas(popup);
        f.render_widget(Clear, popup);
        let block = Block::bordered().border_style(Style::new().fg(th.accent)).title(" Keys ".bold());
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect, bucket: Duration, range: Duration, capped: bool) {
        let th = &self.theme;
        let dim = Style::new().fg(th.dim);
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
            Span::styled("?", key),
            Span::styled(" help ", dim),
        ];
        let mut warnings = Vec::new();
        if capped {
            warnings.push(format!("only {} stored", fmt_dur(range)));
        }
        warnings.extend(self.note.clone());
        let mut warnings: Vec<Span> = warnings
            .into_iter()
            .flat_map(|w| [Span::styled("│ ", dim), Span::styled(w + " ", Style::new().fg(th.warn))])
            .collect();
        if let Some((msg, at)) = &self.flash
            && at.elapsed() < Duration::from_secs(5)
        {
            warnings.extend([Span::styled("│ ", dim), Span::styled(format!("{msg} "), Style::new().fg(th.ok))]);
        }
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

pub fn title(label: &str, value: String, color: Color, scope: &str, extra: Option<String>, th: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {label} "), Style::new().bold()),
        Span::styled(value, Style::new().fg(color).bold()),
    ];
    if let Some(e) = extra {
        spans.push(Span::raw(format!(" {e}")));
    }
    spans.push(Span::styled(" · ", Style::new().fg(th.dim)));
    spans.push(Span::raw(format!("{scope} ")));
    Line::from(spans)
}

pub struct ChartSpec<'a> {
    pub title: Line<'static>,
    pub main: &'a [(f64, f64)],
    pub color: Color,
    pub top: &'a [TopLine],
    pub y_top: f64,
    pub y_labels: [String; 3],
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

pub fn render_chart(f: &mut Frame, area: Rect, spec: ChartSpec, range: Duration, bucket: Duration, st: &Settings, th: &Theme) {
    let axis_style = Style::new().fg(th.dim);
    let r = range.as_secs_f64();
    let x_labels = [format!("-{}", fmt_dur(range)), format!("-{}", fmt_dur(range / 2)), "now".to_string()];
    // Same label width on both charts keeps their time axes aligned.
    let y_labels = spec.y_labels.map(|l| format!("{l:>5}"));
    let marker = st.charset.marker();
    let main_pts = if st.style == GraphStyle::Bars {
        use settings::Charset::*;
        let per_cell = if matches!(st.charset, Braille | Octant | Sextant | Quadrant) { 2 } else { 1 };
        let slots = area.width.saturating_sub(CHART_CHROME).max(1) as usize * per_cell;
        bars(spec.main, r, bucket.as_secs_f64(), slots)
    } else {
        spec.main.to_vec()
    };
    let main = Dataset::default()
        .marker(marker)
        .graph_type(st.style.graph_type())
        .style(Style::new().fg(spec.color))
        .data(&main_pts);
    // A cell shows only the last dataset drawn into it, so overlaid lines
    // would punch holes in a filled area. In area mode the processes become
    // stacked bands instead: biggest first (at the bottom), the total's colour
    // filling the rest.
    let mut stacked = if st.style == GraphStyle::Area { stack(spec.main, spec.top) } else { vec![] };
    if !stacked.is_empty() {
        // Plot size in cells: minus borders, x-axis line and labels (height);
        // minus borders, the 5-wide y labels and the y-axis line (width).
        let rows = area.height.saturating_sub(4) as usize;
        let cols = area.width.saturating_sub(8) as usize;
        let (dx, dy) = st.charset.dots_per_cell();
        snap_bands(spec.main, &mut stacked, spec.y_top, rows, dy);
        for band in &mut stacked {
            *band = band_columns(band, r, bucket.as_secs_f64(), cols, dx);
        }
    }
    let datasets: Vec<Dataset> = if st.style == GraphStyle::Area {
        let bands = spec.top.iter().zip(&stacked).rev().map(|(t, pts)| {
            Dataset::default().marker(marker).graph_type(GraphType::Bar).style(Style::new().fg(t.color)).data(pts)
        });
        std::iter::once(main).chain(bands).collect()
    } else {
        let lines = spec.top.iter().map(|t| {
            Dataset::default().marker(marker).graph_type(st.style.graph_type()).style(Style::new().fg(t.color)).data(&t.pts)
        });
        // The total goes last so it stays visible on top of the process lines.
        lines.chain(std::iter::once(main)).collect()
    };

    let mut block = Block::bordered().border_style(axis_style).title(spec.title);
    if !spec.top.is_empty() {
        let swatch = if st.charset == settings::Charset::Ascii { "# " } else { "■ " };
        let mut spans = vec![Span::raw(" ")];
        for t in spec.top {
            spans.push(Span::styled(swatch, Style::new().fg(t.color)));
            spans.push(Span::raw(format!("{} {}  ", t.label, t.value)));
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
