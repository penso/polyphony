use {
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::{Constraint, Direction, Layout, Margin, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Gauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
            Wrap,
        },
    },
};

use super::detail_common::{kv_line, render_scroll_indicator, render_separator};
use crate::app::{AppState, DetailSection, DetailView};

pub(crate) fn draw_movement_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    movement_id: &str,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let Some(movement) = snapshot.movements.iter().find(|m| m.id == movement_id) else {
        draw_not_found(frame, area, "Movement no longer available", theme);
        return;
    };

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" Movement ", Style::default().fg(theme.info)),
            Span::styled(
                format!("{} ", super::orchestrator::movement_kind_label(movement.kind)),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(theme.highlight)),
                Span::styled(":focus  ", Style::default().fg(theme.muted)),
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("Shift+O", Style::default().fg(theme.highlight)),
                Span::styled(":open  ", Style::default().fg(theme.muted)),
                Span::styled("a", Style::default().fg(theme.highlight)),
                Span::styled(":accept  ", Style::default().fg(theme.muted)),
                Span::styled("x", Style::default().fg(theme.highlight)),
                Span::styled(":reject  ", Style::default().fg(theme.muted)),
                Span::styled("e", Style::default().fg(theme.highlight)),
                Span::styled(":events  ", Style::default().fg(theme.muted)),
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

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    // Title
    frame.render_widget(
        Paragraph::new(Span::styled(
            movement.title.clone(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ))
        .wrap(Wrap { trim: false }),
        rows[0],
    );

    // Status + target
    let status_color = super::orchestrator::movement_status_color_pub(&movement.status, theme);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(movement.status.to_string(), Style::default().fg(status_color)),
            Span::styled("  ", Style::default()),
            Span::styled(
                super::orchestrator::movement_target_label(movement),
                Style::default().fg(theme.info),
            ),
        ])),
        rows[1],
    );

    // Task progress gauge
    let ratio = if movement.task_count > 0 {
        movement.tasks_completed as f64 / movement.task_count as f64
    } else {
        0.0
    };
    let gauge_label = format!("{}/{} tasks", movement.tasks_completed, movement.task_count);
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(status_color).bg(theme.border))
            .label(Span::styled(gauge_label, Style::default().fg(theme.foreground)))
            .ratio(ratio),
        rows[2],
    );

    render_separator(frame, rows[3], inner.width, theme);

    // Body: KV info + related tasks + events
    let format_time = super::format_detail_time;

    let mut lines = vec![
        kv_line("ID", &movement.id, theme),
        kv_line("Kind", super::orchestrator::movement_kind_label(movement.kind), theme),
        kv_line("Target", &super::orchestrator::movement_target_label(movement), theme),
        kv_line("Created", &format_time(movement.created_at), theme),
    ];

    if let Some(deliverable) = &movement.deliverable {
        lines.push(kv_line("Decision", &deliverable.decision.to_string(), theme));
        if let Some(url) = &deliverable.url {
            lines.push(kv_line("URL", url, theme));
        }
    }
    if let Some(workspace_key) = &movement.workspace_key {
        lines.push(kv_line("Wkspace", workspace_key, theme));
    }
    if let Some(workspace_path) = &movement.workspace_path {
        lines.push(kv_line("Path", &workspace_path.display().to_string(), theme));
    }

    if let Some(target) = &movement.review_target {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Review Target",
            Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
        )));
        lines.push(kv_line("Repo", &target.repository, theme));
        lines.push(kv_line("PR", &format!("#{}", target.number), theme));
        lines.push(kv_line("SHA", &target.head_sha, theme));
        lines.push(kv_line(
            "Branches",
            &format!("{} -> {}", target.head_branch, target.base_branch),
            theme,
        ));
        if let Some(url) = &target.url {
            lines.push(kv_line("URL", url, theme));
        }
    }

    // Read focus state from the detail stack
    let (focus, tasks_selected) = match app.current_detail() {
        Some(DetailView::Movement {
            focus,
            tasks_selected,
            ..
        }) => (*focus, *tasks_selected),
        _ => (DetailSection::Body, 0),
    };
    let tasks_focused = focus == DetailSection::Section(0);

    // Related tasks
    let related_tasks: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|t| t.movement_id == movement.id)
        .collect();
    if !related_tasks.is_empty() {
        lines.push(Line::default());
        let section_marker = if tasks_focused { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(section_marker, Style::default().fg(theme.highlight)),
            Span::styled(
                "Tasks",
                Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
            ),
        ]));
        for (i, task) in related_tasks.iter().enumerate() {
            let task_status_color = match task.status {
                polyphony_core::TaskStatus::Pending => theme.info,
                polyphony_core::TaskStatus::InProgress => theme.success,
                polyphony_core::TaskStatus::Completed => theme.muted,
                polyphony_core::TaskStatus::Failed => theme.danger,
                polyphony_core::TaskStatus::Cancelled => theme.muted,
            };
            let is_selected = tasks_focused && i == tasks_selected;
            let prefix = if is_selected { "▸ " } else { "  " };
            let title_style = if is_selected {
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.highlight)),
                Span::styled(
                    format!("#{} ", task.ordinal),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
                    task.status.to_string(),
                    Style::default().fg(task_status_color),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    task.title.clone(),
                    title_style,
                ),
            ]));
        }
    }

    // Recent events (compact: 3 most recent)
    let movement_identifier = super::orchestrator::movement_target_label(movement);
    lines.extend(super::orchestrator::compact_recent_event_lines(
        snapshot,
        &movement_identifier,
        theme,
    ));

    // Scrollable rendering
    let body_area = rows[4];
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
