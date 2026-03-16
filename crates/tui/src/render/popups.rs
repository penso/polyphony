use {
    chrono::{DateTime, Utc},
    polyphony_core::{
        DeliverableDecision, DispatchMode, IssueApprovalState, MovementRow,
        RuntimeSnapshot, TaskCategory, TaskRow, TaskStatus, VisibleTriggerRow,
    },
    ratatui::{
        layout::{Constraint, Direction, Layout, Margin, Rect},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
            Wrap,
        },
    },
};

use crate::{app::AppState, theme::Theme};

pub fn draw_issue_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    issue: &VisibleTriggerRow,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let max_w = frame.area().width.saturating_sub(4).min(100);
    let max_h = frame.area().height.saturating_sub(2).min(40);
    let area = centered_rect(frame.area(), max_w, max_h);
    frame.render_widget(Clear, area);

    let source_label = format!("{} {}", issue.source, issue.kind);

    // Border title: source + identifier
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(format!(" {source_label} "), Style::default().fg(theme.info)),
            Span::styled(
                format!("{} ", issue.trigger_id),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(Line::from(modal_hint_spans(issue, theme)).right_aligned())
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.panel_alt));

    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    // Compute title height: wrap title into available width
    let title_width = inner.width as usize;
    let title_lines_count = (if title_width > 0 {
        issue.title.len().div_ceil(title_width)
    } else {
        1
    })
    .clamp(1, 3); // cap at 3 lines

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(title_lines_count as u16), // Title (wraps) + created time
            Constraint::Length(1), // Status, priority, labels, author, updated
            Constraint::Length(1), // Indicator legend
            Constraint::Length(1), // Separator
            Constraint::Min(1),    // Description
        ])
        .split(inner);

    // Row 1: Title (wrapping) with created time right-aligned on first line
    let state_color = super::triggers::state_color(&issue.status, theme);
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

    let created_str = issue
        .created_at
        .map(|c| format_relative_time(c, Utc::now()))
        .unwrap_or_default();

    if !created_str.is_empty() {
        let time_label = format!(" {created_str} ago ");
        let time_len = time_label.len() as u16;

        let title_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(time_len)])
            .split(rows[0]);

        frame.render_widget(
            Paragraph::new(Span::styled(
                issue.title.clone(),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ))
            .wrap(Wrap { trim: false }),
            title_cols[0],
        );

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                time_label,
                Style::default().fg(theme.muted),
            )))
            .alignment(ratatui::layout::Alignment::Right),
            title_cols[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                issue.title.clone(),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ))
            .wrap(Wrap { trim: false }),
            rows[0],
        );
    }

    // Row 2: Status | Priority | Labels | Author | Updated
    let mut meta_spans: Vec<Span<'_>> = Vec::new();

    meta_spans.push(Span::styled(
        format!(" {} ", issue.status),
        Style::default().fg(state_color),
    ));
    meta_spans.push(Span::styled("  ", Style::default()));
    meta_spans.push(Span::styled(
        format!("approval {}", issue.approval_state),
        Style::default().fg(match issue.approval_state {
            IssueApprovalState::Approved => theme.success,
            IssueApprovalState::Waiting => theme.warning,
        }),
    ));
    meta_spans.push(Span::styled("  ", Style::default()));
    meta_spans.push(Span::styled(
        priority_str,
        Style::default().fg(priority_color),
    ));

    if !issue.labels.is_empty() {
        meta_spans.push(Span::styled("  ", Style::default()));
        for (i, label) in issue.labels.iter().enumerate() {
            if i > 0 {
                meta_spans.push(Span::styled(" ", Style::default()));
            }
            meta_spans.push(Span::styled(
                label.clone(),
                Style::default().fg(label_color(label, theme)),
            ));
        }
    }

    if let Some(author) = &issue.author {
        meta_spans.push(Span::styled("  ", Style::default()));
        meta_spans.push(Span::styled(
            format!("@{author}"),
            Style::default().fg(theme.highlight),
        ));
    }

    if let Some(updated) = issue.updated_at {
        let age = format_relative_time(updated, Utc::now());
        meta_spans.push(Span::styled("  ", Style::default()));
        meta_spans.push(Span::styled(
            format!("updated {age} ago"),
            Style::default().fg(theme.muted),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(meta_spans)), rows[1]);

    // Indicator legend row
    let is_running = snapshot
        .running
        .iter()
        .any(|r| r.issue_id == issue.trigger_id);
    let mut legend_spans: Vec<Span<'_>> = Vec::new();
    if is_running {
        legend_spans.push(Span::styled("⠋ ", Style::default().fg(theme.highlight)));
        legend_spans.push(Span::styled("running", Style::default().fg(theme.muted)));
    } else if issue.has_workspace {
        legend_spans.push(Span::styled("● ", Style::default().fg(theme.highlight)));
        legend_spans.push(Span::styled("workspace active", Style::default().fg(theme.muted)));
    } else {
        legend_spans.push(Span::styled("  no workspace", Style::default().fg(theme.muted)));
    }
    frame.render_widget(Paragraph::new(Line::from(legend_spans)), rows[2]);

    // Separator
    render_separator(frame, rows[3], inner.width, theme);

    // Description with markdown rendering and scroll.
    // Strip HTML tags first — GitHub issues often contain <details>, <img>, etc.
    // which tui-markdown doesn't support and would log warnings for.
    let desc_raw = issue.description.as_deref().unwrap_or("No description.");
    let desc_cleaned = strip_html_tags(desc_raw);

    let desc_widget = tui_markdown::from_str(&desc_cleaned);
    let desc_area = rows[4];
    let visible_height = desc_area.height as usize;
    let total_lines = desc_widget.lines.len();

    let max_scroll = total_lines.saturating_sub(visible_height);
    if (app.detail_scroll as usize) > max_scroll {
        app.detail_scroll = max_scroll as u16;
    }

    frame.render_widget(
        Paragraph::new(desc_widget)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        desc_area,
    );

    if total_lines > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll).position(app.detail_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            desc_area,
            &mut scrollbar_state,
        );
    }
}

/// Pick a color for a label based on common keywords.
fn label_color(label: &str, theme: Theme) -> Color {
    match label.to_ascii_lowercase().as_str() {
        "bug" | "defect" => theme.danger,
        "feature" | "enhancement" => theme.success,
        "documentation" | "docs" => theme.info,
        "good first issue" | "help wanted" => Color::Cyan,
        "priority" | "urgent" | "critical" => theme.warning,
        "wontfix" | "invalid" | "duplicate" => theme.muted,
        _ => theme.foreground,
    }
}

fn format_relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(dt).num_seconds().max(0) as u64;
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 604800 {
        format!("{}d", secs / 86400)
    } else if secs < 2_592_000 {
        format!("{}w", secs / 604800)
    } else {
        format!("{}mo", secs / 2_592_000)
    }
}

fn modal_hint_spans<'a>(issue: &VisibleTriggerRow, theme: Theme) -> Vec<Span<'a>> {
    let mut spans = vec![
        Span::styled(" j/k", Style::default().fg(theme.highlight)),
        Span::styled(":scroll  ", Style::default().fg(theme.muted)),
        Span::styled("o", Style::default().fg(theme.highlight)),
        Span::styled(":open  ", Style::default().fg(theme.muted)),
    ];
    if issue.kind == polyphony_core::VisibleTriggerKind::Issue {
        spans.push(Span::styled("d", Style::default().fg(theme.highlight)));
        spans.push(Span::styled(
            ":dispatch  ",
            Style::default().fg(theme.muted),
        ));
        if issue.approval_state == IssueApprovalState::Waiting {
            spans.push(Span::styled("a", Style::default().fg(theme.highlight)));
            spans.push(Span::styled(":approve  ", Style::default().fg(theme.muted)));
        }
    }
    spans.push(Span::styled("Esc", Style::default().fg(theme.highlight)));
    spans.push(Span::styled(":close ", Style::default().fg(theme.muted)));
    spans
}

pub fn draw_leaving_modal(frame: &mut ratatui::Frame<'_>, theme: Theme) {
    let full_area = frame.area();
    frame.render_widget(Clear, full_area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background)),
        full_area,
    );
    let area = centered_rect(full_area, 24, 3);
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

pub fn draw_task_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    task: &TaskRow,
    app: &mut AppState,
) {
    let theme = app.theme;
    let area = centered_rect(
        frame.area(),
        frame.area().width.saturating_sub(4).min(92),
        frame.area().height.saturating_sub(2).min(32),
    );
    frame.render_widget(Clear, area);

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
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
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

    let title_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

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

    let status_color = task_status_color(task.status, theme);
    let meta = Line::from(vec![
        Span::styled(task_status_icon(task.status), Style::default().fg(status_color)),
        Span::styled(" ", Style::default().fg(theme.muted)),
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

    let mut lines = vec![
        kv_line("ID", &task.id, theme),
        kv_line("Flow", &task.movement_id, theme),
        kv_line("Turns", &task.turns_completed.to_string(), theme),
        kv_line("Tokens", &task.total_tokens.to_string(), theme),
        kv_line("Created", &format_absolute_time(task.created_at), theme),
        kv_line("Updated", &format_absolute_time(task.updated_at), theme),
    ];

    if let Some(started_at) = task.started_at {
        lines.push(kv_line("Started", &format_absolute_time(started_at), theme));
    }
    if let Some(finished_at) = task.finished_at {
        lines.push(kv_line(
            "Finished",
            &format_absolute_time(finished_at),
            theme,
        ));
    }
    if let Some(description) = &task.description
        && !description.trim().is_empty()
    {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Description",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
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
            Style::default()
                .fg(theme.danger)
                .add_modifier(Modifier::BOLD),
        )));
        for line in error.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.warning),
            )));
        }
    }

    let visible_height = title_rows[3].height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    if app.task_detail_scroll as usize > max_scroll {
        app.task_detail_scroll = max_scroll as u16;
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.task_detail_scroll, 0)),
        title_rows[3],
    );

    if total_lines > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll).position(app.task_detail_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            title_rows[3],
            &mut scrollbar_state,
        );
    }
}

fn kv_line<'a>(label: &'static str, value: &str, theme: Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<8}"), Style::default().fg(theme.muted)),
        Span::styled(value.to_string(), Style::default().fg(theme.foreground)),
    ])
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

fn task_status_color(status: TaskStatus, theme: Theme) -> Color {
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

const MODE_OPTIONS: [(DispatchMode, &str, &str); 4] = [
    (
        DispatchMode::Manual,
        "Manual",
        "You choose which issues to dispatch",
    ),
    (
        DispatchMode::Automatic,
        "Automatic",
        "Issues are dispatched automatically",
    ),
    (
        DispatchMode::Nightshift,
        "Nightshift",
        "Auto + code improvements when idle",
    ),
    (
        DispatchMode::Idle,
        "Idle",
        "Only opportunistic dispatch when idle and budgets say there is headroom",
    ),
];

pub fn draw_mode_modal(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot, app: &AppState) {
    let theme = app.theme;
    let modal_height =
        u16::try_from(MODE_OPTIONS.len().saturating_mul(4).saturating_add(2)).unwrap_or(u16::MAX);
    let area = centered_rect(frame.area(), 52, modal_height);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Dispatch Mode ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled(" j/k", Style::default().fg(theme.highlight)),
                Span::styled(":navigate  ", Style::default().fg(theme.muted)),
                Span::styled("Enter", Style::default().fg(theme.highlight)),
                Span::styled(":select  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.background));

    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    let mut constraints =
        Vec::with_capacity(MODE_OPTIONS.len().saturating_mul(2).saturating_add(2));
    constraints.push(Constraint::Length(1));
    for index in 0..MODE_OPTIONS.len() {
        constraints.push(Constraint::Length(3));
        if index + 1 != MODE_OPTIONS.len() {
            constraints.push(Constraint::Length(1));
        }
    }
    constraints.push(Constraint::Min(0));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, (mode, label, desc)) in MODE_OPTIONS.iter().enumerate() {
        let is_selected = i == app.mode_modal_selected;
        let is_active = *mode == snapshot.dispatch_mode;

        let marker = if is_active {
            "● "
        } else {
            "  "
        };
        let marker_color = if is_active {
            theme.success
        } else {
            theme.muted
        };

        let label_style = if is_selected {
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };

        let row_area = rows[1 + i * 2];
        let row_style = if is_selected {
            Style::default().bg(theme.panel_alt)
        } else {
            Style::default()
        };

        frame.render_widget(Block::default().style(row_style), row_area);

        let row_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(row_area);
        let desc_columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Fill(1)])
            .split(row_sections[1]);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::styled(*label, label_style),
            ]))
            .style(row_style),
            row_sections[0],
        );
        frame.render_widget(
            Paragraph::new(*desc)
                .style(row_style.fg(theme.muted))
                .wrap(Wrap { trim: false }),
            desc_columns[1],
        );
    }
}

pub fn draw_agent_picker_modal(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    let profile_count = snapshot.agent_profile_names.len();
    // Height: 1 border top + 1 spacer + (profile_count * 1) + 1 spacer + 1 border bottom
    let content_height = (profile_count as u16).clamp(1, 10);
    let total_height = content_height + 4; // borders + spacers
    let area = centered_rect(frame.area(), 48, total_height);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Dispatch to Agent ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled(" j/k", Style::default().fg(theme.highlight)),
                Span::styled(":navigate  ", Style::default().fg(theme.muted)),
                Span::styled("Enter", Style::default().fg(theme.highlight)),
                Span::styled(":dispatch  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.background));

    frame.render_widget(block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    for (i, name) in snapshot.agent_profile_names.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let is_selected = i == app.agent_picker_selected;
        let row_area = Rect {
            x: inner.x,
            y: inner.y + i as u16,
            width: inner.width,
            height: 1,
        };

        let marker = if is_selected {
            "▸ "
        } else {
            "  "
        };
        let label_style = if is_selected {
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };

        let bg = if is_selected {
            Style::default().bg(theme.panel_alt)
        } else {
            Style::default()
        };

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.highlight)),
                Span::styled(name.clone(), label_style),
            ]))
            .style(bg),
            row_area,
        );
    }
}

fn format_absolute_time(dt: DateTime<Utc>) -> String {
    super::format_detail_time(dt)
}

pub fn draw_movement_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    movement: &MovementRow,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let area = centered_rect(
        frame.area(),
        frame.area().width.saturating_sub(4).min(100),
        frame.area().height.saturating_sub(2).min(40),
    );
    frame.render_widget(Clear, area);

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
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("Shift+O", Style::default().fg(theme.highlight)),
                Span::styled(":open  ", Style::default().fg(theme.muted)),
                Span::styled("a", Style::default().fg(theme.highlight)),
                Span::styled(":accept  ", Style::default().fg(theme.muted)),
                Span::styled("x", Style::default().fg(theme.highlight)),
                Span::styled(":reject  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
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
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

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

    let status_color =
        super::orchestrator::movement_status_color_pub(&movement.status, theme);
    let meta = Line::from(vec![
        Span::styled(
            movement.status.to_string(),
            Style::default().fg(status_color),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            super::orchestrator::movement_target_label(movement),
            Style::default().fg(theme.info),
        ),
    ]);
    frame.render_widget(Paragraph::new(meta), rows[1]);
    render_separator(frame, rows[2], inner.width, theme);

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
        kv_line(
            "Tasks",
            &format!("{}/{}", movement.tasks_completed, movement.task_count),
            theme,
        ),
        kv_line("Created", &format_absolute_time(movement.created_at), theme),
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

    // Recent events for this movement
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Recent Events",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));
    let movement_identifier = super::orchestrator::movement_target_label(movement);
    let mut event_count = 0usize;
    for event in snapshot.recent_events.iter().rev() {
        if !super::orchestrator::event_mentions_movement_pub(
            event,
            movement,
            &movement_identifier,
        ) {
            continue;
        }
        event_count += 1;
        lines.push(super::orchestrator::render_event_line_pub(event, theme));
    }
    if event_count == 0 {
        lines.push(Line::from(Span::styled(
            "No recent events for this movement.",
            Style::default().fg(theme.muted),
        )));
    }

    let visible_height = rows[3].height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    if app.movement_detail_scroll as usize > max_scroll {
        app.movement_detail_scroll = max_scroll as u16;
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.movement_detail_scroll, 0)),
        rows[3],
    );

    if total_lines > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll).position(app.movement_detail_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            rows[3],
            &mut scrollbar_state,
        );
    }
}

pub fn draw_deliverable_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    movement: &MovementRow,
    app: &mut AppState,
) {
    let theme = app.theme;
    let area = centered_rect(
        frame.area(),
        frame.area().width.saturating_sub(4).min(92),
        frame.area().height.saturating_sub(2).min(32),
    );
    frame.render_widget(Clear, area);

    let deliverable = movement.deliverable.as_ref().expect("selected deliverable");
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" Outcome ", Style::default().fg(theme.info)),
            Span::styled(
                format!(
                    "{} ",
                    super::deliverables::deliverable_label_pub(deliverable)
                ),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("Shift+O", Style::default().fg(theme.highlight)),
                Span::styled(":open  ", Style::default().fg(theme.muted)),
                Span::styled("a", Style::default().fg(theme.highlight)),
                Span::styled(":accept  ", Style::default().fg(theme.muted)),
                Span::styled("x", Style::default().fg(theme.highlight)),
                Span::styled(":reject  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
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
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

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

    let decision_color = match deliverable.decision {
        DeliverableDecision::Waiting => theme.warning,
        DeliverableDecision::Accepted => theme.success,
        DeliverableDecision::Rejected => theme.danger,
    };
    let meta = Line::from(vec![
        Span::styled(
            deliverable.decision.to_string(),
            Style::default().fg(decision_color),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            super::deliverables::flow_label_pub(movement),
            Style::default().fg(theme.info),
        ),
    ]);
    frame.render_widget(Paragraph::new(meta), rows[1]);
    render_separator(frame, rows[2], inner.width, theme);

    let mut lines = vec![
        kv_line("ID", &movement.id, theme),
        kv_line(
            "Output",
            &super::deliverables::deliverable_label_pub(deliverable),
            theme,
        ),
        kv_line(
            "Decision",
            &deliverable.decision.to_string(),
            theme,
        ),
        kv_line("Created", &format_absolute_time(movement.created_at), theme),
    ];

    if let Some(url) = &deliverable.url {
        lines.push(kv_line("URL", url, theme));
    }
    if let Some(workspace_path) = &movement.workspace_path {
        lines.push(kv_line("Path", &workspace_path.display().to_string(), theme));
    }

    let visible_height = rows[3].height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    if app.deliverable_detail_scroll as usize > max_scroll {
        app.deliverable_detail_scroll = max_scroll as u16;
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.deliverable_detail_scroll, 0)),
        rows[3],
    );

    if total_lines > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll).position(app.deliverable_detail_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            rows[3],
            &mut scrollbar_state,
        );
    }
}

pub fn draw_agent_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let area = centered_rect(
        frame.area(),
        frame.area().width.saturating_sub(4).min(100),
        frame.area().height.saturating_sub(2).min(40),
    );
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Agent Detail ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":close ", Style::default().fg(theme.muted)),
            ])
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

    let lines = app
        .selected_agent(snapshot)
        .map(|agent| {
            super::agents::build_agent_detail_lines(
                snapshot,
                agent,
                app.agent_detail_artifact
                    .as_ref()
                    .and_then(|artifact| artifact.saved_context.as_ref()),
                theme,
            )
        })
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "No agent run selected.",
                Style::default().fg(theme.muted),
            ))]
        });

    let visible_height = inner.height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    if app.agents_detail_scroll as usize > max_scroll {
        app.agents_detail_scroll = max_scroll as u16;
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.agents_detail_scroll, 0)),
        inner,
    );

    if total_lines > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll).position(app.agents_detail_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            inner,
            &mut scrollbar_state,
        );
    }
}

/// Strip HTML tags from text, preserving content between tags.
fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {},
        }
    }
    out
}
