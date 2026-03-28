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

/// Terminal-native theme: uses `Color::Reset` for backgrounds so the terminal's
/// own background shows through (like lazygit). Semantic colors use ANSI indexed
/// colors that adapt to the terminal's palette.
pub const fn default_theme() -> Theme {
    Theme {
        foreground: Color::Reset,       // terminal default foreground
        muted: Color::DarkGray,         // ANSI bright black / dim
        highlight: Color::Magenta,      // stands out in both light/dark
        info: Color::Blue,              // ANSI blue
        success: Color::Green,          // ANSI green
        warning: Color::Yellow,         // ANSI yellow
        danger: Color::Red,             // ANSI red
        border: Color::DarkGray,        // subtle borders
        selection: Color::Indexed(237), // subtle highlight row (#3a3a3a)
        panel_alt: Color::Reset,        // transparent
        background: Color::Reset,       // transparent — terminal bg
        panel: Color::Reset,            // transparent — terminal bg
    }
}

pub fn detect_terminal_theme() -> Option<Theme> {
    // The default theme already adapts to the terminal — no detection needed.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_uses_transparent_backgrounds() {
        let theme = default_theme();
        assert_eq!(theme.background, Color::Reset);
        assert_eq!(theme.panel, Color::Reset);
        assert_eq!(theme.panel_alt, Color::Reset);
    }
}
