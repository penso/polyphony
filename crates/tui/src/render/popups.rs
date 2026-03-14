use {
    polyphony_core::VisibleIssueRow,
    ratatui::{
        layout::{Constraint, Direction, Layout, Margin, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{Block, BorderType, Clear, Paragraph, Wrap},
    },
};

use crate::theme::Theme;

pub fn draw_issue_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    issue: &VisibleIssueRow,
    theme: Theme,
) {
    let area = centered_rect(frame.area(), 76, 22);
    frame.render_widget(Clear, area);

    let source_icon = if issue.issue_identifier.starts_with("GH-")
        || issue.issue_identifier.contains('#')
    {
        " GitHub"
    } else {
        "◆ Linear"
    };

    let state_color = super::issues::state_color(&issue.state, theme);

    let priority_str = issue
        .priority
        .map(|p| format!("P{p}"))
        .unwrap_or_else(|| "—".into());
    let priority_color = match issue.priority {
        Some(0) => theme.danger,
        Some(1) => theme.warning,
        Some(2) => theme.info,
        _ => theme.muted,
    };

    let block = Block::default()
        .title(Line::from(Span::styled(
            format!(" {} ", issue.issue_identifier),
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(Span::styled(" Esc to close ", Style::default().fg(theme.muted)))
                .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.panel_alt));

    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Title
            Constraint::Length(1), // Separator
            Constraint::Length(1), // Source
            Constraint::Length(1), // Status
            Constraint::Length(1), // Priority
            Constraint::Length(1), // Labels
            Constraint::Length(1), // URL
            Constraint::Length(1), // Separator
            Constraint::Min(1),   // Description
        ])
        .split(inner);

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            issue.title.clone(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )))
        .wrap(Wrap { trim: false }),
        rows[0],
    );

    // Separator
    render_separator(frame, rows[1], inner.width, theme);

    // Source
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Source   ", Style::default().fg(theme.muted)),
            Span::styled(source_icon, Style::default().fg(theme.info)),
        ])),
        rows[2],
    );

    // Status
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Status   ", Style::default().fg(theme.muted)),
            Span::styled(issue.state.clone(), Style::default().fg(state_color)),
        ])),
        rows[3],
    );

    // Priority
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Priority ", Style::default().fg(theme.muted)),
            Span::styled(priority_str, Style::default().fg(priority_color)),
        ])),
        rows[4],
    );

    // Labels
    let labels_str = if issue.labels.is_empty() {
        "—".into()
    } else {
        issue.labels.join(", ")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Labels   ", Style::default().fg(theme.muted)),
            Span::styled(labels_str, Style::default().fg(theme.foreground)),
        ])),
        rows[5],
    );

    // URL
    let url_str = issue.url.as_deref().unwrap_or("—");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("URL      ", Style::default().fg(theme.muted)),
            Span::styled(url_str, Style::default().fg(theme.info)),
        ])),
        rows[6],
    );

    // Separator
    render_separator(frame, rows[7], inner.width, theme);

    // Description
    let desc_text = issue
        .description
        .as_deref()
        .unwrap_or("No description.");
    frame.render_widget(
        Paragraph::new(Span::styled(
            desc_text,
            Style::default().fg(theme.foreground),
        ))
        .wrap(Wrap { trim: false }),
        rows[8],
    );
}

pub fn draw_leaving_modal(frame: &mut ratatui::Frame<'_>, theme: Theme) {
    let area = centered_rect(frame.area(), 24, 3);
    frame.render_widget(Clear, area);
    let text = Line::from(Span::styled(
        "Leaving...",
        Style::default().fg(theme.foreground),
    ));
    frame.render_widget(
        Paragraph::new(text)
            .alignment(ratatui::layout::Alignment::Center)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.background)),
            ),
        area,
    );
}

fn render_separator(frame: &mut ratatui::Frame<'_>, area: Rect, width: u16, theme: Theme) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(width as usize),
            Style::default().fg(theme.border),
        ))),
        area,
    );
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}
