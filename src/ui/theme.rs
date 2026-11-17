//! Colours of the current Omarchy theme, so chart images match the terminal around them
//! (as in whereami). Tokyo Night when the theme file is missing.

use std::collections::HashMap;

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    fn parse(s: &str) -> Option<Self> {
        let hex = s.trim().trim_matches('"').strip_prefix('#')?;
        if hex.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Self((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// The same colour for ratatui text.
    #[must_use]
    pub fn tui(self) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(self.0, self.1, self.2)
    }
}

/// The theme roles the charts use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    pub muted: Rgb,
    pub dim: Rgb,
    pub accent: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub orange: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    /// The terminal colours text is drawn in, where they differ from the roles above.
    pub text: TextColors,
}

/// Text colours not among the chart roles: brighter variants and greys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextColors {
    pub black: Rgb,
    pub light_red: Rgb,
    pub light_green: Rgb,
    pub light_blue: Rgb,
    pub cyan: Rgb,
    pub gray: Rgb,
    pub white: Rgb,
}

impl Theme {
    /// Reads `~/.local/state/omarchy/current/theme/colors.toml`.
    #[must_use]
    pub fn load() -> Self {
        let text = std::env::var_os("HOME")
            .map(|h| {
                std::path::PathBuf::from(h).join(".local/state/omarchy/current/theme/colors.toml")
            })
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    /// Theme from `key = "#rrggbb"` lines; missing keys take Tokyo Night's colours.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let map: HashMap<&str, Rgb> = text
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                Some((k.trim(), Rgb::parse(v)?))
            })
            .collect();
        let get = |key: &str, fallback: &str| {
            map.get(key)
                .copied()
                .unwrap_or_else(|| Rgb::parse(fallback).expect("fallback colours are valid hex"))
        };
        Self {
            background: get("background", "#1a1b26"),
            foreground: get("foreground", "#a9b1d6"),
            muted: get("muted", "#414868"),
            dim: get("dark_foreground", "#565f89"),
            accent: get("accent", "#7aa2f7"),
            red: get("red", "#f7768e"),
            yellow: get("yellow", "#e0af68"),
            orange: get("orange", "#eb927b"),
            green: get("green", "#9ece6a"),
            cyan: get("cyan", "#449dab"),
            blue: get("blue", "#7aa2f7"),
            magenta: get("magenta", "#ad8ee6"),
            text: TextColors {
                black: get("dark_background", "#15161e"),
                light_red: get("bright_red", "#ff7a93"),
                light_green: get("bright_green", "#b9f27c"),
                light_blue: get("bright_blue", "#7da6ff"),
                cyan: Rgb(0x7d, 0xcf, 0xff),
                gray: Rgb(0x9a, 0xa5, 0xce),
                white: get("bright_foreground", "#c0caf5"),
            },
        }
    }

    /// The colour a terminal in this theme shows for `c`, as text (`fg`) or behind it.
    #[must_use]
    pub fn cell(&self, c: ratatui::style::Color, fg: bool) -> Rgb {
        use ratatui::style::Color;
        let t = &self.text;
        match c {
            Color::Reset if fg => self.foreground,
            Color::Reset => self.background,
            Color::Black => t.black,
            Color::Red => self.red,
            Color::LightRed => t.light_red,
            Color::Green => self.green,
            Color::LightGreen => t.light_green,
            Color::Yellow | Color::LightYellow => self.yellow,
            Color::Blue => self.blue,
            Color::LightBlue => t.light_blue,
            Color::Magenta | Color::LightMagenta => self.magenta,
            Color::Cyan | Color::LightCyan => t.cyan,
            Color::Gray => t.gray,
            Color::DarkGray => self.dim,
            Color::White => t.white,
            Color::Rgb(r, g, b) => Rgb(r, g, b),
            Color::Indexed(_) => self.foreground,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omarchy_colors_with_fallbacks() {
        let t = Theme::parse(
            "mode = \"dark\"\naccent = \"#112233\"\nred = \"#ABCDEF\"\nbad = \"#12\"\n",
        );
        assert_eq!(t.accent, Rgb(0x11, 0x22, 0x33));
        assert_eq!(t.red, Rgb(0xab, 0xcd, 0xef));
        assert_eq!(t.background, Rgb(0x1a, 0x1b, 0x26));
    }
}
