//! Display settings: graph style, character set, theme and top-process lines.
//! Defaults come from a small TOML-style config file and can be overridden on
//! the command line or changed at runtime (and saved back with `w`).

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use ratatui::symbols::Marker;
use ratatui::widgets::GraphType;
use std::path::PathBuf;

pub const DEFAULT_TOP: usize = 5;
pub const MAX_TOP: usize = 8;
/// Rows used by `--inline` when no height is given.
pub const DEFAULT_INLINE: u16 = 24;

/// The charts pmon can show, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum)]
pub enum PanelKind {
    Cpu,
    #[value(alias = "mem")]
    Ram,
    Disk,
    #[value(alias = "network")]
    Net,
    Gpu,
}

impl PanelKind {
    pub const ALL: [PanelKind; 5] = [PanelKind::Cpu, PanelKind::Ram, PanelKind::Disk, PanelKind::Net, PanelKind::Gpu];
}

/// How panels are arranged: a preset, or rows of panels from a template
/// such as `cpu / ram disk / net gpu`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arrangement {
    Preset(LayoutKind),
    Rows(Vec<Vec<PanelKind>>),
}

impl Arrangement {
    /// The config-file spelling: a preset name or the template.
    pub fn text(&self) -> String {
        match self {
            Arrangement::Preset(k) => name(k),
            Arrangement::Rows(rows) => {
                rows.iter().map(|r| r.iter().map(name).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(" / ")
            }
        }
    }

    /// The next (or previous) choice: the presets, plus `custom` if there is one.
    pub fn step(&self, custom: Option<&Arrangement>, by: isize) -> Arrangement {
        let mut all: Vec<Arrangement> = custom.into_iter().cloned().collect();
        all.extend(LayoutKind::value_variants().iter().map(|k| Arrangement::Preset(*k)));
        let i = all.iter().position(|a| a == self).unwrap_or(0) as isize;
        all[(i + by).rem_euclid(all.len() as isize) as usize].clone()
    }
}

/// A preset name, or a template: rows split by `/`, panels in a row split by
/// spaces or commas, each panel at most once.
pub fn parse_layout(v: &str) -> Result<Arrangement> {
    if let Ok(k) = LayoutKind::from_str(v.trim(), true) {
        return Ok(Arrangement::Preset(k));
    }
    let mut seen = Vec::new();
    let mut rows = Vec::new();
    for row in v.split('/') {
        let mut panels = Vec::new();
        for word in row.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()) {
            let p: PanelKind = parse_enum("panel", word)?;
            if seen.contains(&p) {
                bail!("`{word}` appears twice in the layout");
            }
            seen.push(p);
            panels.push(p);
        }
        if !panels.is_empty() {
            rows.push(panels);
        }
    }
    if rows.is_empty() {
        bail!("layout must be auto, rows, columns, grid, or rows of panels like \"cpu / ram disk / net gpu\"");
    }
    Ok(Arrangement::Rows(rows))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LayoutKind {
    /// Stacked when there's room, otherwise a grid.
    Auto,
    /// One above the other.
    Rows,
    /// Side by side.
    Columns,
    /// Two columns.
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GraphStyle {
    /// A line through the samples.
    Line,
    /// A line with the area underneath filled.
    Area,
    /// A vertical bar per bucket.
    Bars,
    /// Just the points.
    Dots,
}

impl GraphStyle {
    pub fn graph_type(self) -> GraphType {
        match self {
            GraphStyle::Line => GraphType::Line,
            GraphStyle::Area => GraphType::Area,
            GraphStyle::Bars => GraphType::Bar,
            GraphStyle::Dots => GraphType::Scatter,
        }
    }
}

/// Characters used to draw the plot. Higher resolution first; the last three
/// work with any font.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Charset {
    /// Braille dots, 2x4 per cell.
    Braille,
    /// Solid octants, 2x4 per cell (needs a recent font).
    Octant,
    /// Solid sextants, 2x3 per cell (needs a recent font).
    Sextant,
    /// Quadrant blocks, 2x2 per cell.
    Quadrant,
    /// Half blocks, 1x2 per cell.
    #[value(name = "halfblock")]
    HalfBlock,
    /// Full blocks, one per cell.
    Block,
    /// Bullets, one per cell.
    Dot,
    /// `*`, one per cell.
    Ascii,
}

impl Charset {
    pub fn marker(self) -> Marker {
        match self {
            Charset::Braille => Marker::Braille,
            Charset::Octant => Marker::Octant,
            Charset::Sextant => Marker::Sextant,
            Charset::Quadrant => Marker::Quadrant,
            Charset::HalfBlock => Marker::HalfBlock,
            Charset::Block => Marker::Block,
            Charset::Dot => Marker::Dot,
            Charset::Ascii => Marker::Custom('*'),
        }
    }

    /// Resolution of one terminal cell as (columns, rows) of dots.
    pub fn dots_per_cell(self) -> (usize, usize) {
        match self {
            Charset::Braille | Charset::Octant => (2, 4),
            Charset::Sextant => (2, 3),
            Charset::Quadrant => (2, 2),
            Charset::HalfBlock => (1, 2),
            Charset::Block | Charset::Dot | Charset::Ascii => (1, 1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ThemeName {
    /// The terminal's own palette.
    Default,
    Nord,
    Gruvbox,
    Dracula,
    Solarized,
    /// For light terminal backgrounds.
    Light,
    /// No colours (also chosen by the NO_COLOR environment variable).
    Mono,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub style: GraphStyle,
    pub charset: Charset,
    pub theme: ThemeName,
    /// Top-process lines in the system view; 0 = off.
    pub top: usize,
    /// Panels to show (in `PanelKind` order).
    pub panels: Vec<PanelKind>,
    pub layout: Arrangement,
    /// In a grid with an odd number of panels, the one given the full-width
    /// top row (`None`: the last panel spans the bottom row instead).
    pub focus: Option<PanelKind>,
    /// A stats box (now/avg/peak and top consumers) beside each chart.
    pub stats: bool,
    /// Draw below the prompt in this many rows and leave the last frame on
    /// screen after exiting, instead of taking over the whole terminal.
    pub inline: Option<u16>,
    /// Combine processes with the same name (a browser's many processes)
    /// into one app in lists, top lines and the picker.
    pub group: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        Settings {
            style: GraphStyle::Line,
            charset: Charset::Braille,
            theme: if no_color { ThemeName::Mono } else { ThemeName::Default },
            top: 0,
            panels: PanelKind::ALL.to_vec(),
            layout: Arrangement::Preset(LayoutKind::Auto),
            focus: Some(PanelKind::Cpu),
            stats: true,
            inline: None,
            group: true,
        }
    }
}

pub fn name<T: ValueEnum>(v: &T) -> String {
    v.to_possible_value().map(|p| p.get_name().to_string()).unwrap_or_default()
}

/// The variant after `v`, wrapping around.
pub fn next<T: ValueEnum + PartialEq + Clone>(v: &T) -> T {
    step(v, 1)
}

/// The variant `by` places after (or before, if negative) `v`, wrapping around.
pub fn step<T: ValueEnum + PartialEq + Clone>(v: &T, by: isize) -> T {
    let all = T::value_variants();
    let i = all.iter().position(|x| x == v).unwrap_or(0) as isize;
    all[(i + by).rem_euclid(all.len() as isize) as usize].clone()
}

/// The one-line description of a value, if it has one.
pub fn help<T: ValueEnum>(v: &T) -> String {
    v.to_possible_value().and_then(|p| p.get_help().map(|h| h.to_string())).unwrap_or_default()
}

/// `%APPDATA%\pmon\config.toml` on Windows, else
/// `$XDG_CONFIG_HOME/pmon/config.toml` (default `~/.config/pmon/config.toml`).
pub fn config_path() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into())).join(".config")
    });
    base.join("pmon").join("config.toml")
}

/// A panel name, or `none`.
pub fn parse_focus(v: &str) -> Result<Option<PanelKind>> {
    if v.trim().eq_ignore_ascii_case("none") { Ok(None) } else { parse_enum("focus panel", v.trim()).map(Some) }
}

pub fn focus_str(f: Option<PanelKind>) -> String {
    f.map_or("none".into(), |k| name(&k))
}

/// A comma-separated list of panel names; empty means none.
pub fn parse_panels(v: &str) -> Result<Vec<PanelKind>> {
    let mut out = v
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| parse_enum::<PanelKind>("panel", p))
        .collect::<Result<Vec<_>>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn panels_str(panels: &[PanelKind]) -> String {
    panels.iter().map(name).collect::<Vec<_>>().join(",")
}

pub fn parse_bool(key: &str, v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("{key} must be true or false"),
    }
}

/// `false`, `true` (default height) or a number of rows.
pub fn parse_inline(v: &str) -> Result<Option<u16>> {
    match v.to_ascii_lowercase().as_str() {
        "false" | "no" | "off" | "0" => Ok(None),
        "true" | "yes" | "on" => Ok(Some(DEFAULT_INLINE)),
        n => n
            .parse::<u16>()
            .map(|n| Some(n.max(8)))
            .map_err(|_| anyhow::anyhow!("inline must be true, false or a number of rows")),
    }
}

fn parse_enum<T: ValueEnum>(key: &str, v: &str) -> Result<T> {
    T::from_str(v, true).map_err(|_| {
        let names: Vec<String> = T::value_variants().iter().map(name).collect();
        anyhow::anyhow!("unknown {key} `{v}` (expected one of: {})", names.join(", "))
    })
}

impl Settings {
    /// Defaults overlaid with the config file, if there is one.
    pub fn load() -> Result<Self> {
        let path = config_path();
        let mut s = Settings::default();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(s),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        s.apply(&text).with_context(|| format!("in {}", path.display()))?;
        Ok(s)
    }

    fn apply(&mut self, text: &str) -> Result<()> {
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!("line {}: expected `key = value`", n + 1);
            };
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            let res = match key {
                "style" => parse_enum(key, value).map(|v| self.style = v),
                "charset" => parse_enum(key, value).map(|v| self.charset = v),
                "theme" => parse_enum(key, value).map(|v| self.theme = v),
                "top" => value
                    .parse::<usize>()
                    .map(|v| self.top = v.min(MAX_TOP))
                    .map_err(|_| anyhow::anyhow!("top must be a number 0-{MAX_TOP}")),
                "panels" => parse_panels(value).map(|v| self.panels = v),
                "layout" => parse_layout(value).map(|v| self.layout = v),
                "focus" => parse_focus(value).map(|v| self.focus = v),
                "stats" => parse_bool(key, value).map(|v| self.stats = v),
                "inline" => parse_inline(value).map(|v| self.inline = v),
                "group" => parse_bool(key, value).map(|v| self.group = v),
                _ => Err(anyhow::anyhow!(
                    "unknown setting `{key}` (expected style, charset, theme, top, panels, layout, focus, stats, inline or group)"
                )),
            };
            res.with_context(|| format!("line {}", n + 1))?;
        }
        Ok(())
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let rows = [
            (format!("style = \"{}\"", name(&self.style)), "line, area, bars, dots".to_string()),
            (
                format!("charset = \"{}\"", name(&self.charset)),
                "braille, octant, sextant, quadrant, halfblock, block, dot, ascii".to_string(),
            ),
            (format!("theme = \"{}\"", name(&self.theme)), "default, nord, gruvbox, dracula, solarized, light, mono".to_string()),
            (format!("top = {}", self.top), format!("top-process lines in the system view, 0 = off (max {MAX_TOP})")),
            (format!("panels = \"{}\"", panels_str(&self.panels)), "any of cpu, ram, disk, net, gpu".to_string()),
            (format!("layout = \"{}\"", self.layout.text()), "auto, rows, columns, grid, or e.g. \"cpu / ram disk / net gpu\"".to_string()),
            (format!("focus = \"{}\"", focus_str(self.focus)), "panel given the full-width row in an odd grid, or none".to_string()),
            (format!("stats = {}", self.stats), "now/avg/peak and top consumers beside each chart".to_string()),
            (
                format!("inline = {}", self.inline.map_or("false".to_string(), |n| n.to_string())),
                "draw below the prompt in N rows and leave it on screen; false = full screen".to_string(),
            ),
            (format!("group = {}", self.group), "combine processes with the same name into one app".to_string()),
        ];
        let mut text = String::from("# pmon display defaults (written by `pmon --config` or `w` in pmon)\n");
        for (line, comment) in rows {
            text += &format!("{line:<22}# {comment}\n");
        }
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_config() {
        let s = Settings {
            style: GraphStyle::Area,
            charset: Charset::HalfBlock,
            theme: ThemeName::Nord,
            top: 3,
            panels: vec![PanelKind::Cpu, PanelKind::Net],
            layout: Arrangement::Preset(LayoutKind::Grid),
            focus: Some(PanelKind::Cpu),
            stats: false,
            inline: Some(30),
            group: true,
        };
        let text = format!(
            "style = \"{}\"\ncharset = {} # comment\n\n theme=\"{}\"\ntop = 3\npanels = \"network, cpu\"\nlayout = grid\nstats = off\ninline = 30\n",
            name(&s.style),
            name(&s.charset),
            name(&s.theme)
        );
        let mut got = Settings { theme: ThemeName::Default, ..Settings::default() };
        got.apply(&text).unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn rejects_bad_values() {
        let err = Settings::default().apply("theme = \"neon\"").unwrap_err();
        assert!(format!("{err:#}").contains("expected one of"));
        assert!(Settings::default().apply("colour = red").is_err());
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn saved_file_loads_back() {
        let dir = std::env::temp_dir().join(format!("pmon-settings-{}", std::process::id()));
        let s = Settings { panels: vec![PanelKind::Ram, PanelKind::Gpu], inline: Some(20), stats: false, ..Settings::default() };
        // save() writes to config_path(); exercise the same text via apply.
        let rows = format!("panels = \"{}\"\ninline = 20\nstats = false\n", panels_str(&s.panels));
        let mut got = Settings::default();
        got.apply(&rows).unwrap();
        assert_eq!((got.panels, got.inline, got.stats), (s.panels, s.inline, s.stats));
        assert_eq!(parse_inline("true").unwrap(), Some(DEFAULT_INLINE));
        assert!(parse_panels("cpu,toaster").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use PanelKind::*;

    #[test]
    fn parses_templates_and_presets() {
        assert_eq!(parse_layout("Grid").unwrap(), Arrangement::Preset(LayoutKind::Grid));
        let a = parse_layout(" cpu / ram,disk / net  gpu ").unwrap();
        assert_eq!(a, Arrangement::Rows(vec![vec![Cpu], vec![Ram, Disk], vec![Net, Gpu]]));
        assert_eq!(a.text(), "cpu / ram disk / net gpu");
        assert_eq!(parse_layout(&a.text()).unwrap(), a);
        assert!(parse_layout("cpu / cpu").is_err());
        assert!(parse_layout("cpu / toaster").is_err());
        assert!(parse_layout(" / ").is_err());
        // Cycling: the custom template, then the presets.
        let auto = Arrangement::Preset(LayoutKind::Auto);
        assert_eq!(a.step(Some(&a), 1), auto);
        assert_eq!(auto.step(Some(&a), -1), a);
        assert_eq!(Arrangement::Preset(LayoutKind::Grid).step(None, 1), auto);
    }
}
