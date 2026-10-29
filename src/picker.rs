//! Fuzzy process picker (nucleo).

use crate::util::fmt_bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
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

fn fuzzy(items: &[PickItem], query: &str, matcher: &mut Matcher) -> Vec<usize> {
    let pat = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let hay = items.iter().enumerate().map(|(i, it)| Hay(i, &it.haystack));
    pat.match_list(hay, matcher).into_iter().map(|(h, _)| h.0).collect()
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
    let exact: Vec<usize> = (0..items.len()).filter(|&i| items[i].key.name == query).collect();
    if !exact.is_empty() {
        return single(items, &exact);
    }
    single(items, &fuzzy(items, query, &mut Matcher::new(Config::DEFAULT)))
}

pub enum Action {
    None,
    Cancel,
    Select(ProcKey),
}

pub struct Picker {
    items: Vec<PickItem>,
    query: String,
    matches: Vec<usize>,
    state: TableState,
    matcher: Matcher,
    info_label: &'static str,
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

    pub fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Enter => {
                if let Some(&i) = self.state.selected().and_then(|s| self.matches.get(s)) {
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

    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        let popup = if area.width > 40 && area.height > 12 { area.inner(Margin::new(2, 1)) } else { area };
        f.render_widget(Clear, popup);
        let block = Block::bordered()
            .border_style(Style::new().fg(Color::Yellow))
            .title(" Select process ".bold())
            .title(Line::from(format!(" {}/{} ", self.matches.len(), self.items.len())).right_aligned())
            .title_bottom(" ↑↓ move · Enter select · Esc cancel ".dark_gray());
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
        let header = Row::new(["PID", "NAME", "CPU%", "MEM", self.info_label]).style(Style::new().bold().fg(Color::DarkGray));
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(Style::new().reversed())
            .highlight_symbol("▌");
        f.render_stateful_widget(table, list, &mut self.state);
    }
}
