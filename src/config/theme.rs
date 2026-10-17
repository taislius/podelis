//! Colours, named after their role in the design ("SQL operator console"
//! in the Figma file). Chosen once at startup.

// Colours are written as 0xrrggbb to match the design file.
#![allow(clippy::unreadable_literal)]

use std::sync::OnceLock;

use ratatui::style::Color;

use crate::config::settings::ThemeName;

#[derive(Clone, Copy)]
pub struct Theme {
    // Surfaces, darkest to lightest.
    /// Behind everything.
    pub canvas: Color,
    /// Pane bodies, application bar.
    pub panel: Color,
    /// Pane sub-bars, gutters, table headers, status bar.
    pub inset: Color,
    /// Focused pane label, active tab, current line, selected row.
    pub active: Color,
    /// Selected cell, selected row number, badges.
    pub selected: Color,
    /// The active tab: a neutral grey, so it shows without competing.
    pub tab: Color,

    // Lines.
    /// Dividers inside panes.
    pub line: Color,
    /// Pane borders and header separators.
    pub line_strong: Color,
    /// Focus, brand, primary actions.
    pub accent: Color,

    // Text, brightest to faintest.
    pub text_bright: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    /// Text on an accent-coloured background.
    pub on_accent: Color,

    // Syntax and states.
    pub keyword: Color,
    pub ident: Color,
    pub function: Color,
    pub string: Color,
    pub number: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    /// Editor text selection.
    pub selection: Color,
}

const fn hex(rgb: u32) -> Color {
    Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The design's palette.
const DARK: Theme = Theme {
    canvas: hex(0x050a0a),
    panel: hex(0x091110),
    inset: hex(0x0c1715),
    active: hex(0x10231f),
    selected: hex(0x173e34),
    tab: hex(0x2b3230),
    line: hex(0x29433d),
    line_strong: hex(0x3b6359),
    accent: hex(0x43d6a7),
    text_bright: hex(0xf3faf7),
    text: hex(0xd9e7e2),
    muted: hex(0x718a82),
    dim: hex(0x4d665f),
    on_accent: hex(0x020505),
    keyword: hex(0xbb9af7),
    ident: hex(0x57c7e8),
    function: hex(0x7aa2f7),
    string: hex(0x9ece6a),
    number: hex(0xe0af68),
    ok: hex(0x9ece6a),
    warn: hex(0xe0af68),
    error: hex(0xf7768e),
    selection: hex(0x29433d),
};

/// The same roles for light terminal backgrounds.
const LIGHT: Theme = Theme {
    canvas: hex(0xf6faf8),
    panel: hex(0xffffff),
    inset: hex(0xeef4f1),
    active: hex(0xe0f2ea),
    selected: hex(0xc6eadb),
    tab: hex(0xdfe3e2),
    line: hex(0xd3e0db),
    line_strong: hex(0xa9c2b9),
    accent: hex(0x0f8f68),
    text_bright: hex(0x0b1512),
    text: hex(0x1f2d29),
    muted: hex(0x5a6f68),
    dim: hex(0x8a9c96),
    on_accent: hex(0xffffff),
    keyword: hex(0x7a3fc4),
    ident: hex(0x0b7fa3),
    function: hex(0x2f5fc4),
    string: hex(0x3d7a1c),
    number: hex(0xa8620f),
    ok: hex(0x3d7a1c),
    warn: hex(0xa8620f),
    error: hex(0xc4304f),
    selection: hex(0xc6eadb),
};

static THEME: OnceLock<Theme> = OnceLock::new();

pub fn set_theme(name: ThemeName) {
    let theme = match name {
        ThemeName::Dark => DARK,
        ThemeName::Light => LIGHT,
    };
    let _ = THEME.set(if supports_truecolor() { theme } else { theme.to_256() });
}

pub fn theme() -> &'static Theme {
    THEME.get().unwrap_or(&DARK)
}

/// Whether the terminal shows 24-bit colour. Terminals that don't (e.g.
/// macOS Terminal.app) get the nearest of the 256 standard colours.
fn supports_truecolor() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    if matches!(env("COLORTERM").as_str(), "truecolor" | "24bit") {
        return true;
    }
    let term_program = env("TERM_PROGRAM");
    if term_program == "Apple_Terminal" {
        return false;
    }
    matches!(term_program.as_str(), "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "WarpTerminal")
        || ["kitty", "alacritty", "foot", "wezterm", "ghostty"].iter().any(|t| env("TERM").contains(t))
}

impl Theme {
    fn to_256(self) -> Theme {
        let c = |color: Color| match color {
            Color::Rgb(r, g, b) => Color::Indexed(nearest_256(r, g, b)),
            other => other,
        };
        Theme {
            canvas: c(self.canvas),
            panel: c(self.panel),
            inset: c(self.inset),
            active: c(self.active),
            selected: c(self.selected),
            tab: c(self.tab),
            line: c(self.line),
            line_strong: c(self.line_strong),
            accent: c(self.accent),
            text_bright: c(self.text_bright),
            text: c(self.text),
            muted: c(self.muted),
            dim: c(self.dim),
            on_accent: c(self.on_accent),
            keyword: c(self.keyword),
            ident: c(self.ident),
            function: c(self.function),
            string: c(self.string),
            number: c(self.number),
            ok: c(self.ok),
            warn: c(self.warn),
            error: c(self.error),
            selection: c(self.selection),
        }
    }
}

/// Nearest xterm-256 colour: the 6×6×6 cube or the 24-step grey ramp.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| LEVELS.iter().enumerate().min_by_key(|(_, l)| (i32::from(**l) - i32::from(v)).abs()).unwrap().0;
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    let cube_rgb = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let avg = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let grey_step = (avg.saturating_sub(8) / 10).min(23) as u8;
    let grey = 8 + 10 * grey_step;
    if dist((grey, grey, grey)) < dist(cube_rgb) { 232 + grey_step } else { cube as u8 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_256_colours() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(0x43, 0xd6, 0xa7), 79); // accent → a teal in the cube
        assert_eq!(nearest_256(128, 128, 128), 244); // mid grey → grey ramp
    }

    #[test]
    fn fallback_has_no_rgb() {
        let t = DARK.to_256();
        assert!(!matches!(t.accent, Color::Rgb(..)) && !matches!(t.canvas, Color::Rgb(..)));
    }
}
