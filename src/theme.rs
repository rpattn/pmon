//! Colour schemes. Only foreground colours are set, so the terminal's own
//! background shows through.

use crate::settings::ThemeName;
use ratatui::style::Color;

pub struct Theme {
    pub cpu: Color,
    pub mem: Color,
    /// Disk read / network receive, and disk write / network send.
    pub read: Color,
    pub write: Color,
    pub gpu: Color,
    /// Borders, axes, labels and other low-key text.
    pub dim: Color,
    /// Picker border and prompts.
    pub accent: Color,
    pub warn: Color,
    pub ok: Color,
    pub err: Color,
    /// Top-process lines.
    pub palette: [Color; 8],
}

const fn hex(rgb: u32) -> Color {
    Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

const fn hexes<const N: usize>(v: [u32; N]) -> [Color; N] {
    let mut out = [Color::Reset; N];
    let mut i = 0;
    while i < N {
        out[i] = hex(v[i]);
        i += 1;
    }
    out
}

pub fn theme(name: ThemeName) -> Theme {
    use Color::*;
    match name {
        ThemeName::Default => Theme {
            cpu: Cyan,
            mem: LightMagenta,
            read: LightGreen,
            write: LightRed,
            gpu: LightBlue,
            dim: DarkGray,
            accent: Yellow,
            warn: Yellow,
            ok: Green,
            err: Red,
            palette: [Yellow, LightGreen, LightBlue, LightRed, White, Green, Blue, Red],
        },
        ThemeName::Nord => Theme {
            cpu: hex(0x88c0d0),
            mem: hex(0xb48ead),
            read: hex(0xa3be8c),
            write: hex(0xd08770),
            gpu: hex(0x81a1c1),
            dim: hex(0x4c566a),
            accent: hex(0xebcb8b),
            warn: hex(0xebcb8b),
            ok: hex(0xa3be8c),
            err: hex(0xbf616a),
            palette: hexes([0xebcb8b, 0xa3be8c, 0x81a1c1, 0xd08770, 0xbf616a, 0x8fbcbb, 0x5e81ac, 0xe5e9f0]),
        },
        ThemeName::Gruvbox => Theme {
            cpu: hex(0x83a598),
            mem: hex(0xd3869b),
            read: hex(0xb8bb26),
            write: hex(0xfe8019),
            gpu: hex(0x8ec07c),
            dim: hex(0x665c54),
            accent: hex(0xfabd2f),
            warn: hex(0xfabd2f),
            ok: hex(0xb8bb26),
            err: hex(0xfb4934),
            palette: hexes([0xfabd2f, 0xb8bb26, 0xfe8019, 0x8ec07c, 0xfb4934, 0xebdbb2, 0x458588, 0xd65d0e]),
        },
        ThemeName::Dracula => Theme {
            cpu: hex(0x8be9fd),
            mem: hex(0xff79c6),
            read: hex(0x50fa7b),
            write: hex(0xffb86c),
            gpu: hex(0xbd93f9),
            dim: hex(0x6272a4),
            accent: hex(0xf1fa8c),
            warn: hex(0xf1fa8c),
            ok: hex(0x50fa7b),
            err: hex(0xff5555),
            palette: hexes([0xf1fa8c, 0x50fa7b, 0xffb86c, 0xbd93f9, 0xff5555, 0xf8f8f2, 0x6272a4, 0x8be9fd]),
        },
        ThemeName::Solarized => Theme {
            cpu: hex(0x2aa198),
            mem: hex(0xd33682),
            read: hex(0x859900),
            write: hex(0xcb4b16),
            gpu: hex(0x268bd2),
            dim: hex(0x657b83),
            accent: hex(0xb58900),
            warn: hex(0xb58900),
            ok: hex(0x859900),
            err: hex(0xdc322f),
            palette: hexes([0xb58900, 0x859900, 0xcb4b16, 0x268bd2, 0x6c71c4, 0xdc322f, 0x93a1a1, 0x586e75]),
        },
        ThemeName::Light => Theme {
            cpu: hex(0x0060a0),
            mem: hex(0xa0007a),
            read: hex(0x1a7f37),
            write: hex(0xbc4c00),
            gpu: hex(0x8250df),
            dim: hex(0x909090),
            accent: hex(0x9a6700),
            warn: hex(0x9a6700),
            ok: hex(0x1a7f37),
            err: hex(0xcf222e),
            palette: hexes([0x9a6700, 0x1a7f37, 0xbc4c00, 0x0969da, 0x8250df, 0xcf222e, 0x404040, 0x00838f]),
        },
        ThemeName::Mono => Theme {
            cpu: Reset,
            mem: Reset,
            read: Reset,
            write: Reset,
            gpu: Reset,
            dim: Reset,
            accent: Reset,
            warn: Reset,
            ok: Reset,
            err: Reset,
            palette: [Reset; 8],
        },
    }
}
