use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub foreground: Color,
    pub muted: Color,
    pub highlight: Color,
    pub info: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub border: Color,
    pub selection: Color,
    /// Only used for popup overlays that need to clear the background.
    pub panel_alt: Color,
    pub background: Color,
    pub panel: Color,
}

pub const fn default_theme() -> Theme {
    Theme {
        foreground: Color::Rgb(241, 245, 249),
        muted: Color::Rgb(107, 114, 128),
        highlight: Color::Rgb(167, 139, 250),
        info: Color::Rgb(129, 140, 248),
        success: Color::Rgb(134, 239, 172),
        warning: Color::Rgb(251, 191, 36),
        danger: Color::Rgb(248, 113, 113),
        border: Color::Rgb(42, 42, 62),
        selection: Color::Rgb(31, 27, 47),
        panel_alt: Color::Rgb(26, 26, 46),
        background: Color::Rgb(10, 10, 15),
        panel: Color::Rgb(18, 18, 26),
    }
}

pub fn detect_terminal_theme() -> Option<Theme> {
    // Keep the TUI visually stable regardless of the user's terminal profile.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_uses_explicit_dark_palette() {
        let theme = default_theme();
        assert_eq!(theme.background, Color::Rgb(10, 10, 15));
        assert_eq!(theme.panel, Color::Rgb(18, 18, 26));
    }
}
