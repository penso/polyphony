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
        foreground: Color::White,
        muted: Color::DarkGray,
        highlight: Color::Yellow,
        info: Color::Cyan,
        success: Color::Green,
        warning: Color::Yellow,
        danger: Color::Red,
        border: Color::DarkGray,
        selection: Color::DarkGray,
        panel_alt: Color::Reset,
        background: Color::Reset,
        panel: Color::Reset,
    }
}

pub fn detect_terminal_theme() -> Option<Theme> {
    // No theme detection needed — we use terminal defaults + highlight colors.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_uses_reset_background() {
        let theme = default_theme();
        assert_eq!(theme.background, Color::Reset);
    }
}
