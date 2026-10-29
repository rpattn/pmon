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
}

impl Default for Settings {
    fn default() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        Settings {
            style: GraphStyle::Line,
            charset: Charset::Braille,
            theme: if no_color { ThemeName::Mono } else { ThemeName::Default },
            top: 0,
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
                _ => Err(anyhow::anyhow!("unknown setting `{key}` (expected style, charset, theme or top)")),
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
        let s = Settings { style: GraphStyle::Area, charset: Charset::HalfBlock, theme: ThemeName::Nord, top: 3 };
        let text = format!(
            "style = \"{}\"\ncharset = {} # comment\n\n theme=\"{}\"\ntop = 3\n",
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
