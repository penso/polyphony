use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, LineGauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
};

use super::detail_common::{kv_line, render_scroll_indicator, render_separator};
use crate::app::AppState;

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
                format!(
                    "{} ",
                    super::orchestrator::movement_kind_label(movement.kind)
                ),
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
            Span::styled(
                movement.status.to_string(),
                Style::default().fg(status_color),
            ),
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
        LineGauge::default()
            .filled_style(
                Style::default()
                    .fg(theme.background)
                    .bg(status_color)
                    .add_modifier(Modifier::BOLD),
            )
            .unfilled_style(Style::default().fg(theme.border).bg(theme.background))
            .filled_symbol(ratatui::symbols::line::THICK_HORIZONTAL)
            .unfilled_symbol(ratatui::symbols::line::THICK_HORIZONTAL)
            .label(Line::from(Span::styled(
                gauge_label,
                Style::default().fg(theme.foreground),
            )))
            .ratio(ratio),
        rows[2],
    );

    render_separator(frame, rows[3], inner.width, theme);

    // Body: KV info + related tasks + events
    let format_time = super::format_detail_time;

    let mut lines = vec![
        kv_line("ID", &movement.id, theme),
        kv_line(
            "Kind",
            super::orchestrator::movement_kind_label(movement.kind),
            theme,
        ),
        kv_line(
            "Target",
            &super::orchestrator::movement_target_label(movement),
            theme,
        ),
        kv_line("Created", &format_time(movement.created_at), theme),
    ];

    if let Some(deliverable) = &movement.deliverable {
        lines.push(kv_line(
            "Decision",
            &deliverable.decision.to_string(),
            theme,
        ));
        if let Some(url) = &deliverable.url {
            lines.push(kv_line("URL", url, theme));
        }
    }
    if let Some(workspace_key) = &movement.workspace_key {
        lines.push(kv_line("Wkspace", workspace_key, theme));
    }
    if let Some(workspace_path) = &movement.workspace_path {
        lines.push(kv_line(
            "Path",
            &workspace_path.display().to_string(),
            theme,
        ));
    }

    if let Some(target) = &movement.review_target {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Review Target",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
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

    // Tasks with agent session info
    {
        let mut task_indices: Vec<usize> = snapshot
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.movement_id == movement.id)
            .map(|(i, _)| i)
            .collect();
        task_indices.sort_by_key(|&i| snapshot.tasks[i].ordinal);

        if !task_indices.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "Tasks",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )));

            for &idx in &task_indices {
                let task = &snapshot.tasks[idx];
                let icon = super::tasks::task_status_icon(&task.status);
                let color = super::tasks::task_status_color(&task.status, theme);

                // Status icon + title
                lines.push(Line::from(vec![
                    Span::styled(format!("{icon} "), Style::default().fg(color)),
                    Span::styled(
                        task.title.clone(),
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));

                // Agent, turns, tokens on one line
                let agent_label = task.agent_name.as_deref().unwrap_or("-");
                let turns_label = format!("{} turns", task.turns_completed);
                let tokens_label = if task.total_tokens > 0 {
                    super::agents::format_tokens_pub(task.total_tokens)
                } else {
                    "-".into()
                };
                lines.push(Line::from(vec![
                    Span::styled("  Agent: ", Style::default().fg(theme.muted)),
                    Span::styled(agent_label.to_string(), Style::default().fg(theme.info)),
                    Span::styled("  Turns: ", Style::default().fg(theme.muted)),
                    Span::styled(turns_label, Style::default().fg(theme.foreground)),
                    Span::styled("  Tokens: ", Style::default().fg(theme.muted)),
                    Span::styled(tokens_label, Style::default().fg(theme.foreground)),
                ]));

                // Duration
                let duration_label = match (task.started_at, task.finished_at) {
                    (Some(start), Some(end)) => {
                        super::agents::format_duration(end.signed_duration_since(start))
                    },
                    (Some(start), None) => {
                        let elapsed = super::agents::format_duration(
                            chrono::Utc::now().signed_duration_since(start),
                        );
                        format!("{elapsed} (running)")
                    },
                    _ => "not started".into(),
                };
                lines.push(Line::from(vec![
                    Span::styled("  Duration: ", Style::default().fg(theme.muted)),
                    Span::styled(duration_label, Style::default().fg(theme.foreground)),
                ]));

                // Error if present
                if let Some(error) = &task.error {
                    lines.push(Line::from(vec![
                        Span::styled("  Error: ", Style::default().fg(theme.muted)),
                        Span::styled(error.clone(), Style::default().fg(theme.danger)),
                    ]));
                }
            }
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

    render_scroll_indicator(
        frame,
        body_area,
        scroll_pos,
        total_lines,
        visible_height,
        theme,
    );
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
