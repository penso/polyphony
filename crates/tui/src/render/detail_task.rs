use {
    polyphony_core::{RuntimeSnapshot, TaskCategory, TaskStatus},
    ratatui::{
        layout::{Constraint, Direction, Layout, Margin, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
        },
    },
};

use super::detail_common::{kv_line, render_scroll_indicator, render_separator};
use crate::app::AppState;

pub(crate) fn draw_task_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    task_id: &str,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let Some(task) = snapshot.tasks.iter().find(|t| t.id == task_id) else {
        draw_not_found(frame, area, "Task no longer available", theme);
        return;
    };

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" Task ", Style::default().fg(theme.info)),
            Span::styled(
                format!("#{} ", task.ordinal),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":back ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.detail_border_focused {
            theme.highlight
        } else {
            theme.border
        }))
        .style(Style::default().bg(theme.panel_alt));
    frame.render_widget(&block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    let title_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    // Title
    frame.render_widget(
        Paragraph::new(Span::styled(
            task.title.clone(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ))
        .wrap(Wrap { trim: false }),
        title_rows[0],
    );

    // Meta line
    let status_color = task_status_color(task.status, theme);
    let meta = Line::from(vec![
        Span::styled(task_status_icon(task.status), Style::default().fg(status_color)),
        Span::styled(" ", Style::default()),
        Span::styled(task.status.to_string(), Style::default().fg(status_color)),
        Span::styled("  ", Style::default()),
        Span::styled(
            task_category_label(task.category),
            Style::default().fg(theme.muted),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            task.agent_name.as_deref().unwrap_or("unassigned"),
            Style::default().fg(theme.muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(meta), title_rows[1]);
    render_separator(frame, title_rows[2], inner.width, theme);

    // Body
    let format_time = super::format_detail_time;
    let mut lines = vec![
        kv_line("ID", &task.id, theme),
        kv_line("Flow", &task.movement_id, theme),
        kv_line("Turns", &task.turns_completed.to_string(), theme),
        kv_line("Tokens", &task.total_tokens.to_string(), theme),
        kv_line("Created", &format_time(task.created_at), theme),
        kv_line("Updated", &format_time(task.updated_at), theme),
    ];

    if let Some(started_at) = task.started_at {
        lines.push(kv_line("Started", &format_time(started_at), theme));
    }
    if let Some(finished_at) = task.finished_at {
        lines.push(kv_line("Finished", &format_time(finished_at), theme));
    }
    if let Some(description) = &task.description
        && !description.trim().is_empty()
    {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Description",
            Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
        )));
        for line in description.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.foreground),
            )));
        }
    }
    if let Some(error) = &task.error
        && !error.trim().is_empty()
    {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Error",
            Style::default().fg(theme.danger).add_modifier(Modifier::BOLD),
        )));
        for line in error.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.warning),
            )));
        }
    }

    // Scrollable rendering
    let body_area = title_rows[3];
    let visible_height = body_area.height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    let current_scroll = app.current_detail_scroll();
    if current_scroll as usize > max_scroll {
        app.set_current_detail_scroll(max_scroll as u16);
    }
    let scroll_pos = app.current_detail_scroll();

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_pos, 0)),
        body_area,
    );

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_pos as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            body_area,
            &mut scrollbar_state,
        );
    }

    render_scroll_indicator(frame, body_area, scroll_pos, total_lines, visible_height, theme);
}

fn task_status_icon(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "…",
        TaskStatus::InProgress => "◐",
        TaskStatus::Completed => "✓",
        TaskStatus::Failed => "✕",
        TaskStatus::Cancelled => "⊘",
    }
}

fn task_status_color(status: TaskStatus, theme: crate::theme::Theme) -> ratatui::style::Color {
    match status {
        TaskStatus::Pending => theme.info,
        TaskStatus::InProgress => theme.success,
        TaskStatus::Completed => theme.muted,
        TaskStatus::Failed => theme.danger,
        TaskStatus::Cancelled => theme.muted,
    }
}

fn task_category_label(category: TaskCategory) -> &'static str {
    match category {
        TaskCategory::Research => "research",
        TaskCategory::Coding => "coding",
        TaskCategory::Testing => "testing",
        TaskCategory::Documentation => "docs",
        TaskCategory::Review => "review",
    }
}

fn draw_not_found(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    message: &str,
    theme: crate::theme::Theme,
) {
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_alt));
    frame.render_widget(&block, area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    frame.render_widget(
        Paragraph::new(Span::styled(message, Style::default().fg(theme.muted))),
        inner,
    );
}
