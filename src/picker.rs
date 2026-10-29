//! Fuzzy process picker (nucleo).

use crate::theme::Theme;
use crate::util::fmt_bytes;
use std::collections::HashMap;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Row, Table, TableState};

/// A process (pid + start time, so a reused pid never matches), or an app:
/// every process with this name (see [`ProcKey::app`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcKey {
    pub pid: u32,
    pub start_time: u64,
    pub name: String,
}

impl ProcKey {
    /// All processes named `name`. (pid 0 is a real process on Windows, so
    /// the marker is the impossible start time.)
    pub fn app(name: &str) -> Self {
        ProcKey { pid: 0, start_time: u64::MAX, name: name.to_string() }
    }
    pub fn is_app(&self) -> bool {
        self.start_time == u64::MAX
    }
}

pub struct PickItem {
    pub key: ProcKey,
    pub cpu: f32,
    pub rss: u64,
    /// Command line (live) or last-seen info (history).
    pub info: String,
    pub alive: bool,
    /// For an app row: how many processes it combines (0 for a process).
    pub members: usize,
    haystack: String,
}

impl PickItem {
    pub fn new(key: ProcKey, cpu: f32, rss: u64, info: String, alive: bool) -> Self {
        let haystack = format!("{} {} {}", key.name, key.pid, info);
        PickItem { key, cpu, rss, info, alive, members: 0, haystack }
    }
}

/// Adds an app row for every name shared by two or more processes, summing
/// their CPU and memory.
pub fn add_apps(items: &mut Vec<PickItem>) {
    let mut by_name: HashMap<String, (usize, f32, u64, bool)> = HashMap::new();
    for it in items.iter() {
        let e = by_name.entry(it.key.name.clone()).or_default();
        e.0 += 1;
        e.1 += it.cpu;
        e.2 += it.rss;
        e.3 |= it.alive;
    }
    let mut apps: Vec<PickItem> = by_name
        .into_iter()
        .filter(|(_, (n, ..))| *n > 1)
        .map(|(name, (n, cpu, rss, alive))| {
            let mut it = PickItem::new(ProcKey::app(&name), cpu, rss, format!("{n} processes"), alive);
            it.members = n;
            it.haystack = name;
            it
        })
        .collect();
    apps.sort_by(|a, b| a.key.name.cmp(&b.key.name));
    items.extend(apps);
}

struct Hay<'a>(usize, &'a str);
impl AsRef<str> for Hay<'_> {
    fn as_ref(&self) -> &str {
        self.1
    }
}

/// Matches against name + pid + command line, but anything whose *name*
/// matches ranks above matches found only in the command line (otherwise a
/// shell whose arguments mention `foo` can outrank `foo` itself).
fn fuzzy(items: &[PickItem], query: &str, matcher: &mut Matcher) -> Vec<usize> {
    let pat = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let by_name: HashMap<usize, u32> = pat
        .match_list(items.iter().enumerate().map(|(i, it)| Hay(i, &it.key.name)), matcher)
        .into_iter()
        .map(|(h, s)| (h.0, s))
        .collect();
    let mut hits: Vec<(usize, bool, u32)> = pat
        .match_list(items.iter().enumerate().map(|(i, it)| Hay(i, &it.haystack)), matcher)
        .into_iter()
        .map(|(h, s)| (h.0, by_name.contains_key(&h.0), by_name.get(&h.0).copied().unwrap_or(s)))
        .collect();
    // An app ranks above its own processes; otherwise the stable sort keeps
    // the original (busiest first) order among ties.
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(items[b.0].key.is_app().cmp(&items[a.0].key.is_app())));
    hits.into_iter().map(|h| h.0).collect()
}

/// Of several candidates, the only one still running (history mode).
fn single(items: &[PickItem], cands: &[usize]) -> Option<usize> {
    match cands {
        [i] => Some(*i),
        _ => match cands.iter().filter(|&&i| items[i].alive).collect::<Vec<_>>()[..] {
            [i] => Some(*i),
            _ => None,
        },
    }
}

/// Picks a process without the UI when `query` is unambiguous: a pid, a
/// unique exact name, or a single fuzzy match (preferring the one running
/// process when history also holds exited ones).
pub fn resolve(items: &[PickItem], query: &str) -> Option<usize> {
    if let Ok(pid) = query.parse::<u32>() {
        // Items are ordered most relevant first, so for a reused pid this is the latest.
        if let Some(i) = items.iter().position(|it| it.key.pid == pid) {
            return Some(i);
        }
    }
    // `notepad` also matches `notepad.exe`.
    let bare = |n: &str| n.strip_suffix(".exe").unwrap_or(n).to_string();
    let exact: Vec<usize> =
        (0..items.len()).filter(|&i| items[i].key.name == query || bare(&items[i].key.name) == query).collect();
    // Several processes with that name: the app, if there is one.
    if let Some(&i) = exact.iter().find(|&&i| items[i].key.is_app()) {
        return Some(i);
    }
    if !exact.is_empty() {
        return single(items, &exact);
    }
    single(items, &fuzzy(items, query, &mut Matcher::new(Config::DEFAULT)))
}

pub enum Action {
    None,
    Cancel,
    Select(ProcKey),
    /// Kill these processes (one, or all of an app's); `true` = force
    /// (SIGKILL; same as plain on Windows).
    Kill(Vec<ProcKey>, bool),
}

/// A visible row: an item, and whether it's shown inside an expanded app.
#[derive(Clone, Copy)]
struct ViewRow {
    item: usize,
    child: bool,
}

pub struct Picker {
    items: Vec<PickItem>,
    query: String,
    matches: Vec<ViewRow>,
    /// App name -> indices of its processes.
    groups: HashMap<String, Vec<usize>>,
    expanded: std::collections::HashSet<String>,
    state: TableState,
    matcher: Matcher,
    info_label: &'static str,
    /// Item awaiting kill confirmation.
    confirm: Option<usize>,
    /// Outcome of the last kill: `(text, is_error)`.
    message: Option<(String, bool)>,
}

impl Picker {
    pub fn new(items: Vec<PickItem>, query: String, info_label: &'static str) -> Self {
        let mut p = Picker {
            items,
            query,
            matches: vec![],
            groups: HashMap::new(),
            expanded: Default::default(),
            state: TableState::default(),
            matcher: Matcher::new(Config::DEFAULT),
            info_label,
            confirm: None,
            message: None,
        };
        let apps: std::collections::HashSet<String> =
            p.items.iter().filter(|it| it.key.is_app()).map(|it| it.key.name.clone()).collect();
        for (i, it) in p.items.iter().enumerate() {
            if !it.key.is_app() && apps.contains(&it.key.name) {
                p.groups.entry(it.key.name.clone()).or_default().push(i);
            }
        }
        p.refilter();
        p
    }

    /// Rebuilds the visible rows. With no query: a tree, each app where its
    /// busiest process was, its processes listed below it when expanded.
    /// With a query: flat fuzzy matches (apps included).
    fn rebuild(&mut self) {
        self.matches = if self.query.trim().is_empty() {
            let app_row: HashMap<&str, usize> =
                self.items.iter().enumerate().filter(|(_, it)| it.key.is_app()).map(|(i, it)| (it.key.name.as_str(), i)).collect();
            let mut rows = Vec::new();
            let mut placed = std::collections::HashSet::new();
            for (i, it) in self.items.iter().enumerate().filter(|(_, it)| !it.key.is_app()) {
                match app_row.get(it.key.name.as_str()) {
                    Some(&g) => {
                        if placed.insert(g) {
                            rows.push(ViewRow { item: g, child: false });
                            if self.expanded.contains(&it.key.name) {
                                let members = &self.groups[&it.key.name];
                                rows.extend(members.iter().map(|&m| ViewRow { item: m, child: true }));
                            }
                        }
                    }
                    None => rows.push(ViewRow { item: i, child: false }),
                }
            }
            rows
        } else {
            fuzzy(&self.items, &self.query, &mut self.matcher).into_iter().map(|item| ViewRow { item, child: false }).collect()
        };
    }

    fn refilter(&mut self) {
        self.rebuild();
        self.state.select((!self.matches.is_empty()).then_some(0));
    }

    /// Expands or collapses the app under (or containing) the selection,
    /// keeping the app's row selected.
    fn set_expanded(&mut self, open: bool) {
        let Some(row) = self.state.selected().and_then(|s| self.matches.get(s)).copied() else { return };
        let name = self.items[row.item].key.name.clone();
        if !self.groups.contains_key(&name) || (!row.child && !self.items[row.item].key.is_app()) {
            return;
        }
        if open {
            self.expanded.insert(name.clone());
        } else {
            self.expanded.remove(&name);
        }
        self.rebuild();
        let at = self.matches.iter().position(|r| !r.child && self.items[r.item].key.is_app() && self.items[r.item].key.name == name);
        self.state.select(at);
    }

    fn step(&mut self, delta: isize) {
        if let Some(i) = self.state.selected() {
            let last = self.matches.len().saturating_sub(1) as isize;
            self.state.select(Some((i as isize + delta).clamp(0, last) as usize));
        }
    }

    fn selected(&self) -> Option<usize> {
        self.state.selected().and_then(|s| self.matches.get(s)).map(|r| r.item)
    }

    /// The processes behind an item: itself, or an app's members.
    fn targets(&self, i: usize) -> Vec<ProcKey> {
        match self.groups.get(&self.items[i].key.name).filter(|_| self.items[i].key.is_app()) {
            Some(members) => members.iter().map(|&m| self.items[m].key.clone()).collect(),
            None => vec![self.items[i].key.clone()],
        }
    }

    /// Reports the result of an `Action::Kill`. A process that exited leaves
    /// the list; one that is still running stays selected so Delete, `f` can
    /// force it.
    pub fn kill_result(&mut self, keys: &[ProcKey], force: bool, results: Vec<Result<bool, String>>) {
        let label = match keys {
            [k] => format!("{} [{}]", k.name, k.pid),
            [k, ..] => format!("{} ×{}", k.name, keys.len()),
            [] => return,
        };
        let gone: Vec<&ProcKey> = keys.iter().zip(&results).filter(|(_, r)| matches!(r, Ok(true))).map(|(k, _)| k).collect();
        let running = results.iter().filter(|r| matches!(r, Ok(false))).count();
        let errors: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
        if !gone.is_empty() {
            // Drop exited processes, and an app once none of it is left.
            let sel = self.state.selected().unwrap_or(0);
            let keep: Vec<bool> = self.items.iter().map(|it| !gone.contains(&&it.key)).collect();
            let mut idx = 0;
            self.items.retain(|_| {
                idx += 1;
                keep[idx - 1]
            });
            let names: std::collections::HashSet<String> =
                self.items.iter().filter(|it| !it.key.is_app()).map(|it| it.key.name.clone()).collect();
            self.items.retain(|it| !it.key.is_app() || names.contains(&it.key.name));
            self.groups.clear();
            let apps: std::collections::HashSet<String> =
                self.items.iter().filter(|it| it.key.is_app()).map(|it| it.key.name.clone()).collect();
            for (i, it) in self.items.iter().enumerate() {
                if !it.key.is_app() && apps.contains(&it.key.name) {
                    self.groups.entry(it.key.name.clone()).or_default().push(i);
                }
            }
            self.rebuild();
            if !self.matches.is_empty() {
                self.state.select(Some(sel.min(self.matches.len() - 1)));
            }
        }
        self.message = Some(match (gone.len(), running, errors.first()) {
            (_, 0, None) => (format!("killed {label}"), false),
            (_, _, Some(e)) if keys.len() == 1 => (format!("{label}: {e}"), true),
            (_, 0, Some(e)) => (format!("{label}: {} killed, {} failed ({e})", gone.len(), errors.len()), true),
            _ if cfg!(unix) && !force => (format!("sent SIGTERM to {label}, {running} still running: Del f to force"), true),
            _ => (format!("{label}: {running} still running"), true),
        });
    }

    pub fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.message = None;
        if let Some(i) = self.confirm.take() {
            return match k.code {
                KeyCode::Enter => Action::Kill(self.targets(i), false),
                KeyCode::Char('f') | KeyCode::Char('F') => Action::Kill(self.targets(i), true),
                _ => Action::None, // anything else cancels
            };
        }
        match k.code {
            KeyCode::Delete => self.confirm = self.selected(),
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Enter => {
                if let Some(i) = self.selected() {
                    return Action::Select(self.items[i].key.clone());
                }
            }
            KeyCode::Right => self.set_expanded(true),
            KeyCode::Left => self.set_expanded(false),
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-10),
            KeyCode::PageDown => self.step(10),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => {
                let t = self.query.trim_end().len();
                let cut = self.query[..t].rfind(' ').map_or(0, |i| i + 1);
                self.query.truncate(cut);
                self.refilter();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        Action::None
    }

    fn footer(&self, th: &Theme) -> Line<'static> {
        if let Some(i) = self.confirm {
            let it = &self.items[i];
            let name: String = it.key.name.chars().take(24).collect();
            let what = if it.key.is_app() { format!("all {} {name} processes", it.members) } else { format!("{name} [{}]", it.key.pid) };
            let force = if cfg!(unix) { " · f force" } else { "" };
            return Line::from(vec![
                Span::styled(format!(" Kill {what}? "), Style::new().fg(th.err).bold()),
                format!("Enter kill{force} · other key cancels ").into(),
            ]);
        }
        match &self.message {
            Some((m, true)) => Span::styled(format!(" {m} "), Style::new().fg(th.err)).into(),
            Some((m, false)) => Span::styled(format!(" {m} "), Style::new().fg(th.ok)).into(),
            None if self.groups.is_empty() => {
                Span::styled(" ↑↓ move · Enter select · Del kill · Esc cancel ", Style::new().fg(th.dim)).into()
            }
            None => Span::styled(" ↑↓ move · →← expand app · Enter select · Del kill · Esc cancel ", Style::new().fg(th.dim)).into(),
        }
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, th: &Theme) {
        let popup = if area.width > 40 && area.height > 12 { area.inner(Margin::new(2, 1)) } else { area };
        f.render_widget(Clear, popup);
        let block = Block::bordered()
            .border_style(Style::new().fg(th.accent))
            .title(" Select process ".bold())
            .title(Line::from(format!(" {} processes ", self.items.iter().filter(|it| !it.key.is_app()).count())).right_aligned())
            .title_bottom(self.footer(th));
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        let [input, list] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

        f.render_widget(Paragraph::new(Line::from(vec!["> ".yellow().bold(), self.query.as_str().into()])), input);
        let cx = input.x + 2 + self.query.chars().count() as u16;
        f.set_cursor_position((cx.min(input.right().saturating_sub(1)), input.y));

        let rows = self.matches.iter().map(|row| {
            let it = &self.items[row.item];
            let (pid, name) = if it.key.is_app() {
                let arrow = if self.expanded.contains(&it.key.name) && self.query.trim().is_empty() { "▾" } else { "▸" };
                (arrow.to_string(), format!("{} ×{}", it.key.name, it.members))
            } else if row.child {
                (it.key.pid.to_string(), format!(" └ {}", it.key.name))
            } else {
                (it.key.pid.to_string(), it.key.name.clone())
            };
            let style = if it.key.is_app() { Style::new().bold() } else { Style::new() };
            Row::new(vec![pid, name, format!("{:.1}", it.cpu), fmt_bytes(it.rss as f64), it.info.clone()]).style(style)
        });
        let widths = [
            Constraint::Length(7),
            Constraint::Length(20),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Fill(1),
        ];
        let header = Row::new(["PID", "NAME", "CPU%", "MEM", self.info_label]).style(Style::new().bold().fg(th.dim));
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("▌");
        f.render_stateful_widget(table, list, &mut self.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, pid: u32, info: &str) -> PickItem {
        PickItem::new(ProcKey { pid, start_time: 0, name: name.into() }, 0.0, 0, info.into(), true)
    }

    #[test]
    fn name_matches_rank_first() {
        let items = vec![
            item("bash", 1, "/bin/bash -c run zzstubborn now"),
            item("zzstubborn", 2, "/tmp/test/zzstubborn -c trap \"\" TERM; while :; do sleep 1; done"),
        ];
        let m = fuzzy(&items, "zzstubborn", &mut Matcher::new(Config::DEFAULT));
        assert_eq!(m, vec![1, 0]);
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    fn item(name: &str, pid: u32, cpu: f32) -> PickItem {
        PickItem::new(ProcKey { pid, start_time: 1, name: name.into() }, cpu, 100, String::new(), true)
    }

    #[test]
    fn apps_group_and_expand() {
        let mut items = vec![item("chrome", 10, 5.0), item("bash", 20, 3.0), item("chrome", 11, 1.0)];
        add_apps(&mut items);
        let app = items.iter().find(|it| it.key.is_app()).unwrap();
        assert_eq!((app.key.name.as_str(), app.members, app.cpu, app.rss), ("chrome", 2, 6.0, 200));
        // `-p chrome` picks the app rather than being ambiguous.
        assert!(items[resolve(&items, "chrome").unwrap()].key.is_app());

        let mut p = Picker::new(items, String::new(), "COMMAND");
        let names = |p: &Picker| p.matches.iter().map(|r| p.items[r.item].key.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&p), ["chrome", "bash"]); // the app sits where its busiest process was
        p.set_expanded(true);
        assert_eq!(names(&p), ["chrome", "chrome", "chrome", "bash"]);
        assert_eq!(p.targets(p.selected().unwrap()).len(), 2);
        p.set_expanded(false);
        assert_eq!(names(&p).len(), 2);
        // Searching puts the app above its processes.
        p.query = "chrome".into();
        p.refilter();
        assert!(p.items[p.selected().unwrap()].key.is_app());
    }
}
