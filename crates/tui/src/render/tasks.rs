use chrono::{DateTime, Utc};
use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table,
    },
};

use crate::app::AppState;

pub fn draw_tasks_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    // Sort tasks oldest first (newest at bottom)
    let mut sorted_indices: Vec<usize> = (0..snapshot.tasks.len()).collect();
    sorted_indices.sort_by(|&a, &b| {
        let ta = snapshot.tasks[a]
            .started_at
            .unwrap_or(snapshot.tasks[a].created_at);
        let tb = snapshot.tasks[b]
            .started_at
            .unwrap_or(snapshot.tasks[b].created_at);
        ta.cmp(&tb)
    });
    app.sorted_task_indices = sorted_indices;
    let tasks = &app.sorted_task_indices;

    let header = Row::new(vec![
        Cell::from(Span::styled("", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Type", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("St", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = tasks
        .iter()
        .map(|&idx| {
            let task = &snapshot.tasks[idx];
            let status_color = task_status_color(&task.status, theme);
            let category_color = task_category_color(&task.category, theme);

            let time_label = format_task_time(task.started_at, task.created_at);

            Row::new(vec![
                Cell::from(Span::styled(time_label, Style::default().fg(theme.muted))),
                Cell::from(Span::styled(
                    task.title.clone(),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    task_category_label(&task.category),
                    Style::default().fg(category_color),
                )),
                Cell::from(Span::styled(
                    task_status_icon(&task.status),
                    Style::default().fg(status_color),
                )),
            ])
        })
        .collect();

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = tasks.len();
    let footer_info = if count == 0 {
        "no tasks".into()
    } else {
        format!(
            "{} of {count}",
            app.tasks_state.selected().unwrap_or_default() + 1
        )
    };

    let table = Table::new(rows, [
        Constraint::Length(16),
        Constraint::Fill(1),
        Constraint::Length(13),
        Constraint::Length(4),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Tasks ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(Span::styled(
                    format!("─{footer_info}─"),
                    Style::default().fg(theme.muted),
                ))
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if app.list_border_focused {
                theme.highlight
            } else {
                theme.border
            })),
    );

    frame.render_stateful_widget(table, area, &mut app.tasks_state);

    if count > 0 {
        let content_height = area.height.saturating_sub(3) as usize;
        if count > content_height {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.tasks_state.selected().unwrap_or(0))
                .viewport_content_length(content_height);
            let scrollbar_area = Rect {
                x: area.x,
                y: area.y + 1,
                width: area.width,
                height: area.height.saturating_sub(2),
            };
            frame.render_stateful_widget(
                Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight),
                scrollbar_area,
                &mut scrollbar_state,
            );
        }
    }
}

pub(crate) fn task_status_icon(status: &polyphony_core::TaskStatus) -> &'static str {
    use polyphony_core::TaskStatus;
    match status {
        TaskStatus::Pending => "…",
        TaskStatus::InProgress => "◐",
        TaskStatus::Completed => "✓",
        TaskStatus::Failed => "✕",
        TaskStatus::Cancelled => "⊘",
    }
}

pub(crate) fn task_status_color(
    status: &polyphony_core::TaskStatus,
    theme: crate::theme::Theme,
) -> ratatui::style::Color {
    use polyphony_core::TaskStatus;
    match status {
        TaskStatus::Pending => theme.info,
        TaskStatus::InProgress => theme.success,
        TaskStatus::Completed => theme.muted,
        TaskStatus::Failed => theme.danger,
        TaskStatus::Cancelled => theme.muted,
    }
}

fn task_category_label(category: &polyphony_core::TaskCategory) -> &'static str {
    use polyphony_core::TaskCategory;
    match category {
        TaskCategory::Coding => "coding",
        TaskCategory::Testing => "testing",
        TaskCategory::Research => "research",
        TaskCategory::Documentation => "docs",
        TaskCategory::Review => "review",
    }
}

fn format_task_time(started_at: Option<DateTime<Utc>>, created_at: DateTime<Utc>) -> String {
    super::format_listing_time(started_at.unwrap_or(created_at))
}

fn task_category_color(
    category: &polyphony_core::TaskCategory,
    theme: crate::theme::Theme,
) -> Color {
    use polyphony_core::TaskCategory;
    match category {
        TaskCategory::Coding => theme.muted,
        TaskCategory::Testing => theme.muted,
        TaskCategory::Research => theme.muted,
        TaskCategory::Documentation => theme.muted,
        TaskCategory::Review => theme.muted,
    }
}
