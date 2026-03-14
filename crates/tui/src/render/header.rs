use {
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::{Constraint, Direction, Layout, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{Block, BorderType, Paragraph, Tabs},
    },
};

use crate::app::{ActiveTab, AppState};

pub fn draw_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;

    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    // Tab bar
    let tabs = Tabs::new(
        ActiveTab::ALL
            .into_iter()
            .map(|tab| Line::from(Span::styled(tab.title(), Style::default().fg(theme.muted))))
            .collect::<Vec<_>>(),
    )
    .select(app.active_tab.index())
    .divider(Span::raw("  "))
    .highlight_style(
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )
    .block({
        let block = Block::default()
            .title(Line::from(Span::styled(
                " Polyphony ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border));

        block
    });
    frame.render_widget(tabs, sections[0]);

    // Status summary
    let summary = vec![Line::from(vec![
        Span::styled("issues ", Style::default().fg(theme.muted)),
        Span::styled(
            snapshot.visible_issues.len().to_string(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("running ", Style::default().fg(theme.muted)),
        Span::styled(
            snapshot.counts.running.to_string(),
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("tasks ", Style::default().fg(theme.muted)),
        Span::styled(
            format!(
                "{}/{}",
                snapshot.counts.tasks_completed,
                snapshot.counts.tasks_pending
                    + snapshot.counts.tasks_in_progress
                    + snapshot.counts.tasks_completed
            ),
            Style::default()
                .fg(theme.info)
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    let (status_dot, status_label, status_color) = if snapshot.from_cache {
        ("●", "cached", theme.warning)
    } else {
        ("●", "online", theme.success)
    };

    frame.render_widget(
        Paragraph::new(summary).block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled(
                        format!(" {status_dot} "),
                        Style::default().fg(status_color),
                    ),
                    Span::styled(
                        format!("{status_label} "),
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border)),
        ),
        sections[1],
    );
}
