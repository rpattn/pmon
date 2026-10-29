//! The monitor: panels (CPU, RAM, disk, network, GPU) with optional stats
//! boxes, a status line, and the picker / help overlays.

use crate::agg::{self, METRICS, Point, Xy};
use crate::chart::TopLine;
use crate::daemon;
use crate::panels::{self, Panel, StatList, Stats, Window};
use crate::picker::{self, Action, PickItem, Picker, ProcKey};
use crate::sampler::{self, Sampler, owned_by};
use crate::settings::{self, Arrangement, DEFAULT_TOP, MAX_TOP, PanelKind, Settings};
use crate::store::{self, PROC_FILE, ProcRec, Ring, SYS_FILE, SysRec};
use crate::theme::{Theme, theme};
use crate::util::{fmt_bytes, fmt_bytes_short, fmt_dur, nice_ceil_bytes, now_ms};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame, TerminalOptions, Viewport};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const LIVE_INTERVAL: Duration = Duration::from_secs(1);
const LIVE_WINDOW: Duration = Duration::from_secs(120);
const HISTORY_WINDOW: Duration = Duration::from_secs(600);
/// Narrowest view while data is still short of the window: history needs a
/// few of the daemon's 2s samples; live starts at ten 1s samples so the
/// chart is filled almost from the first second.
const MIN_WINDOW_HISTORY: Duration = Duration::from_secs(30);
const MIN_WINDOW_LIVE: Duration = Duration::from_secs(10);

/// Live mode records the busiest this-many processes by each metric every
/// second, as candidates for top-process lines and stats lists.
const TRACK: usize = 16;
/// Bounds on how often the view is redrawn between samples (see `draw`).
const MIN_FRAME: Duration = Duration::from_millis(100);
const MAX_FRAME: Duration = Duration::from_secs(1);
/// Entries in a stats box's ranked list.
const STATS_LIST: usize = 8;

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

/// What a tracked series is: one process, or every process sharing a name
/// (an app, e.g. a browser's many processes).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TKey {
    Proc(u32, u64),
    App(String),
}

/// Samples of one process or app, for top-process lines and stats lists.
struct Tracked {
    name: String,
    /// Processes in the app at the latest sample (1 for a process).
    members: usize,
    series: Series,
}

impl Tracked {
    fn label(&self) -> String {
        if self.members > 1 { format!("{} ×{}", self.name, self.members) } else { self.name.clone() }
    }
}

/// The scoped process, or app when `key.pid == 0` (see `ProcKey::app`).
struct ProcView {
    key: ProcKey,
    series: Series,
    gone: bool,
    /// For an app: processes at the latest sample.
    members: usize,
}

impl ProcView {
    fn is_app(&self) -> bool {
        self.key.is_app()
    }
    fn label(&self) -> String {
        let gone = if self.gone { " (exited)" } else { "" };
        if self.is_app() {
            format!("{} ×{}{gone}", self.key.name, self.members)
        } else {
            format!("{} [{}]{gone}", self.key.name, self.key.pid)
        }
    }
}

enum Source {
    Live(Box<Sampler>),
    History {
        sys: Ring<SysRec>,
        procs: Option<Ring<ProcRec>>,
        sys_cur: u64,
        proc_cur: u64,
        top_cur: u64,
        /// Samples locally (not saved) while no daemon is running, so the
        /// chart carries on live after the stored history.
        fallback: Option<Box<Sampler>>,
    },
}

/// A process metric to rank or draw: one value, or the sum of two (disk
/// read + write).
#[derive(Clone, Copy)]
enum Measure {
    One(usize),
    Sum(usize, usize),
}

impl Measure {
    fn of(self, v: &[f64; METRICS]) -> f64 {
        match self {
            Measure::One(m) => v[m],
            Measure::Sum(a, b) => v[a] + v[b],
        }
    }
    fn xy(self, agg: &[Xy; METRICS]) -> Xy {
        match self {
            Measure::One(m) => agg[m].clone(),
            Measure::Sum(a, b) => agg[a].iter().zip(&agg[b]).map(|(p, q)| (p.0, p.1 + q.1)).collect(),
        }
    }
}

struct App {
    source: Source,
    interval: Duration,
    window: Duration,
    agg: Option<Duration>,
    ncpu: usize,
    mem_total: u64,
    gpu_mem_total: u64,
    has_gpu: bool,
    system: Series,
    /// Per-interface (name, received, sent) bytes/s from the last local sample.
    ifaces: Vec<(String, f64, f64)>,
    proc: Option<ProcView>,
    picker: Option<Picker>,
    note: Option<String>,
    /// Short-lived message, e.g. after saving settings; `true` = an error.
    flash: Option<(String, Instant, bool)>,
    /// History mode: whether a daemon is writing new samples.
    daemon_up: bool,
    quit: bool,
    /// Drawing the last frame left on screen in inline mode.
    finished: bool,
    started: Instant,
    settings: Settings,
    theme: Theme,
    /// Top-line count to restore when `t` turns them back on.
    top_last: usize,
    /// Collect per-process samples. Stays on once top lines or stats have
    /// been shown, so hiding and showing them again is instant.
    top_tracking: bool,
    /// Take the next sample soon instead of waiting a full interval.
    tick_soon: bool,
    tracked: HashMap<TKey, Tracked>,
    help: bool,
    /// A layout template from the config, kept in the `l` cycle.
    custom_layout: Option<Arrangement>,
    /// How often to redraw so the chart slides smoothly (set by `draw`).
    frame_every: Duration,
}

pub fn run(opts: Options) -> Result<i32> {
    let (source, interval, ncpu, has_gpu) = if opts.history {
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
        let source = Source::History { sys, procs, sys_cur: 0, proc_cur: 0, top_cur: 0, fallback: None };
        (source, interval, ncpu, false)
    } else {
        let s = Sampler::new()?;
        let (ncpu, gpu) = (s.ncpu(), s.has_gpu());
        (Source::Live(Box::new(s)), LIVE_INTERVAL, ncpu, gpu)
    };
    let default_window = if opts.history { HISTORY_WINDOW } else { LIVE_WINDOW };
    let tracking = opts.settings.top > 0 || opts.settings.stats;
    let mut app = App {
        source,
        interval,
        window: opts.since.unwrap_or(default_window).max(interval * 2),
        agg: opts.agg,
        ncpu,
        mem_total: 0,
        gpu_mem_total: 0,
        has_gpu,
        system: Series::default(),
        ifaces: vec![],
        proc: None,
        picker: None,
        note: None,
        flash: opts.flash.map(|m| (m, Instant::now(), false)),
        daemon_up: true,
        quit: false,
        finished: false,
        started: Instant::now(),
        theme: theme(opts.settings.theme),
        top_last: if opts.settings.top > 0 { opts.settings.top } else { DEFAULT_TOP },
        top_tracking: tracking,
        tick_soon: false,
        settings: opts.settings,
        tracked: HashMap::new(),
        help: false,
        custom_layout: None,
        frame_every: MAX_FRAME,
    };
    if matches!(app.settings.layout, Arrangement::Rows(_)) {
        app.custom_layout = Some(app.settings.layout.clone());
    }
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

    // Inline: only as many rows as the panels use at this width (plus the
    // margin and status line), up to the configured height.
    let inline = app.settings.inline.map(|rows| {
        let width = crossterm::terminal::size().map_or(80, |(w, _)| w);
        let used = panels::used_height(Rect::new(0, 0, width, rows.saturating_sub(2)), &app.visible_panels(), &app.settings.layout, app.settings.focus);
        (used + 2).clamp(8, rows)
    });
    let mut terminal = match inline {
        Some(rows) => match ratatui::try_init_with_options(TerminalOptions { viewport: Viewport::Inline(rows) }) {
            Ok(t) => t,
            Err(e) => {
                // Raw mode may already be on; don't leave the shell in it.
                ratatui::restore();
                anyhow::bail!("can't draw inline here ({e}); try without --inline");
            }
        },
        None => ratatui::init(),
    };
    let res = app.event_loop(&mut terminal);
    if inline.is_some() {
        // Leave a clean, static last frame in the scrollback, then put the
        // prompt below it.
        app.finished = true;
        app.picker = None;
        app.help = false;
        let mut bottom = 0;
        let _ = terminal.draw(|f| {
            app.draw(f);
            bottom = f.area().bottom().saturating_sub(1);
        });
        let _ = terminal.set_cursor_position((0, bottom));
    }
    ratatui::restore();
    if inline.is_some() {
        println!();
    }
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
            // Wake for the next sample or the next frame, whichever is first.
            let wake = next_tick.min(Instant::now() + self.frame_every);
            if event::poll(wake.saturating_duration_since(Instant::now()))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        self.on_key(k);
                        if std::mem::take(&mut self.tick_soon) {
                            next_tick = next_tick.min(Instant::now() + Duration::from_millis(300));
                        }
                    }
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
                Action::Kill(keys, force) => {
                    let results = keys.iter().map(|k| sampler::kill(k.pid, k.start_time, force)).collect();
                    p.kill_result(&keys, force, results);
                }
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
            KeyCode::Char('f') => {
                let choices: Vec<Option<PanelKind>> = std::iter::once(None).chain(PanelKind::ALL.map(Some)).collect();
                let i = choices.iter().position(|&c| c == self.settings.focus).unwrap_or(0);
                self.settings.focus = choices[(i + 1) % choices.len()];
                let what = self.settings.focus.map_or("none (last panel spans)".into(), |k| settings::name(&k));
                self.flash = Some((format!("focus: {what}"), Instant::now(), false));
            }
            KeyCode::Char('l') => self.settings.layout = self.settings.layout.step(self.custom_layout.as_ref(), 1),
            KeyCode::Char('a') => {
                self.settings.group = !self.settings.group;
                let how = if self.settings.group { "grouped by app" } else { "one row per process" };
                self.flash = Some((format!("processes {how}"), Instant::now(), false));
            }
            KeyCode::Char('i') => {
                self.settings.stats = !self.settings.stats;
                if self.settings.stats {
                    self.start_tracking();
                }
            }
            KeyCode::Char(c @ '1'..='5') => {
                let kind = PanelKind::ALL[c as usize - '1' as usize];
                let panels = &mut self.settings.panels;
                if let Some(i) = panels.iter().position(|&p| p == kind) {
                    panels.remove(i);
                } else {
                    panels.push(kind);
                    panels.sort();
                }
            }
            KeyCode::Char('t') => self.set_top(if self.settings.top > 0 { 0 } else { self.top_last }),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_top((self.settings.top.max(1) + 1).min(MAX_TOP)),
            KeyCode::Char('-') => self.set_top(self.settings.top.saturating_sub(1)),
            KeyCode::Char('w') => {
                let (msg, err) = match self.settings.save() {
                    Ok(path) => (format!("saved defaults to {}", path.display()), false),
                    Err(e) => (format!("save failed: {e:#}"), true),
                };
                self.flash = Some((msg, Instant::now(), err));
            }
            KeyCode::Char('d') => {
                let (msg, err) = match daemon::start_quiet() {
                    Ok((pid, retain)) => {
                        self.daemon_up = true;
                        let hint = if self.is_history() { "" } else { "; pmon -H shows history" };
                        (format!("daemon started (pid {pid}, keeping {}){hint}", fmt_dur(retain)), false)
                    }
                    Err(e) => (format!("{e:#}"), true),
                };
                self.flash = Some((msg, Instant::now(), err));
                self.tick_soon = true;
            }
            _ => {}
        }
    }

    /// Shows `n` top-process lines (0 hides them). Hiding keeps collecting.
    fn set_top(&mut self, n: usize) {
        if n == 0 && self.settings.top > 0 {
            self.top_last = self.settings.top;
        }
        if n > 0 {
            self.start_tracking();
        }
        self.settings.top = n;
    }

    fn start_tracking(&mut self) {
        if self.top_tracking {
            return;
        }
        self.top_tracking = true;
        // A process's CPU % needs two readings: take the first one now and
        // the second shortly, rather than a full interval or two from now.
        if let Source::Live(s) = &mut self.source {
            s.refresh_processes(false);
        }
        self.tick_soon = true;
    }

    fn tick(&mut self) {
        self.note = None;
        match &mut self.source {
            Source::Live(s) => {
                let mut sink = Sink {
                    system: &mut self.system,
                    mem_total: &mut self.mem_total,
                    gpu_mem_total: &mut self.gpu_mem_total,
                    ifaces: &mut self.ifaces,
                    proc: self.proc.as_mut(),
                    tracked: &mut self.tracked,
                };
                sample_live(s, &mut sink, self.top_tracking, false);
            }
            Source::History { sys, procs, sys_cur, proc_cur, top_cur, fallback } => {
                let (system, mem_total, gpu_total) = (&mut self.system, &mut self.mem_total, &mut self.gpu_mem_total);
                match sys.read_from(*sys_cur, |r| {
                    *mem_total = r.mem_total;
                    *gpu_total = r.gpu_mem_total;
                    system.push(Point::new(r.ts_ms, r.v));
                }) {
                    Ok(c) => *sys_cur = c,
                    Err(e) => self.note = Some(format!("read error: {e}")),
                }
                if let (Some(pv), Some(pr)) = (self.proc.as_mut(), procs.as_ref()) {
                    match read_proc(pr, *proc_cur, pv) {
                        Ok(c) => *proc_cur = c,
                        Err(e) => self.note = Some(format!("read error: {e}")),
                    }
                }
                if let Some(pr) = procs.as_ref().filter(|_| self.top_tracking) {
                    let tracked = &mut self.tracked;
                    // Records of one daemon sample share a timestamp and arrive
                    // together, so apps are summed per batch.
                    let mut batch: (u64, HashMap<String, (usize, [f64; METRICS])>) = (0, HashMap::new());
                    let flush = |tracked: &mut HashMap<TKey, Tracked>, (ts, apps): &mut (u64, HashMap<String, (usize, [f64; METRICS])>)| {
                        for (name, (n, v)) in apps.drain() {
                            track(tracked, TKey::App(name.clone()), || name, n, Point::new(*ts, v));
                        }
                    };
                    let res = pr.read_from(*top_cur, |r| {
                        if r.ts_ms != batch.0 {
                            flush(tracked, &mut batch);
                            batch.0 = r.ts_ms;
                        }
                        let name = r.name_str();
                        let e = batch.1.entry(name.clone()).or_insert((0, [0.0; METRICS]));
                        e.0 += 1;
                        for (a, b) in e.1.iter_mut().zip(&r.v) {
                            *a += b;
                        }
                        track(tracked, TKey::Proc(r.pid, r.start_time), || name, 1, Point::new(r.ts_ms, r.v));
                    });
                    flush(tracked, &mut batch);
                    match res {
                        Ok(c) => *top_cur = c,
                        Err(e) => self.note = Some(format!("read error: {e}")),
                    }
                }
                // Drop what the daemon has already overwritten.
                let retention = sys.interval_ms as u64 * sys.capacity();
                let newest = self.system.last().map_or(0, |p| p.ts_ms);
                self.system.trim_before(newest.saturating_sub(retention));
                // With no daemon, carry on with local samples (shown, not saved).
                // Its records stay the source whenever it runs.
                self.daemon_up = daemon::is_running();
                if self.daemon_up {
                    *fallback = None;
                } else if let Some(s) = fallback {
                    let mut sink = Sink {
                        system: &mut self.system,
                        mem_total: &mut self.mem_total,
                        gpu_mem_total: &mut self.gpu_mem_total,
                        ifaces: &mut self.ifaces,
                        proc: self.proc.as_mut(),
                        tracked: &mut self.tracked,
                    };
                    sample_live(s, &mut sink, self.top_tracking, true);
                } else if let Ok(mut s) = Sampler::new() {
                    // CPU % needs two readings; sampling starts next tick.
                    s.refresh_processes(false);
                    *fallback = Some(Box::new(s));
                }
                self.has_gpu = self.gpu_mem_total > 0
                    || fallback.as_ref().is_some_and(|s| s.has_gpu())
                    || self.system.pts.iter().any(|p| p.v[agg::GPU] > 0.0);
                let newest = self.system.last().map_or(0, |p| p.ts_ms);
                if let Some(pv) = &mut self.proc {
                    let last = pv.series.last().map_or(0, |p| p.ts_ms);
                    pv.gone = newest > last + 3 * sys.interval_ms as u64;
                }
                let stale = now_ms().saturating_sub(newest);
                if !self.daemon_up {
                    self.note = Some("not recording: d starts daemon".into());
                } else if stale > 5 * sys.interval_ms as u64 {
                    self.note = Some(format!("no new samples for {}", fmt_dur(Duration::from_millis(stale))));
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
        let mut pv = ProcView { key, series: Series::default(), gone: false, members: 0 };
        if let Source::History { procs: Some(pr), proc_cur, .. } = &mut self.source {
            match read_proc(pr, 0, &mut pv) {
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
                if self.settings.group {
                    picker::add_apps(&mut items);
                }
                items
            }
            Source::History { procs: Some(pr), fallback, .. } => {
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
                // Sampling locally: add your processes started since the daemon
                // stopped, and count anything still running as running.
                if let Some(s) = fallback {
                    s.refresh_processes(false);
                    if let Some(me) = s.current_user() {
                        for p in s.processes().filter(|p| owned_by(p, &me)) {
                            let mut r =
                                ProcRec { ts_ms: now, start_time: p.start_time(), pid: p.pid().as_u32(), v: s.metrics(p), name: [0; 32] };
                            r.set_name(p.name().as_encoded_bytes());
                            latest.insert((r.pid, r.start_time), r);
                        }
                        newest = now;
                    }
                }
                let live_after = newest.saturating_sub(3 * pr.interval_ms as u64);
                let mut recs: Vec<ProcRec> = latest.into_values().collect();
                // Running first (busiest first), then exited (most recently seen first).
                recs.sort_by(|a, b| {
                    let (a_live, b_live) = (a.ts_ms >= live_after, b.ts_ms >= live_after);
                    b_live.cmp(&a_live).then(if a_live { b.v[agg::CPU].total_cmp(&a.v[agg::CPU]) } else { b.ts_ms.cmp(&a.ts_ms) })
                });
                let mut items: Vec<PickItem> = recs
                    .into_iter()
                    .map(|r| {
                        let alive = r.ts_ms >= live_after;
                        let seen = if alive {
                            "running".to_string()
                        } else {
                            format!("exited, last seen {} ago", fmt_dur(Duration::from_millis(now.saturating_sub(r.ts_ms))))
                        };
                        let key = ProcKey { pid: r.pid, start_time: r.start_time, name: r.name_str() };
                        PickItem::new(key, r.v[agg::CPU] as f32, r.v[agg::MEM] as u64, seen, alive)
                    })
                    .collect();
                if self.settings.group {
                    picker::add_apps(&mut items);
                }
                items
            }
            Source::History { procs: None, .. } => Vec::new(),
        }
    }

    /// Effective time range, and whether it was capped by stored history.
    /// While there is less data than the window, the view covers just the
    /// data (at least 10s live, 30s history) so it fills the chart: live
    /// starts zoomed in and widens as time passes; -H is capped (and says so).
    fn range(&self, now: u64) -> (Duration, bool) {
        // Live process series start when picked; history is about the daemon's data.
        let series = match &self.proc {
            Some(pv) if !self.is_history() => &pv.series,
            _ => &self.system,
        };
        let first = series.pts.first().map_or(now, |p| p.ts_ms);
        let have = Duration::from_millis(now.saturating_sub(first));
        if have + self.interval < self.window {
            let min = if self.is_history() { MIN_WINDOW_HISTORY } else { MIN_WINDOW_LIVE };
            (have.max(min).min(self.window), self.is_history())
        } else {
            (self.window, false)
        }
    }

    /// Panels to draw: the configured ones, minus GPU when there's none.
    fn visible_panels(&self) -> Vec<PanelKind> {
        self.settings.panels.iter().copied().filter(|&p| p != PanelKind::Gpu || self.has_gpu).collect()
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < 30 || area.height < 8 {
            f.render_widget(Paragraph::new("terminal too small"), area);
            return;
        }
        // A blank row keeps the panels' bottom border off the status line.
        let [main_area, _, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(area);

        let now = now_ms();
        let (range, capped) = self.range(now);
        let kinds = self.visible_panels();
        let placed = panels::layout(main_area, &kinds, &self.settings.layout, self.settings.focus);
        let cols = panels::plot_cols(&placed, self.settings.stats);
        let bucket = self.agg.unwrap_or_else(|| agg::auto_bucket(range, cols, self.interval)).max(self.interval);
        // Samples drift a little, so one empty bucket is normal; two or more
        // missing samples is an outage.
        let gap = (bucket + self.interval * 2).as_secs_f64();
        // Time moves the chart left; redraw about when it has moved one dot
        // (braille has two per column), so short windows slide smoothly
        // while long ones cost about a frame a second.
        self.frame_every = (range / (cols as u32 * 2).max(1)).clamp(MIN_FRAME, MAX_FRAME);

        if kinds.is_empty() {
            let msg = "No panels shown: press 1-5 to add CPU, RAM, disk, network, GPU (or pmon --config)";
            f.render_widget(Paragraph::new(msg).style(Style::new().fg(self.theme.dim)), main_area);
        }
        let ctx = Ctx { now, range, bucket, start_ms: now.saturating_sub(range.as_millis() as u64) };
        let sys_agg = agg::aggregate(&self.system.pts, now, range, bucket);
        let proc_agg = self.proc.as_ref().map(|pv| agg::aggregate(&pv.series.pts, now, range, bucket));
        // A process keeps one colour across all panels.
        let mut colors: HashMap<TKey, Color> = HashMap::new();
        let window = Window { range, bucket, gap };
        for &(kind, rect) in &placed {
            let panel = self.panel(kind, &ctx, &sys_agg, proc_agg.as_ref(), &mut colors);
            panels::render(f, rect, &panel, &window, &self.settings, &self.theme);
        }

        self.draw_status(f, status_area, bucket, range, capped);
        if self.help {
            self.draw_help(f, area);
        }
        if let Some(p) = &mut self.picker {
            p.draw(f, area, &self.theme);
        }
    }

    /// Builds one panel for the current scope (system or selected process).
    fn panel(
        &self,
        kind: PanelKind,
        ctx: &Ctx,
        sys_agg: &[Xy; METRICS],
        proc_agg: Option<&[Xy; METRICS]>,
        colors: &mut HashMap<TKey, Color>,
    ) -> Panel {
        let th = &self.theme;
        let is_proc = self.proc.is_some();
        let (series, agg_v, scope) = match (&self.proc, proc_agg) {
            (Some(pv), Some(a)) => {
                (&pv.series, a, pv.label())
            }
            _ => (&self.system, sys_agg, "system".to_string()),
        };
        let last = series.last().map(|p| p.v);
        let now_of = |m: usize| last.map(|v| v[m]);
        let mm = |pts: &[Point], m: usize| agg::mean_max(pts, ctx.start_ms, m).unwrap_or((0.0, 0.0));
        let peak = |xy: &Xy| xy.iter().map(|p| p.1).fold(0.0, f64::max);
        let want_list = self.settings.stats && !is_proc;
        let show_top = self.settings.top > 0 && !is_proc;
        let ncpu = self.ncpu as f64;
        let pct = |v: f64| format!("{v:.1}%");
        let rate = |v: f64| format!("{}/s", fmt_bytes(v));
        let none = || "--".to_string();

        match kind {
            PanelKind::Cpu => {
                // System: 0–100% of the machine. A process: top-style (100% =
                // one core) on an axis growing in 100% steps up to N*100%.
                let y_top = if is_proc { ((peak(&agg_v[agg::CPU]) / 100.0).ceil() * 100.0).clamp(100.0, ncpu * 100.0) } else { 100.0 };
                let (avg, max) = mm(&series.pts, agg::CPU);
                let ranked = self.ranked(ctx, Measure::One(agg::CPU), 0.1);
                let top = if show_top { self.top_lines(ctx, &ranked, Measure::One(agg::CPU), 1.0 / ncpu, pct, colors) } else { vec![] };
                let list = want_list.then(|| self.list(&ranked, |v| pct(v / ncpu), colors));
                Panel {
                    title: chart_title("CPU", now_of(agg::CPU).map_or_else(none, pct), th.cpu, &scope, None, th),
                    main: agg_v[agg::CPU].clone(),
                    color: th.cpu,
                    extra: vec![],
                    legend: legend_of(&top),
                    top,
                    y_top,
                    y_labels: [0.0, y_top / 2.0, y_top].map(|v| format!("{v:.0}%")),
                    stats: Some(stats(vec![("now", now_of(agg::CPU).map_or_else(none, pct)), ("avg", pct(avg)), ("peak", pct(max))], list)),
                }
            }
            PanelKind::Ram => {
                let (avg, max) = mm(&series.pts, agg::MEM);
                let total = self.mem_total as f64;
                let (y_top, extra) = if is_proc {
                    (nice_ceil_bytes(peak(&agg_v[agg::MEM]) * 1.1), Some(format!("peak {}", fmt_bytes(max))))
                } else {
                    let share = now_of(agg::MEM).filter(|_| total > 0.0).map(|m| format!("of {} ({:.0}%)", fmt_bytes(total), m / total * 100.0));
                    (total.max(1.0), share)
                };
                let ranked = self.ranked(ctx, Measure::One(agg::MEM), 0.0);
                let top = if show_top { self.top_lines(ctx, &ranked, Measure::One(agg::MEM), 1.0, fmt_bytes, colors) } else { vec![] };
                let list = want_list.then(|| self.list(&ranked, fmt_bytes, colors));
                let now = now_of(agg::MEM).map_or_else(none, fmt_bytes);
                Panel {
                    title: chart_title(if is_proc { "RSS" } else { "RAM" }, now.clone(), th.mem, &scope, extra, th),
                    main: agg_v[agg::MEM].clone(),
                    color: th.mem,
                    extra: vec![],
                    legend: legend_of(&top),
                    top,
                    y_top,
                    y_labels: [0.0, y_top / 2.0, y_top].map(fmt_bytes_short),
                    stats: Some(stats(vec![("now", now), ("avg", fmt_bytes(avg)), ("peak", fmt_bytes(max))], list)),
                }
            }
            PanelKind::Disk => {
                let y_top = nice_ceil_bytes(peak(&agg_v[agg::DISK_R]).max(peak(&agg_v[agg::DISK_W])) * 1.1).max(1024.0);
                let io = Measure::Sum(agg::DISK_R, agg::DISK_W);
                let max = series.pts[series.pts.partition_point(|p| p.ts_ms < ctx.start_ms)..]
                    .iter()
                    .map(|p| io.of(&p.v))
                    .fold(0.0, f64::max);
                let ranked = self.ranked(ctx, io, 1.0);
                let list = want_list.then(|| self.list(&ranked, rate, colors));
                let (r, w) = (now_of(agg::DISK_R).map_or_else(none, rate), now_of(agg::DISK_W).map_or_else(none, rate));
                Panel {
                    title: chart_title("Disk", format!("R {r}  W {w}"), th.read, &scope, None, th),
                    main: agg_v[agg::DISK_R].clone(),
                    color: th.read,
                    extra: vec![(agg_v[agg::DISK_W].clone(), th.write)],
                    legend: vec![("read".into(), th.read), ("write".into(), th.write)],
                    top: vec![],
                    y_top,
                    y_labels: [0.0, y_top / 2.0, y_top].map(fmt_bytes_short),
                    stats: Some(stats(vec![("read", r), ("write", w), ("peak", rate(max))], list)),
                }
            }
            PanelKind::Net => {
                // No per-process network data, so this is always system-wide.
                let a = sys_agg;
                let y_top = nice_ceil_bytes(peak(&a[agg::NET_RX]).max(peak(&a[agg::NET_TX])) * 1.1).max(1024.0);
                let last = self.system.last().map(|p| p.v);
                let (rx, tx) = (last.map_or_else(none, |v| rate(v[agg::NET_RX])), last.map_or_else(none, |v| rate(v[agg::NET_TX])));
                let max = self.system.pts[self.system.pts.partition_point(|p| p.ts_ms < ctx.start_ms)..]
                    .iter()
                    .map(|p| p.v[agg::NET_RX] + p.v[agg::NET_TX])
                    .fold(0.0, f64::max);
                // Interfaces with traffic first (already sorted), idle ones if room.
                let list = self.settings.stats.then(|| {
                    self.ifaces
                        .iter()
                        .take(STATS_LIST)
                        .map(|(name, rx, tx)| (name.clone(), format!("↓{} ↑{}", fmt_bytes_short(*rx), fmt_bytes_short(*tx)), None))
                        .collect()
                });
                let scope = if is_proc { "system (no per-process data)" } else { "system" };
                Panel {
                    title: chart_title("Net", format!("↓ {rx}  ↑ {tx}"), th.read, scope, None, th),
                    main: a[agg::NET_RX].clone(),
                    color: th.read,
                    extra: vec![(a[agg::NET_TX].clone(), th.write)],
                    legend: vec![("received".into(), th.read), ("sent".into(), th.write)],
                    top: vec![],
                    y_top,
                    y_labels: [0.0, y_top / 2.0, y_top].map(fmt_bytes_short),
                    stats: Some(Stats {
                        rows: vec![("recv", rx), ("sent", tx), ("peak", rate(max))],
                        list_title: "interfaces",
                        list: list.unwrap_or_default(),
                    }),
                }
            }
            PanelKind::Gpu => {
                let (avg, max) = mm(&series.pts, agg::GPU);
                // VRAM as a share of the card, on the same 0–100% axis. GPUs that
                // borrow system RAM (most integrated ones) report no usage; then
                // there's no line rather than a misleading flat zero.
                let vram_total = self.gpu_mem_total as f64;
                let vram_known = vram_total > 0.0 && peak(&agg_v[agg::GPU_MEM]) > 0.0;
                let vram: Xy = if vram_known {
                    agg_v[agg::GPU_MEM].iter().map(|&(x, y)| (x, y / vram_total * 100.0)).collect()
                } else {
                    vec![]
                };
                let vram_now = now_of(agg::GPU_MEM)
                    .filter(|_| vram_known)
                    .map(|m| format!("VRAM {} of {}", fmt_bytes(m), fmt_bytes(vram_total)));
                let ranked = self.ranked(ctx, Measure::One(agg::GPU), 0.1);
                let top = if show_top { self.top_lines(ctx, &ranked, Measure::One(agg::GPU), 1.0, pct, colors) } else { vec![] };
                let list = want_list.then(|| self.list(&ranked, pct, colors));
                let mut legend = legend_of(&top);
                if legend.is_empty() {
                    legend = vec![("usage".into(), th.gpu)];
                    if vram_known {
                        legend.push(("VRAM".into(), th.mem));
                    }
                }
                Panel {
                    title: chart_title("GPU", now_of(agg::GPU).map_or_else(none, pct), th.gpu, &scope, vram_now, th),
                    main: agg_v[agg::GPU].clone(),
                    color: th.gpu,
                    extra: vec![(vram, th.mem)],
                    legend,
                    top,
                    y_top: 100.0,
                    y_labels: [0.0, 50.0, 100.0].map(|v| format!("{v:.0}%")),
                    stats: Some(stats(vec![("now", now_of(agg::GPU).map_or_else(none, pct)), ("avg", pct(avg)), ("peak", pct(max))], list)),
                }
            }
        }
    }

    /// Tracked processes (or apps, when grouping) with data in view, busiest
    /// (by mean) first.
    fn ranked(&self, ctx: &Ctx, m: Measure, min_mean: f64) -> Vec<(TKey, &Tracked, f64)> {
        let mut out: Vec<_> = self
            .tracked
            .iter()
            .filter(|(k, _)| matches!(k, TKey::App(_)) == self.settings.group)
            .filter_map(|(k, t)| {
                let pts = &t.series.pts[t.series.pts.partition_point(|p| p.ts_ms < ctx.start_ms)..];
                if pts.is_empty() {
                    return None;
                }
                let mean = pts.iter().map(|p| m.of(&p.v)).sum::<f64>() / pts.len() as f64;
                (mean >= min_mean && mean > 0.0).then_some((k.clone(), t, mean))
            })
            .collect();
        out.sort_by(|a, b| b.2.total_cmp(&a.2));
        out
    }

    fn color_for(&self, key: TKey, colors: &mut HashMap<TKey, Color>) -> Color {
        let palette = &self.theme.palette;
        let next = palette[colors.len() % palette.len()];
        *colors.entry(key).or_insert(next)
    }

    /// The top-process lines for one panel; `scale` converts the metric to
    /// the chart's units (e.g. one-core % to whole-machine %).
    fn top_lines(
        &self,
        ctx: &Ctx,
        ranked: &[(TKey, &Tracked, f64)],
        m: Measure,
        scale: f64,
        fmt: impl Fn(f64) -> String,
        colors: &mut HashMap<TKey, Color>,
    ) -> Vec<TopLine> {
        ranked
            .iter()
            .take(self.settings.top)
            .map(|(key, t, _)| {
                let agg_t = agg::aggregate(&t.series.pts, ctx.now, ctx.range, ctx.bucket);
                let pts = m.xy(&agg_t).into_iter().map(|(x, y)| (x, y * scale)).collect();
                let last = t.series.last().map_or(0.0, |p| m.of(&p.v) * scale);
                let label = t.label().chars().take(16).collect();
                TopLine { label, value: fmt(last), color: self.color_for(key.clone(), colors), pts }
            })
            .collect()
    }

    /// A stats box's ranked list (by mean in view, showing the latest value);
    /// processes drawn as lines get their line's colour.
    fn list(
        &self,
        ranked: &[(TKey, &Tracked, f64)],
        fmt: impl Fn(f64) -> String,
        colors: &HashMap<TKey, Color>,
    ) -> StatList {
        ranked.iter().take(STATS_LIST).map(|(key, t, mean)| (t.label(), fmt(*mean), colors.get(key).copied())).collect()
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let st = &self.settings;
        let dim = Style::new().fg(th.dim);
        let value = Style::new().fg(th.accent).bold();
        let on_off = |b: bool| (if b { "on" } else { "off" }).to_string();
        let text = |t: &str| vec![Span::raw(t.to_string())];
        // The panel keys as toggles: ● shown, ○ hidden.
        let chips: Vec<Span> = PanelKind::ALL
            .iter()
            .flat_map(|k| {
                let on = st.panels.contains(k);
                let chip = format!("{}{}", if on { "●" } else { "○" }, settings::name(k));
                [Span::styled(chip, if on { Style::new() } else { dim }), Span::raw(" ")]
            })
            .collect();
        let top = if st.top > 0 { st.top.to_string() } else { "off".into() };
        let groups: [HelpGroup; 5] = [
            ("Move", vec![
                ("/", vec![Span::raw("pick a process "), Span::styled("(Del kills)", dim)], String::new()),
                ("Esc", text("back to system-wide"), String::new()),
                ("q", text("quit"), String::new()),
            ]),
            ("Panels", vec![
                ("1-5", chips, String::new()),
                ("l", text("layout"), st.layout.text()),
                ("f", text("focus (wide in odd grid)"), settings::focus_str(st.focus)),
                ("i", text("stats boxes"), on_off(st.stats)),
            ]),
            ("Processes", vec![
                ("t + -", text("top-process lines"), top),
                ("a", text("group by app"), on_off(st.group)),
            ]),
            ("Look", vec![
                ("s", text("graph style"), settings::name(&st.style)),
                ("g", text("characters"), settings::name(&st.charset)),
                ("c", text("theme"), settings::name(&st.theme)),
            ]),
            ("Save", vec![
                ("w", text("save as defaults"), String::new()),
                ("d", text("start history daemon"), String::new()),
            ]),
        ];
        // Blank lines between groups when there's room for them.
        let n_rows: usize = groups.iter().map(|g| g.1.len()).sum();
        let gaps = (area.height as usize) > n_rows + groups.len();
        let mut lines: Vec<Line> = Vec::new();
        for (g, (label, rows)) in groups.into_iter().enumerate() {
            if g > 0 && gaps {
                lines.push(Line::raw(""));
            }
            for (r, (key, what, val)) in rows.into_iter().enumerate() {
                let label = if r == 0 { label } else { "" };
                let pad = 22usize.saturating_sub(what.iter().map(|s| s.width()).sum());
                let mut spans = vec![
                    Span::styled(format!(" {label:<10}"), Style::new().fg(th.accent)),
                    Span::styled(format!("{key:<7}"), Style::new().bold()),
                ];
                spans.extend(what);
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(val, value));
                lines.push(Line::from(spans));
            }
        }
        let width = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 3;
        let [popup] = Layout::horizontal([Constraint::Length(width.min(area.width))]).flex(Flex::Center).areas(area);
        let [popup] =
            Layout::vertical([Constraint::Length((lines.len() as u16 + 2).min(area.height))]).flex(Flex::Center).areas(popup);
        f.render_widget(Clear, popup);
        let block = Block::bordered()
            .border_style(Style::new().fg(th.accent))
            .title(" Keys ".bold())
            .title(Line::styled(" ? or Esc closes ", dim).right_aligned());
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect, bucket: Duration, range: Duration, capped: bool) {
        let th = &self.theme;
        let dim = Style::new().fg(th.dim);
        let key = Style::new().bold();
        let (iv, bk) = (fmt_dur(self.interval), fmt_dur(bucket));
        if self.finished {
            // The frame left on screen after an inline run.
            let ran = fmt_dur(self.started.elapsed());
            let line = Line::from(vec![
                Span::styled(" pmon ", Style::new().reversed().bold()),
                Span::styled(format!(" ran {ran} · every {iv} · bucket {bk} max · last {}", fmt_dur(range)), dim),
            ]);
            f.render_widget(Paragraph::new(line), area);
            return;
        }
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
            warnings.push(format!("only {} of data", fmt_dur(range)));
        }
        warnings.extend(self.note.clone());
        let mut warnings: Vec<Span> = warnings
            .into_iter()
            .flat_map(|w| [Span::styled("│ ", dim), Span::styled(w + " ", Style::new().fg(th.warn))])
            .collect();
        if let Some((msg, at, err)) = &self.flash
            && at.elapsed() < Duration::from_secs(5)
        {
            let color = if *err { th.err } else { th.ok };
            warnings.extend([Span::styled("│ ", dim), Span::styled(format!("{msg} "), Style::new().fg(color))]);
        }
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

/// A help-overlay group: its label and `(key, description, current value)` rows.
type HelpGroup<'a> = (&'static str, Vec<(&'static str, Vec<Span<'a>>, String)>);

/// The time window being drawn.
struct Ctx {
    now: u64,
    range: Duration,
    bucket: Duration,
    start_ms: u64,
}

fn stats(rows: Vec<(&'static str, String)>, list: Option<StatList>) -> Stats {
    Stats { rows, list_title: "top processes", list: list.unwrap_or_default() }
}

fn legend_of(top: &[TopLine]) -> Vec<(String, Color)> {
    top.iter().map(|t| (format!("{} {}", t.label, t.value), t.color)).collect()
}

pub fn chart_title(label: &str, value: String, color: Color, scope: &str, extra: Option<String>, th: &Theme) -> Line<'static> {
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

/// Where locally taken samples go.
struct Sink<'a> {
    system: &'a mut Series,
    mem_total: &'a mut u64,
    gpu_mem_total: &'a mut u64,
    ifaces: &'a mut Vec<(String, f64, f64)>,
    proc: Option<&'a mut ProcView>,
    tracked: &'a mut HashMap<TKey, Tracked>,
}

/// Adds a sample to a tracked series, creating it (named by `name`) if new.
fn track(tracked: &mut HashMap<TKey, Tracked>, key: TKey, name: impl FnOnce() -> String, members: usize, p: Point) {
    let t = tracked.entry(key).or_insert_with(|| Tracked { name: name(), members, series: Series::default() });
    t.members = members;
    t.series.push(p);
}

/// One local sample: system totals, the selected process, and (if tracking)
/// candidates for top-process lines and stats. `own_only` limits those to the
/// current user's processes, matching what the daemon records.
fn sample_live(s: &mut Sampler, sink: &mut Sink, top: bool, own_only: bool) {
    let ts_ms = now_ms();
    let (v, total, gpu_total) = s.system();
    *sink.mem_total = total;
    *sink.gpu_mem_total = gpu_total;
    *sink.ifaces = s.interfaces();
    sink.system.push(Point::new(ts_ms, v));
    let app_scope = sink.proc.as_deref().is_some_and(|pv| pv.is_app());
    if let Some(pv) = sink.proc.as_deref_mut().filter(|pv| !pv.gone && !pv.is_app()) {
        match s.process(pv.key.pid, pv.key.start_time) {
            Some(v) => pv.series.push(Point::new(ts_ms, v)),
            None => pv.gone = true,
        }
    }
    if !top && !app_scope {
        return;
    }
    s.refresh_processes(false);
    let me = if own_only { s.current_user() } else { None };
    let mut all: Vec<(&sysinfo::Process, [f64; METRICS])> =
        s.processes().filter(|p| me.as_ref().is_none_or(|u| owned_by(p, u))).map(|p| (p, s.metrics(p))).collect();
    // Every process counts towards its app's total, not just tracked ones.
    let mut apps: HashMap<String, (usize, [f64; METRICS])> = HashMap::new();
    for (p, v) in &all {
        let e = apps.entry(p.name().to_string_lossy().into_owned()).or_insert((0, [0.0; METRICS]));
        e.0 += 1;
        for (a, b) in e.1.iter_mut().zip(v) {
            *a += b;
        }
    }
    if let Some(pv) = sink.proc.as_deref_mut().filter(|pv| pv.is_app()) {
        let (n, v) = apps.get(&pv.key.name).copied().unwrap_or((0, [0.0; METRICS]));
        pv.members = n;
        pv.gone = n == 0;
        if n > 0 {
            pv.series.push(Point::new(ts_ms, v));
        }
    }
    if !top {
        return;
    }
    // Keep the busiest few processes and apps by each measure panels rank by.
    let measures = [Measure::One(agg::CPU), Measure::One(agg::MEM), Measure::Sum(agg::DISK_R, agg::DISK_W), Measure::One(agg::GPU)];
    let mut keep = HashSet::new();
    let mut keep_apps = HashSet::new();
    let mut app_list: Vec<(&String, &(usize, [f64; METRICS]))> = apps.iter().collect();
    for m in measures {
        all.sort_by(|a, b| m.of(&b.1).total_cmp(&m.of(&a.1)));
        keep.extend(all.iter().take(TRACK).filter(|(_, v)| m.of(v) > 0.0).map(|(p, _)| p.pid()));
        app_list.sort_by(|a, b| m.of(&b.1.1).total_cmp(&m.of(&a.1.1)));
        keep_apps.extend(app_list.iter().take(TRACK).filter(|(_, (_, v))| m.of(v) > 0.0).map(|(name, _)| (*name).clone()));
    }
    for (p, v) in all.into_iter().filter(|(p, _)| keep.contains(&p.pid())) {
        let key = TKey::Proc(p.pid().as_u32(), p.start_time());
        track(sink.tracked, key, || p.name().to_string_lossy().into_owned(), 1, Point::new(ts_ms, v));
    }
    for name in keep_apps {
        let (n, v) = apps[&name];
        track(sink.tracked, TKey::App(name.clone()), || name, n, Point::new(ts_ms, v));
    }
}

/// Reads the scoped process's (or app's) records from `from` on.
fn read_proc(ring: &Ring<ProcRec>, from: u64, pv: &mut ProcView) -> std::io::Result<u64> {
    if !pv.is_app() {
        let key = &pv.key;
        let series = &mut pv.series;
        return ring.read_from(from, |r| {
            if r.pid == key.pid && r.start_time == key.start_time {
                series.push(Point::new(r.ts_ms, r.v));
            }
        });
    }
    // An app: sum its processes' records per sample.
    let mut cur: Option<(u64, usize, [f64; METRICS])> = None;
    let mut done = Vec::new();
    let end = ring.read_from(from, |r| {
        if r.name_str() != pv.key.name {
            return;
        }
        match &mut cur {
            Some((ts, n, v)) if *ts == r.ts_ms => {
                *n += 1;
                for (a, b) in v.iter_mut().zip(&r.v) {
                    *a += b;
                }
            }
            _ => {
                done.extend(cur.take());
                cur = Some((r.ts_ms, 1, r.v));
            }
        }
    })?;
    done.extend(cur);
    for (ts, n, v) in done {
        pv.members = n;
        pv.series.push(Point::new(ts, v));
    }
    Ok(end)
}
