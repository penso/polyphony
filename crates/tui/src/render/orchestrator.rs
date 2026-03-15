use {
    chrono::Utc,
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::{Constraint, Direction, Layout, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Cell, HighlightSpacing, Paragraph, Row, Scrollbar,
            ScrollbarOrientation, ScrollbarState, Table, Wrap,
        },
    },
};

use crate::app::AppState;

pub fn draw_orchestrator_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // Status panel
            Constraint::Length(8),  // Movements table (compact)
            Constraint::Min(8),    // Recent events (gets remaining space)
        ])
        .split(area);

    draw_status_panel(frame, sections[0], snapshot, app);
    draw_movements_table(frame, sections[1], snapshot, app);
    draw_events_panel(frame, sections[2], snapshot, app);
}

fn draw_status_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    let loading = &snapshot.loading;

    let state_text = if loading.reconciling {
        ("Reconciling", theme.warning)
    } else if loading.fetching_issues {
        ("Fetching issues", theme.info)
    } else if loading.fetching_budgets {
        ("Refreshing budgets", theme.info)
    } else if loading.fetching_models {
        ("Discovering models", theme.info)
    } else if snapshot.counts.running > 0 {
        ("Running agents", theme.success)
    } else if snapshot.counts.retrying > 0 {
        ("Waiting (retries queued)", theme.warning)
    } else {
        ("Idle", theme.muted)
    };

    let next_poll = if snapshot.cadence.tracker_poll_interval_ms > 0 {
        if let Some(last) = snapshot.cadence.last_tracker_poll_at {
            let elapsed_ms = Utc::now()
                .signed_duration_since(last)
                .num_milliseconds()
                .max(0) as u64;
            let remaining_ms = snapshot
                .cadence
                .tracker_poll_interval_ms
                .saturating_sub(elapsed_ms);
            if remaining_ms == 0 {
                "due now".into()
            } else {
                format!("{}s", remaining_ms / 1000)
            }
        } else {
            "pending".into()
        }
    } else {
        "manual".into()
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("State     ", Style::default().fg(theme.muted)),
            Span::styled(state_text.0, Style::default().fg(state_text.1)),
        ]),
        Line::from(vec![
            Span::styled("Running   ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.counts.running.to_string(),
                Style::default().fg(theme.success),
            ),
            Span::styled("  retry ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.counts.retrying.to_string(),
                Style::default().fg(theme.warning),
            ),
            Span::styled("  next poll ", Style::default().fg(theme.muted)),
            Span::styled(next_poll, Style::default().fg(theme.foreground)),
        ]),
        Line::from(vec![
            Span::styled("Movements ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.counts.movements.to_string(),
                Style::default().fg(theme.foreground),
            ),
            Span::styled("  tasks ", Style::default().fg(theme.muted)),
            Span::styled(
                format!(
                    "{}/{}/{}",
                    snapshot.counts.tasks_completed,
                    snapshot.counts.tasks_in_progress,
                    snapshot.counts.tasks_pending,
                ),
                Style::default().fg(theme.info),
            ),
            Span::styled(" (done/active/pending)", Style::default().fg(theme.muted)),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Line::from(Span::styled(
                    " Orchestrator ",
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                )))
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border)),
        ),
        area,
    );
}

fn draw_movements_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let movements = &snapshot.movements;

    let header = Row::new(vec![
        Cell::from(Span::styled("Issue", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Status", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Tasks", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("PR", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = movements
        .iter()
        .map(|m| {
            let status_color = movement_status_color(&m.status, theme);
            let task_info = format!("{}/{}", m.tasks_completed, m.task_count);
            let pr_icon = if m.has_deliverable {
                "✓"
            } else {
                "—"
            };

            Row::new(vec![
                Cell::from(Span::styled(
                    m.issue_identifier.clone().unwrap_or_default(),
                    Style::default().fg(theme.info),
                )),
                Cell::from(Span::styled(
                    m.title.clone(),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    m.status.to_string(),
                    Style::default().fg(status_color),
                )),
                Cell::from(Span::styled(
                    task_info,
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    pr_icon,
                    Style::default().fg(if m.has_deliverable {
                        theme.success
                    } else {
                        theme.muted
                    }),
                )),
            ])
        })
        .collect();

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = movements.len();
    let footer_info = if count == 0 {
        "no movements".into()
    } else {
        format!(
            "{} of {count}",
            app.movements_state.selected().unwrap_or_default() + 1
        )
    };

    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Fill(1),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(4),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol("▸ ")
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Movements ",
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

    frame.render_stateful_widget(table, area, &mut app.movements_state);
}

fn movement_status_color(
    status: &polyphony_core::MovementStatus,
    theme: crate::theme::Theme,
) -> ratatui::style::Color {
    use polyphony_core::MovementStatus;
    match status {
        MovementStatus::Pending | MovementStatus::Planning => theme.info,
        MovementStatus::InProgress => theme.success,
        MovementStatus::Review => theme.highlight,
        MovementStatus::Delivered => theme.success,
        MovementStatus::Failed => theme.danger,
        MovementStatus::Cancelled => theme.muted,
    }
}

fn draw_events_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    app.events_area = area;

    let total = snapshot.recent_events.len();
    let content_height = area.height.saturating_sub(2) as usize; // border top+bottom

    // Reverse iteration: oldest first, newest at bottom (log-style)
    let lines: Vec<Line> = snapshot
        .recent_events
        .iter()
        .rev()
        .map(|event| {
            let ts = event.at.format("%H:%M:%S");
            let scope_color = match event.scope {
                polyphony_core::EventScope::Dispatch => theme.info,
                polyphony_core::EventScope::Handoff => theme.highlight,
                polyphony_core::EventScope::Worker | polyphony_core::EventScope::Agent => {
                    theme.success
                }
                polyphony_core::EventScope::Retry => theme.warning,
                polyphony_core::EventScope::Throttle => theme.danger,
                _ => theme.muted,
            };
            Line::from(vec![
                Span::styled(format!("{ts} "), Style::default().fg(theme.muted)),
                Span::styled(
                    format!("{:<10}", format!("{}", event.scope)),
                    Style::default().fg(scope_color),
                ),
                Span::styled(&event.message, Style::default().fg(theme.foreground)),
            ])
        })
        .collect();

    // Auto-scroll to bottom (newest) when new events arrive.
    // Follow new events if user was already at/near the bottom, or if
    // everything fits on screen (no scrollbar).
    let max_scroll = total.saturating_sub(content_height) as u16;
    let was_at_bottom = app.events_scroll >= max_scroll.saturating_sub(1);
    if was_at_bottom || total <= content_height {
        app.events_scroll = max_scroll;
    }
    if app.events_scroll > max_scroll {
        app.events_scroll = max_scroll;
    }

    let count_label = format!(" {total} events ");

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.events_scroll, 0))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(Line::from(Span::styled(
                        " Events ",
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    )))
                    .title(
                        Line::from(Span::styled(
                            count_label,
                            Style::default().fg(theme.muted),
                        ))
                        .right_aligned(),
                    )
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );

    // Scrollbar
    if total > content_height {
        let mut scrollbar_state = ScrollbarState::new(total)
            .position(app.events_scroll as usize)
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
