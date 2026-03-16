use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::AppState;

pub fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let theme = app.theme;

    let version = env!("CARGO_PKG_VERSION");
    let shortcuts = [
        ("1-6", "tabs"),
        ("j/k", "navigate"),
        ("J/K", "detail"),
        ("s", "sort"),
        ("/", "search"),
        ("Enter", "details"),
        ("d", "dispatch"),
        ("o/O", "open"),
        ("a/x", "approve/accept/reject"),
        ("m", "mode"),
        ("r", "refresh"),
        ("q", "quit"),
    ];

    let mut spans = vec![
        Span::styled(format!(" v{version} "), Style::default().fg(theme.muted)),
        Span::styled("│ ", Style::default().fg(theme.border)),
    ];

    for (i, (key, label)) in shortcuts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(theme.border)));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default().fg(theme.highlight),
        ));
        spans.push(Span::styled(
            format!(":{label}"),
            Style::default().fg(theme.muted),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
