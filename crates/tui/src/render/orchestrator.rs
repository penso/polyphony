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
            Constraint::Length(6), // Status panel
            Constraint::Length(8), // Movements table
            Constraint::Min(8),    // Movement detail
            Constraint::Min(8),    // Recent events
        ])
        .split(area);

    draw_status_panel(frame, sections[0], snapshot, app);
    draw_movements_table(frame, sections[1], snapshot, app);
    draw_movement_detail(frame, sections[2], snapshot, app);
    draw_events_panel(frame, sections[3], snapshot, app);
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
        Line::from(throttle_status_spans(snapshot, theme)),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Line::from(Span::styled(
                    " Orchestration ",
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

fn throttle_status_spans<'a>(
    snapshot: &RuntimeSnapshot,
    theme: crate::theme::Theme,
) -> Vec<Span<'a>> {
    if snapshot.throttles.is_empty() {
        return vec![
            Span::styled("Throttles ", Style::default().fg(theme.muted)),
            Span::styled("none", Style::default().fg(theme.muted)),
        ];
    }

    let now = Utc::now();
    let mut spans = vec![
        Span::styled("Throttles ", Style::default().fg(theme.muted)),
        Span::styled(
            snapshot.throttles.len().to_string(),
            Style::default()
                .fg(theme.danger)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().fg(theme.muted)),
    ];

    for (index, throttle) in snapshot.throttles.iter().take(2).enumerate() {
        if index > 0 {
            spans.push(Span::styled(", ", Style::default().fg(theme.muted)));
        }
        let remaining = throttle.until.signed_duration_since(now);
        spans.push(Span::styled(
            format!(
                "{} {}",
                short_component(&throttle.component),
                compact_duration(remaining),
            ),
            Style::default().fg(theme.danger),
        ));
    }

    if snapshot.throttles.len() > 2 {
        spans.push(Span::styled(
            format!(" +{}", snapshot.throttles.len() - 2),
            Style::default().fg(theme.muted),
        ));
    }

    spans
}

fn short_component(component: &str) -> &str {
    component.rsplit(':').next().unwrap_or(component)
}

fn compact_duration(duration: chrono::Duration) -> String {
    let total_seconds = duration.num_seconds().max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
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
        Cell::from(Span::styled("Kind", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Target", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Status", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Tasks", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Out", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = movements
        .iter()
        .map(|m| {
            let status_color = movement_status_color(&m.status, theme);
            let task_info = format!("{}/{}", m.tasks_completed, m.task_count);
            let output_icon = if m.has_deliverable {
                "✓"
            } else if matches!(m.kind, polyphony_core::MovementKind::PullRequestReview) {
                "R"
            } else {
                "—"
            };

            Row::new(vec![
                Cell::from(Span::styled(
                    movement_kind_label(m.kind),
                    Style::default().fg(theme.info),
                )),
                Cell::from(Span::styled(
                    movement_target_label(m),
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
                    output_icon,
                    Style::default().fg(if m.has_deliverable {
                        theme.success
                    } else if matches!(m.kind, polyphony_core::MovementKind::PullRequestReview) {
                        theme.highlight
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
        Constraint::Length(10),
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

fn draw_movement_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    app.movement_detail_area = area;

    let lines = snapshot
        .movements
        .get(app.movements_state.selected().unwrap_or_default())
        .map(|movement| build_movement_detail_lines(snapshot, movement, theme))
        .unwrap_or_else(|| {
            vec![
                Line::from(Span::styled(
                    "No movement selected.",
                    Style::default().fg(theme.muted),
                )),
                Line::from(Span::styled(
                    "Use j/k to choose a movement, Shift+J/Shift+K or the mouse wheel to scroll this pane.",
                    Style::default().fg(theme.muted),
                )),
            ]
        });

    let content_height = area.height.saturating_sub(2) as usize;
    let content_width = area.width.saturating_sub(2).max(1);
    let total_lines = wrapped_line_count(&lines, content_width);
    let max_scroll = total_lines.saturating_sub(content_height) as u16;
    if app.movement_detail_scroll > max_scroll {
        app.movement_detail_scroll = max_scroll;
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.movement_detail_scroll, 0))
            .block(
                Block::default()
                    .title(Line::from(Span::styled(
                        " Movement Detail ",
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    )))
                    .title(
                        Line::from(vec![
                            Span::styled("Shift+J/K", Style::default().fg(theme.highlight)),
                            Span::styled(" scroll ", Style::default().fg(theme.muted)),
                        ])
                        .right_aligned(),
                    )
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );

    if total_lines > content_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .position(app.movement_detail_scroll as usize)
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

fn movement_kind_label(kind: polyphony_core::MovementKind) -> &'static str {
    match kind {
        polyphony_core::MovementKind::IssueDelivery => "issue",
        polyphony_core::MovementKind::PullRequestReview => "pr_review",
        polyphony_core::MovementKind::PullRequestCommentReview => "pr_comment",
    }
}

fn movement_target_label(movement: &polyphony_core::MovementRow) -> String {
    if let Some(target) = &movement.review_target {
        format!("{}#{}", target.repository, target.number)
    } else {
        movement.issue_identifier.clone().unwrap_or_default()
    }
}

fn build_movement_detail_lines(
    snapshot: &RuntimeSnapshot,
    movement: &polyphony_core::MovementRow,
    theme: crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            movement_kind_label(movement.kind).to_string(),
            Style::default().fg(theme.highlight),
        ),
        Span::styled(" | ", Style::default().fg(theme.border)),
        Span::styled(
            movement_target_label(movement),
            Style::default().fg(theme.info),
        ),
        Span::styled(" | ", Style::default().fg(theme.border)),
        Span::styled(
            movement.status.to_string(),
            Style::default().fg(movement_status_color(&movement.status, theme)),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        movement.title.clone(),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::default());

    lines.push(section_heading("Summary", theme));
    lines.push(Line::from(vec![
        Span::styled("Tasks ", Style::default().fg(theme.muted)),
        Span::styled(
            format!("{}/{}", movement.tasks_completed, movement.task_count),
            Style::default().fg(theme.foreground),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Output ", Style::default().fg(theme.muted)),
        Span::styled(
            movement_output_label(movement),
            Style::default().fg(theme.foreground),
        ),
    ]));
    if let Some(workspace_key) = &movement.workspace_key {
        lines.push(Line::from(vec![
            Span::styled("Workspace ", Style::default().fg(theme.muted)),
            Span::styled(workspace_key.clone(), Style::default().fg(theme.info)),
        ]));
    }
    if let Some(workspace_path) = &movement.workspace_path {
        lines.push(Line::from(vec![
            Span::styled("Path ", Style::default().fg(theme.muted)),
            Span::styled(
                workspace_path.display().to_string(),
                Style::default().fg(theme.foreground),
            ),
        ]));
    }

    if let Some(target) = &movement.review_target {
        lines.push(Line::default());
        lines.push(section_heading("Review Target", theme));
        lines.push(Line::from(vec![
            Span::styled("Repository ", Style::default().fg(theme.muted)),
            Span::styled(target.repository.clone(), Style::default().fg(theme.info)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("PR ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("#{}", target.number),
                Style::default().fg(theme.info),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Head SHA ", Style::default().fg(theme.muted)),
            Span::styled(
                target.head_sha.clone(),
                Style::default().fg(theme.foreground),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Branches ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("{} -> {}", target.head_branch, target.base_branch),
                Style::default().fg(theme.foreground),
            ),
        ]));
        if let Some(url) = &target.url {
            lines.push(Line::from(vec![
                Span::styled("URL ", Style::default().fg(theme.muted)),
                Span::styled(url.clone(), Style::default().fg(theme.info)),
            ]));
        }
    }

    lines.push(Line::default());
    lines.push(section_heading("Recent Events", theme));
    let movement_identifier = movement_target_label(movement);
    let mut event_count = 0usize;
    for event in snapshot.recent_events.iter().rev() {
        if !event_mentions_movement(event, movement, &movement_identifier) {
            continue;
        }
        event_count += 1;
        lines.push(render_event_line(event, theme));
    }
    if event_count == 0 {
        lines.push(Line::from(Span::styled(
            "No recent events for this movement.",
            Style::default().fg(theme.muted),
        )));
    }

    lines
}

fn movement_output_label(movement: &polyphony_core::MovementRow) -> &'static str {
    if movement.has_deliverable {
        "deliverable"
    } else if matches!(
        movement.kind,
        polyphony_core::MovementKind::PullRequestReview
    ) {
        "PR comment"
    } else {
        "none"
    }
}

fn section_heading(title: &str, theme: crate::theme::Theme) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    ))
}

fn event_mentions_movement(
    event: &polyphony_core::RuntimeEvent,
    movement: &polyphony_core::MovementRow,
    movement_identifier: &str,
) -> bool {
    if message_mentions_identifier(&event.message, movement_identifier) {
        return true;
    }
    movement
        .review_target
        .as_ref()
        .and_then(|target| target.url.as_deref())
        .is_some_and(|url| event.message.contains(url))
}

fn render_event_line(
    event: &polyphony_core::RuntimeEvent,
    theme: crate::theme::Theme,
) -> Line<'static> {
    let ts = event.at.format("%H:%M:%S");
    let scope_color = match event.scope {
        polyphony_core::EventScope::Dispatch => theme.info,
        polyphony_core::EventScope::Handoff => theme.highlight,
        polyphony_core::EventScope::Worker | polyphony_core::EventScope::Agent => theme.success,
        polyphony_core::EventScope::Retry => theme.warning,
        polyphony_core::EventScope::Throttle => theme.danger,
        _ => theme.muted,
    };

    Line::from(vec![
        Span::styled(format!("{ts} "), Style::default().fg(theme.muted)),
        Span::styled(
            format!("{:<10}", event.scope),
            Style::default().fg(scope_color),
        ),
        Span::styled(event.message.clone(), Style::default().fg(theme.foreground)),
    ])
}

fn message_mentions_identifier(message: &str, identifier: &str) -> bool {
    if identifier.is_empty() {
        return false;
    }

    let mut start = 0usize;
    while let Some(offset) = message[start..].find(identifier) {
        let matched = start + offset;
        let end = matched + identifier.len();
        let before_ok = matched == 0
            || !message[..matched]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_ascii_alphanumeric());
        let after_ok = end == message.len()
            || !message[end..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = end;
    }

    false
}

fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> usize {
    let width = width.max(1) as usize;
    lines
        .iter()
        .map(|line| {
            let text_width = line.width();
            if text_width == 0 {
                1
            } else {
                text_width.div_ceil(width)
            }
        })
        .sum()
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

    let content_height = area.height.saturating_sub(2) as usize; // border top+bottom

    // Reverse iteration: oldest first, newest at bottom (log-style)
    let lines: Vec<Line> = snapshot
        .recent_events
        .iter()
        .rev()
        .map(|event| render_event_line(event, theme))
        .collect();

    let total_lines = wrapped_line_count(&lines, area.width.saturating_sub(2).max(1));

    // Auto-scroll to bottom (newest) when new events arrive.
    // Follow new events if user was already at/near the bottom, or if
    // everything fits on screen (no scrollbar).
    let max_scroll = total_lines.saturating_sub(content_height) as u16;
    let was_at_bottom = app.events_scroll >= max_scroll.saturating_sub(1);
    if was_at_bottom || total_lines <= content_height {
        app.events_scroll = max_scroll;
    }
    if app.events_scroll > max_scroll {
        app.events_scroll = max_scroll;
    }

    let count_label = format!(" {} events ", snapshot.recent_events.len());

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
                        Line::from(Span::styled(count_label, Style::default().fg(theme.muted)))
                            .right_aligned(),
                    )
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border)),
            ),
        area,
    );

    // Scrollbar
    if total_lines > content_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines)
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

#[cfg(test)]
mod tests {
    use polyphony_core::{
        MovementKind, MovementRow, MovementStatus, ReviewProviderKind, ReviewTarget,
    };

    use crate::render::orchestrator::{
        message_mentions_identifier, movement_target_label, wrapped_line_count,
    };

    #[test]
    fn message_mentions_identifier_respects_boundaries() {
        assert!(message_mentions_identifier(
            "reviewed penso/polyphony#123 successfully",
            "penso/polyphony#123",
        ));
        assert!(!message_mentions_identifier(
            "reviewed penso/polyphony#1234 successfully",
            "penso/polyphony#123",
        ));
    }

    #[test]
    fn movement_target_label_prefers_review_target() {
        let movement = MovementRow {
            id: "movement-1".to_string(),
            kind: MovementKind::PullRequestReview,
            issue_identifier: Some("xm7".to_string()),
            title: "Review PR".to_string(),
            status: MovementStatus::InProgress,
            task_count: 0,
            tasks_completed: 0,
            has_deliverable: false,
            review_target: Some(ReviewTarget {
                provider: ReviewProviderKind::Github,
                repository: "penso/polyphony".to_string(),
                number: 123,
                url: None,
                base_branch: "main".to_string(),
                head_branch: "feature".to_string(),
                head_sha: "abc123".to_string(),
                checkout_ref: Some("refs/pull/123/head".to_string()),
            }),
            workspace_key: None,
            workspace_path: None,
            created_at: chrono::Utc::now(),
        };

        assert_eq!(movement_target_label(&movement), "penso/polyphony#123");
    }

    #[test]
    fn wrapped_line_count_accounts_for_wrapping() {
        let lines = vec![ratatui::text::Line::from("0123456789")];

        assert_eq!(wrapped_line_count(&lines, 4), 3);
    }
}
