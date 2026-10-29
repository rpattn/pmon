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

#[derive(Clone, Debug, PartialEq)]
pub struct ProcKey {
    pub pid: u32,
    pub start_time: u64,
    pub name: String,
}

pub struct PickItem {
    pub key: ProcKey,
    pub cpu: f32,
    pub rss: u64,
    /// Command line (live) or last-seen info (history).
    pub info: String,
    pub alive: bool,
    haystack: String,
}

impl PickItem {
    pub fn new(key: ProcKey, cpu: f32, rss: u64, info: String, alive: bool) -> Self {
        let haystack = format!("{} {} {}", key.name, key.pid, info);
        PickItem { key, cpu, rss, info, alive, haystack }
    }
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
    // Stable sort keeps the original (busiest first) order among ties.
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
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
    if !exact.is_empty() {
        return single(items, &exact);
    }
    single(items, &fuzzy(items, query, &mut Matcher::new(Config::DEFAULT)))
}

pub enum Action {
    None,
    Cancel,
    Select(ProcKey),
    /// Kill this process; `true` = force (SIGKILL; same as plain on Windows).
    Kill(ProcKey, bool),
}

pub struct Picker {
    items: Vec<PickItem>,
    query: String,
    matches: Vec<usize>,
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
            state: TableState::default(),
            matcher: Matcher::new(Config::DEFAULT),
            info_label,
            confirm: None,
            message: None,
        };
        p.refilter();
        p
    }

    fn refilter(&mut self) {
        self.matches = if self.query.trim().is_empty() {
            (0..self.items.len()).collect()
        } else {
            fuzzy(&self.items, &self.query, &mut self.matcher)
        };
        self.state.select((!self.matches.is_empty()).then_some(0));
    }

    fn step(&mut self, delta: isize) {
        if let Some(i) = self.state.selected() {
            let last = self.matches.len().saturating_sub(1) as isize;
            self.state.select(Some((i as isize + delta).clamp(0, last) as usize));
        }
    }

    fn selected(&self) -> Option<usize> {
        self.state.selected().and_then(|s| self.matches.get(s)).copied()
    }

    /// Reports the result of an `Action::Kill`. A process that exited leaves
    /// the list; one that is still running stays selected so Delete, `f` can
    /// force it.
    pub fn kill_result(&mut self, key: &ProcKey, force: bool, res: Result<bool, String>) {
        let label = format!("{} [{}]", key.name, key.pid);
        self.message = Some(match res {
            Ok(true) => {
                let sel = self.state.selected().unwrap_or(0);
                self.items.retain(|it| it.key != *key);
                self.refilter();
                if !self.matches.is_empty() {
                    self.state.select(Some(sel.min(self.matches.len() - 1)));
                }
                (format!("killed {label}"), false)
            }
            Ok(false) if cfg!(unix) && !force => (format!("sent SIGTERM to {label}, still running: Del f to force"), true),
            Ok(false) => (format!("{label} is still running"), true),
            Err(e) => (format!("{label}: {e}"), true),
        });
    }

    pub fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.message = None;
        if let Some(i) = self.confirm.take() {
            return match k.code {
                KeyCode::Enter => Action::Kill(self.items[i].key.clone(), false),
                KeyCode::Char('f') | KeyCode::Char('F') => Action::Kill(self.items[i].key.clone(), true),
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
            let key = &self.items[i].key;
            let name: String = key.name.chars().take(24).collect();
            let force = if cfg!(unix) { " · f force" } else { "" };
            return Line::from(vec![
                Span::styled(format!(" Kill {name} [{}]? ", key.pid), Style::new().fg(th.err).bold()),
                format!("Enter kill{force} · other key cancels ").into(),
            ]);
        }
        match &self.message {
            Some((m, true)) => Span::styled(format!(" {m} "), Style::new().fg(th.err)).into(),
            Some((m, false)) => Span::styled(format!(" {m} "), Style::new().fg(th.ok)).into(),
            None => Span::styled(" ↑↓ move · Enter select · Del kill · Esc cancel ", Style::new().fg(th.dim)).into(),
        }
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, th: &Theme) {
        let popup = if area.width > 40 && area.height > 12 { area.inner(Margin::new(2, 1)) } else { area };
        f.render_widget(Clear, popup);
        let block = Block::bordered()
            .border_style(Style::new().fg(th.accent))
            .title(" Select process ".bold())
            .title(Line::from(format!(" {}/{} ", self.matches.len(), self.items.len())).right_aligned())
            .title_bottom(self.footer(th));
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        let [input, list] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

        f.render_widget(Paragraph::new(Line::from(vec!["> ".yellow().bold(), self.query.as_str().into()])), input);
        let cx = input.x + 2 + self.query.chars().count() as u16;
        f.set_cursor_position((cx.min(input.right().saturating_sub(1)), input.y));

        let rows = self.matches.iter().map(|&i| {
            let it = &self.items[i];
            Row::new(vec![
                it.key.pid.to_string(),
                it.key.name.clone(),
                format!("{:.1}", it.cpu),
                fmt_bytes(it.rss as f64),
                it.info.clone(),
            ])
        });
        let widths = [
            Constraint::Length(7),
            Constraint::Length(16),
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
            item("zzstubborn", 2, "/tmp/claude-1000/-home-rpattn-dev-pmon/e8ad6d4b-9c62-4169-8b8d-14603b2c0cd0/scratchpad/zzstubborn -c trap \"\" TERM; while :; do sleep 1; done"),
        ];
        let m = fuzzy(&items, "zzstubborn", &mut Matcher::new(Config::DEFAULT));
        assert_eq!(m, vec![1, 0]);
    }
}
