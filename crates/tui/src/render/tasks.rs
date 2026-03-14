use {
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Cell, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation,
            ScrollbarState, Table,
        },
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
    let tasks = &snapshot.tasks;

    let header = Row::new(vec![
        Cell::from(Span::styled("ID", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Type", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Status", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = tasks
        .iter()
        .map(|task| {
            let status_color = task_status_color(&task.status, theme);
            let category_color = task_category_color(&task.category, theme);

            Row::new(vec![
                Cell::from(Span::styled(
                    truncate_id(&task.id),
                    Style::default().fg(theme.info),
                )),
                Cell::from(Span::styled(
                    task.title.clone(),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    task.category.to_string(),
                    Style::default().fg(category_color),
                )),
                Cell::from(Span::styled(
                    task.status.to_string(),
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

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol("▸ ")
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
            .border_style(Style::default().fg(theme.border)),
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

fn task_status_color(
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

fn task_category_color(
    category: &polyphony_core::TaskCategory,
    theme: crate::theme::Theme,
) -> ratatui::style::Color {
    use polyphony_core::TaskCategory;
    match category {
        TaskCategory::Coding => theme.highlight,
        TaskCategory::Testing => theme.success,
        TaskCategory::Research => theme.info,
        TaskCategory::Documentation => theme.warning,
        TaskCategory::Review => theme.foreground,
    }
}

fn truncate_id(id: &str) -> String {
    if id.len() <= 8 {
        id.into()
    } else {
        format!("{}…", &id[..7])
    }
}
