use chrono::Utc;
use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, Wrap,
    },
};

/// Build a child tree row with 2 cells: empty time + full-width title.
fn is_noise_event_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower == "usage updated" || lower.starts_with("turn ") && lower.ends_with(" usage updated")
}

fn run_total_tokens(snapshot: &polyphony_core::RuntimeSnapshot, run_id: &str) -> u64 {
    let task_tokens: u64 = snapshot
        .tasks
        .iter()
        .filter(|t| t.run_id == run_id)
        .map(|t| t.total_tokens)
        .sum();
    // Also include tokens from currently running agents for this run's issue
    let issue_id = snapshot.runs.iter().find(|m| m.id == run_id).and_then(|m| {
        m.issue_identifier
            .as_ref()
            .and_then(|ident| {
                snapshot
                    .tracker_issues
                    .iter()
                    .find(|i| i.issue_identifier == *ident)
            })
            .map(|i| i.issue_id.as_str())
    });
    let running_tokens: u64 = issue_id
        .map(|id| {
            snapshot
                .running
                .iter()
                .filter(|r| r.issue_id == id)
                .map(|r| r.tokens.total_tokens)
                .sum()
        })
        .unwrap_or(0);
    task_tokens + running_tokens
}

fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn child_row<'a>(
    indent: usize,
    connector: &str,
    theme: crate::theme::Theme,
    title: Line<'a>,
) -> Row<'a> {
    let pad = " ".repeat(indent);
    let mut spans = vec![Span::styled(
        format!("{pad}{connector} "),
        Style::default().fg(theme.border),
    )];
    spans.extend(title.spans);
    Row::new(vec![Cell::from(Line::from(spans))])
}

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
            Constraint::Min(8),    // Runs table (fills remaining space)
        ])
        .split(area);

    draw_status_panel(frame, sections[0], snapshot, app);
    draw_runs_table(frame, sections[1], snapshot, app);
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
            Span::styled("Runs     ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.counts.runs.to_string(),
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
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.panel)),
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

fn draw_runs_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    use crate::app::OrchestratorTreeRow;

    let theme = app.theme;
    let compact = area.width < 80;
    // Indent child rows to align under the date in run rows.
    // Run row: "{collapse_icon}{time_label} {status_icon} {title}"
    // collapse_icon is 2 chars ("▶ " or "  "), so children indent by 2.
    let child_indent: usize = 2;
    // Dynamic truncation: leave room for border (2), indent, connector (3), padding.
    // For inline excerpts (appended after other text), use ~half the available width.
    let available_width = (area.width as usize).saturating_sub(4); // borders
    let excerpt_width = available_width.saturating_sub(child_indent + 25); // connector + label overhead
    let inline_excerpt_width = available_width / 2;

    // Build task lookup for collapse indicator
    let has_tasks: std::collections::HashSet<&str> =
        snapshot.tasks.iter().map(|t| t.run_id.as_str()).collect();

    // Filter tree rows by search query (run-level matching).
    let search_q = app.runs_search_query.to_lowercase();
    let filtered_rows: Vec<&OrchestratorTreeRow> = if search_q.is_empty() {
        app.orchestrator_tree_rows.iter().collect()
    } else {
        // Find matching run snapshot indices
        let matching_runs: std::collections::HashSet<usize> = app
            .orchestrator_tree_rows
            .iter()
            .filter_map(|row| match row {
                OrchestratorTreeRow::Run { snapshot_index } => {
                    let m = &snapshot.runs[*snapshot_index];
                    if m.title.to_lowercase().contains(&search_q)
                        || m.issue_identifier
                            .as_deref()
                            .is_some_and(|id| id.to_lowercase().contains(&search_q))
                        || m.status.to_string().to_lowercase().contains(&search_q)
                    {
                        Some(*snapshot_index)
                    } else {
                        None
                    }
                },
                _ => None,
            })
            .collect();
        app.orchestrator_tree_rows
            .iter()
            .filter(|row| match row {
                OrchestratorTreeRow::Run { snapshot_index } => {
                    matching_runs.contains(snapshot_index)
                },
                OrchestratorTreeRow::InboxItem { .. }
                | OrchestratorTreeRow::Progress { .. }
                | OrchestratorTreeRow::AgentSession { .. }
                | OrchestratorTreeRow::RunningAgent { .. }
                | OrchestratorTreeRow::AgentLogLine { .. } => {
                    // Child rows follow their run; include if parent matches
                    true
                },
                OrchestratorTreeRow::Task { snapshot_index, .. } => {
                    let task = &snapshot.tasks[*snapshot_index];
                    app.sorted_run_indices.iter().any(|&mi| {
                        matching_runs.contains(&mi) && snapshot.runs[mi].id == task.run_id
                    })
                },
                OrchestratorTreeRow::LogEntry {
                    run_snapshot_index, ..
                }
                | OrchestratorTreeRow::Outcome { run_snapshot_index } => {
                    matching_runs.contains(run_snapshot_index)
                },
            })
            .collect()
    };

    let count = filtered_rows.len();
    if count == 0 {
        app.runs_state.select(None);
    } else if app.runs_state.selected().is_none_or(|i| i >= count) {
        app.runs_state.select(Some(count - 1));
    }

    let rows: Vec<Row> = filtered_rows
        .iter()
        .map(|row| match row {
            OrchestratorTreeRow::Run { snapshot_index } => {
                let m = &snapshot.runs[*snapshot_index];
                let (status_icon, status_icon_color) = run_status_emoji(&m.status, theme);
                let task_info = format!("{}/{}", m.tasks_completed, m.task_count);
                let (output_icon, output_icon_color) = run_output_emoji(m, theme);

                // Aggregate total tokens across all tasks and running agents for this run
                let run_tokens = run_total_tokens(snapshot, &m.id);

                let collapse_icon = if has_tasks.contains(m.id.as_str()) {
                    if app.collapsed_runs.contains(&m.id) {
                        "▶ "
                    } else {
                        "▼ "
                    }
                } else {
                    "  "
                };

                let time_label = if compact {
                    m.created_at
                        .with_timezone(&chrono::Local)
                        .format("%H:%M")
                        .to_string()
                } else {
                    super::format_listing_time(m.created_at)
                };

                let mut title_spans = vec![
                    Span::styled(
                        format!("{status_icon} "),
                        Style::default().fg(status_icon_color),
                    ),
                    Span::styled(m.title.clone(), Style::default().fg(theme.foreground)),
                ];
                if !output_icon.is_empty() {
                    title_spans.push(Span::styled(
                        format!(" {output_icon}"),
                        Style::default().fg(output_icon_color),
                    ));
                }
                title_spans.push(Span::styled(
                    format!("  {task_info}"),
                    Style::default().fg(theme.muted),
                ));
                if run_tokens > 0 {
                    title_spans.push(Span::styled(
                        format!(" {}", format_token_count(run_tokens)),
                        Style::default().fg(theme.muted),
                    ));
                }
                // Show cancel reason inline for cancelled runs
                if m.status == polyphony_core::RunStatus::Cancelled
                    && let Some(reason) = &m.cancel_reason
                {
                    let excerpt = if reason.len() > inline_excerpt_width {
                        let end =
                            reason.floor_char_boundary(inline_excerpt_width.saturating_sub(5));
                        format!(" — {}…", &reason[..end])
                    } else {
                        format!(" — {reason}")
                    };
                    title_spans.push(Span::styled(excerpt, Style::default().fg(theme.warning)));
                }
                let mut spans = vec![Span::styled(
                    format!("{collapse_icon}{time_label} "),
                    Style::default().fg(theme.muted),
                )];
                spans.extend(title_spans);
                Row::new(vec![Cell::from(Line::from(spans))])
            },
            OrchestratorTreeRow::InboxItem {
                item_index,
                run_snapshot_index,
                is_last_child,
            } => {
                let item = &snapshot.inbox_items[*item_index];
                let run = &snapshot.runs[*run_snapshot_index];
                let connector = if *is_last_child {
                    "└─ "
                } else {
                    "├─ "
                };
                let (status_icon, status_color) =
                    super::inbox::status_emoji_pub(&item.status, theme);
                // Show identifier when the title matches the run, full title otherwise
                let display_title = if item.title == run.title {
                    item.identifier.clone()
                } else {
                    item.title.clone()
                };
                child_row(
                    child_indent,
                    connector,
                    theme,
                    Line::from(vec![
                        Span::styled(format!("{status_icon} "), Style::default().fg(status_color)),
                        Span::styled("inbox ", Style::default().fg(theme.muted)),
                        Span::styled(display_title, Style::default().fg(theme.highlight)),
                    ]),
                )
            },
            OrchestratorTreeRow::Progress {
                run_snapshot_index,
                is_last_child,
            } => {
                let m = &snapshot.runs[*run_snapshot_index];
                let connector = if *is_last_child {
                    "└─ "
                } else {
                    "├─ "
                };
                let completed = m.tasks_completed;
                let total = m.task_count;
                let ratio = if total > 0 {
                    completed as f64 / total as f64
                } else {
                    0.0
                };
                // Build a text-based progress bar using Unicode blocks
                let bar_width = 20usize;
                let filled = (ratio * bar_width as f64).round() as usize;
                let empty = bar_width.saturating_sub(filled);
                let bar_filled: String = "█".repeat(filled);
                let bar_empty: String = "░".repeat(empty);
                let label = format!("{completed}/{total}");
                let status_color = run_status_color_pub(&m.status, theme);
                child_row(
                    child_indent,
                    connector,
                    theme,
                    Line::from(vec![
                        Span::styled(bar_filled, Style::default().fg(status_color)),
                        Span::styled(bar_empty, Style::default().fg(theme.border)),
                        Span::styled(format!(" {label}"), Style::default().fg(theme.muted)),
                    ]),
                )
            },
            OrchestratorTreeRow::Task {
                snapshot_index,
                is_last_child,
            } => {
                let task = &snapshot.tasks[*snapshot_index];
                let connector = if *is_last_child {
                    "└─ "
                } else {
                    "├─ "
                };
                let status_icon = super::tasks::task_status_icon(&task.status);
                let status_color = super::tasks::task_status_color(&task.status, theme);

                let mut title_spans = vec![
                    Span::styled(format!("{status_icon} "), Style::default().fg(status_color)),
                    Span::styled("task ", Style::default().fg(theme.muted)),
                    Span::styled(task.title.clone(), Style::default().fg(theme.foreground)),
                ];

                // Append agent session info inline
                if let Some(agent) = &task.agent_name {
                    title_spans.push(Span::styled(
                        format!("  {agent}"),
                        Style::default().fg(theme.info),
                    ));
                }
                if task.turns_completed > 0 {
                    title_spans.push(Span::styled(
                        format!(" {}t", task.turns_completed),
                        Style::default().fg(theme.muted),
                    ));
                }
                match (task.started_at, task.finished_at) {
                    (Some(start), Some(end)) => {
                        title_spans.push(Span::styled(
                            format!(
                                " {}",
                                super::agents::format_duration(end.signed_duration_since(start))
                            ),
                            Style::default().fg(theme.muted),
                        ));
                    },
                    (Some(start), None) => {
                        title_spans.push(Span::styled(
                            format!(
                                " {}",
                                super::agents::format_duration(
                                    chrono::Utc::now().signed_duration_since(start)
                                )
                            ),
                            Style::default().fg(theme.muted),
                        ));
                    },
                    _ => {},
                }

                child_row(child_indent, connector, theme, Line::from(title_spans))
            },
            OrchestratorTreeRow::AgentSession {
                history_index,
                is_last_child,
            } => {
                let session = &snapshot.agent_run_history[*history_index];
                let connector = if *is_last_child {
                    "└─ "
                } else {
                    "├─ "
                };
                let status_icon = match session.status {
                    polyphony_core::AttemptStatus::Succeeded => "✓",
                    polyphony_core::AttemptStatus::Failed => "✕",
                    polyphony_core::AttemptStatus::CancelledByReconciliation => "⊘",
                    _ => "●",
                };
                let status_color = match session.status {
                    polyphony_core::AttemptStatus::Succeeded => theme.success,
                    polyphony_core::AttemptStatus::Failed => theme.danger,
                    polyphony_core::AttemptStatus::CancelledByReconciliation => theme.warning,
                    _ => theme.muted,
                };
                let duration = super::agents::format_duration(
                    session
                        .finished_at
                        .unwrap_or_else(chrono::Utc::now)
                        .signed_duration_since(session.started_at),
                );

                let mut title_spans = vec![
                    Span::styled(format!("{status_icon} "), Style::default().fg(status_color)),
                    Span::styled("agent ", Style::default().fg(theme.muted)),
                    Span::styled(session.agent_name.clone(), Style::default().fg(theme.info)),
                ];
                if let Some(model) = &session.model {
                    title_spans.push(Span::styled(
                        format!(" ({model})"),
                        Style::default().fg(theme.muted),
                    ));
                }
                title_spans.push(Span::styled(
                    format!(" {duration}"),
                    Style::default().fg(theme.muted),
                ));
                if session.tokens.total_tokens > 0 {
                    title_spans.push(Span::styled(
                        format!(
                            " {}tok",
                            super::agents::format_tokens_pub(session.tokens.total_tokens)
                        ),
                        Style::default().fg(theme.muted),
                    ));
                }
                // Show error excerpt for failed sessions
                if session.status == polyphony_core::AttemptStatus::Failed
                    && let Some(error) = &session.error
                {
                    let excerpt = if error.len() > inline_excerpt_width {
                        let end = error.floor_char_boundary(inline_excerpt_width.saturating_sub(5));
                        format!(" — {}…", &error[..end])
                    } else {
                        format!(" — {error}")
                    };
                    title_spans.push(Span::styled(excerpt, Style::default().fg(theme.danger)));
                }
                // Show reason for cancelled sessions
                if session.status == polyphony_core::AttemptStatus::CancelledByReconciliation
                    && let Some(error) = &session.error
                {
                    let excerpt = if error.len() > inline_excerpt_width {
                        let end = error.floor_char_boundary(inline_excerpt_width.saturating_sub(5));
                        format!(" — {}…", &error[..end])
                    } else {
                        format!(" — {error}")
                    };
                    title_spans.push(Span::styled(excerpt, Style::default().fg(theme.warning)));
                }
                // Show last message for successful sessions
                if session.status == polyphony_core::AttemptStatus::Succeeded
                    && let Some(msg) = &session.last_message
                {
                    let excerpt = if msg.len() > inline_excerpt_width {
                        let end = msg.floor_char_boundary(inline_excerpt_width.saturating_sub(5));
                        format!(" — {}…", &msg[..end])
                    } else {
                        format!(" — {msg}")
                    };
                    title_spans.push(Span::styled(excerpt, Style::default().fg(theme.muted)));
                }

                child_row(child_indent, connector, theme, Line::from(title_spans))
            },
            OrchestratorTreeRow::RunningAgent {
                running_index,
                is_last_child,
            } => {
                let running = &snapshot.running[*running_index];
                let connector = if *is_last_child {
                    "└─ "
                } else {
                    "├─ "
                };
                let spinner_chars: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let spinner = spinner_chars[app.frame_count as usize % spinner_chars.len()];
                let duration = super::agents::format_duration(
                    chrono::Utc::now().signed_duration_since(running.started_at),
                );

                let mut title_spans = vec![
                    Span::styled(format!("{spinner} "), Style::default().fg(theme.info)),
                    Span::styled("agent ", Style::default().fg(theme.muted)),
                    Span::styled(running.agent_name.clone(), Style::default().fg(theme.info)),
                ];
                if let Some(model) = &running.model {
                    title_spans.push(Span::styled(
                        format!(" ({model})"),
                        Style::default().fg(theme.muted),
                    ));
                }
                title_spans.push(Span::styled(
                    format!(" {duration}"),
                    Style::default().fg(theme.muted),
                ));
                if running.tokens.total_tokens > 0 {
                    let token_label = super::agents::format_tokens_pub(running.tokens.total_tokens);
                    let direction = app
                        .prev_running_tokens
                        .get(&running.issue_id)
                        .map(|prev| {
                            if running.tokens.output_tokens > prev.output_tokens {
                                "\u{2193}" // ↓ receiving from LLM
                            } else if running.tokens.input_tokens > prev.input_tokens {
                                "\u{2191}" // ↑ sending to LLM
                            } else {
                                ""
                            }
                        })
                        .unwrap_or("");
                    title_spans.push(Span::styled(
                        format!(" {direction}{token_label}"),
                        Style::default().fg(theme.muted),
                    ));
                }
                // Show last event or message as status hint (skip noise like "usage updated")
                if let Some(msg) = running
                    .last_message
                    .as_deref()
                    .filter(|m| !is_noise_event_message(m))
                    .or(running.last_event.as_deref())
                {
                    let excerpt = if msg.len() > inline_excerpt_width {
                        let end = msg.floor_char_boundary(inline_excerpt_width.saturating_sub(5));
                        format!(" — {}…", &msg[..end])
                    } else {
                        format!(" — {msg}")
                    };
                    title_spans.push(Span::styled(excerpt, Style::default().fg(theme.muted)));
                }
                title_spans.push(Span::styled(
                    " (enter for detail)",
                    Style::default().fg(theme.muted),
                ));

                child_row(child_indent, connector, theme, Line::from(title_spans))
            },
            OrchestratorTreeRow::AgentLogLine {
                running_index,
                line_index,
                is_last_child,
            } => {
                let running = &snapshot.running[*running_index];
                let line = &running.recent_log[*line_index];
                let connector = if *is_last_child {
                    " └─"
                } else {
                    " ├─"
                };
                let excerpt = if line.len() > excerpt_width {
                    let end = line.floor_char_boundary(excerpt_width.saturating_sub(1));
                    format!("{}…", &line[..end])
                } else {
                    line.clone()
                };
                child_row(
                    child_indent + 2,
                    connector,
                    theme,
                    Line::from(vec![Span::styled(
                        excerpt,
                        Style::default().fg(theme.muted),
                    )]),
                )
            },
            OrchestratorTreeRow::LogEntry {
                log_index,
                run_snapshot_index,
                is_last_child,
            } => {
                let m = &snapshot.runs[*run_snapshot_index];
                let entry = &m.activity_log[*log_index];
                let connector = if *is_last_child {
                    "└─ "
                } else {
                    "├─ "
                };
                let time_label = entry
                    .at
                    .with_timezone(&chrono::Local)
                    .format("%H:%M")
                    .to_string();
                let scope_color = match entry.scope {
                    polyphony_core::RunLogScope::Inbox => theme.info,
                    polyphony_core::RunLogScope::Agent => theme.success,
                    polyphony_core::RunLogScope::Reconciliation => theme.warning,
                    polyphony_core::RunLogScope::Pipeline => theme.muted,
                };
                let excerpt = if entry.message.len() > excerpt_width {
                    let end = entry
                        .message
                        .floor_char_boundary(excerpt_width.saturating_sub(1));
                    format!("{}…", &entry.message[..end])
                } else {
                    entry.message.clone()
                };
                child_row(
                    child_indent,
                    connector,
                    theme,
                    Line::from(vec![
                        Span::styled(format!("{time_label} "), Style::default().fg(theme.muted)),
                        Span::styled(excerpt, Style::default().fg(scope_color)),
                    ]),
                )
            },
            OrchestratorTreeRow::Outcome { run_snapshot_index } => {
                let m = &snapshot.runs[*run_snapshot_index];
                if let Some(deliverable) = &m.deliverable {
                    let (decision_icon, decision_color) = match deliverable.decision {
                        polyphony_core::DeliverableDecision::Waiting => ("◷", theme.warning),
                        polyphony_core::DeliverableDecision::Accepted => ("✓", theme.success),
                        polyphony_core::DeliverableDecision::Rejected => ("✕", theme.danger),
                    };
                    let kind_label = match deliverable.kind {
                        polyphony_core::DeliverableKind::GithubPullRequest => "PR",
                        polyphony_core::DeliverableKind::GitlabMergeRequest => "MR",
                        polyphony_core::DeliverableKind::LocalBranch => "branch",
                        polyphony_core::DeliverableKind::Patch => "patch",
                        polyphony_core::DeliverableKind::PullRequestReview => "review",
                    };
                    let url_label = deliverable
                        .url
                        .as_deref()
                        .or_else(|| deliverable.metadata.get("branch").and_then(|v| v.as_str()))
                        .unwrap_or(kind_label);
                    let mut diff_spans = Vec::new();
                    if let Some(added) = deliverable
                        .metadata
                        .get("lines_added")
                        .and_then(|v| v.as_u64())
                    {
                        diff_spans.push(Span::styled(
                            format!(" +{added}"),
                            Style::default().fg(theme.success),
                        ));
                    }
                    if let Some(removed) = deliverable
                        .metadata
                        .get("lines_removed")
                        .and_then(|v| v.as_u64())
                    {
                        diff_spans.push(Span::styled(
                            format!(" -{removed}"),
                            Style::default().fg(theme.danger),
                        ));
                    }
                    let mut spans = vec![
                        Span::styled(
                            format!("{decision_icon} "),
                            Style::default().fg(decision_color),
                        ),
                        Span::styled(format!("{kind_label} "), Style::default().fg(theme.muted)),
                        Span::styled(url_label.to_string(), Style::default().fg(theme.info)),
                    ];
                    spans.extend(diff_spans);
                    // Show verdict and confidence for PR reviews.
                    if deliverable.kind == polyphony_core::DeliverableKind::PullRequestReview {
                        if let Some(verdict) =
                            deliverable.metadata.get("verdict").and_then(|v| v.as_str())
                        {
                            let (icon, color) = match verdict {
                                "approve" => ("✓", theme.success),
                                "request_changes" => ("✕", theme.danger),
                                _ => ("●", theme.muted),
                            };
                            spans.push(Span::styled(
                                format!("  {icon} {verdict}"),
                                Style::default().fg(color),
                            ));
                        }
                        if let Some(confidence) = deliverable
                            .metadata
                            .get("confidence")
                            .and_then(|v| v.as_str())
                        {
                            spans.push(Span::styled(
                                format!("  confidence:{confidence}"),
                                Style::default().fg(theme.muted),
                            ));
                        }
                    }
                    child_row(child_indent, "└─", theme, Line::from(spans))
                } else {
                    // No deliverable — show terminal status with workspace path
                    let (icon, color, label) = match m.status {
                        polyphony_core::RunStatus::Delivered => {
                            ("✓", theme.success, "delivered (no changes)")
                        },
                        polyphony_core::RunStatus::Failed => ("✕", theme.danger, "failed"),
                        _ => ("●", theme.muted, "unknown"),
                    };
                    let mut spans = vec![
                        Span::styled(format!("{icon} "), Style::default().fg(color)),
                        Span::styled(label.to_string(), Style::default().fg(theme.muted)),
                    ];
                    if let Some(ws) = &m.workspace_path {
                        spans.push(Span::styled(
                            format!("  {}", ws.display()),
                            Style::default().fg(theme.muted),
                        ));
                    }
                    child_row(child_indent, "└─", theme, Line::from(spans))
                }
            },
        })
        .collect();

    let selected_style = Style::default().add_modifier(Modifier::BOLD);

    // Sticky header: when the first visible row is a child, pin its parent
    // run title at the top so the user always knows which run they're in.
    //
    // We compute the effective scroll offset ourselves because ratatui only
    // updates `TableState::offset()` during rendering (so reading it before
    // render returns the *previous* frame's value, causing a 1-frame lag).
    let viewport_height = area.height.saturating_sub(3) as usize; // borders + footer
    let selected = app.runs_state.selected().unwrap_or(0);
    let prev_offset = app.runs_state.offset();
    let effective_offset = if selected < prev_offset {
        selected
    } else if viewport_height > 0 && selected >= prev_offset + viewport_height {
        selected + 1 - viewport_height
    } else {
        prev_offset
    };

    let sticky_header: Option<Row> = if effective_offset > 0 {
        let first_visible = filtered_rows.get(effective_offset);
        let is_child = first_visible.is_some_and(|r| !matches!(r, OrchestratorTreeRow::Run { .. }));
        if is_child {
            // Walk backwards to find the parent Run row
            let parent_idx = (0..effective_offset)
                .rev()
                .find(|&i| matches!(filtered_rows[i], OrchestratorTreeRow::Run { .. }));
            parent_idx.and_then(|idx| {
                if let OrchestratorTreeRow::Run { snapshot_index } = filtered_rows[idx] {
                    let m = &snapshot.runs[*snapshot_index];
                    let (status_icon, status_icon_color) = run_status_emoji(&m.status, theme);
                    let task_info = format!("{}/{}", m.tasks_completed, m.task_count);
                    let time_label = if compact {
                        m.created_at
                            .with_timezone(&chrono::Local)
                            .format("%H:%M")
                            .to_string()
                    } else {
                        super::format_listing_time(m.created_at)
                    };
                    Some(
                        Row::new(vec![Cell::from(Line::from(vec![
                            Span::styled(
                                format!("  {time_label} "),
                                Style::default().fg(theme.muted),
                            ),
                            Span::styled(
                                format!("{status_icon} "),
                                Style::default().fg(status_icon_color),
                            ),
                            Span::styled(m.title.clone(), Style::default().fg(theme.foreground)),
                            Span::styled(
                                format!("  {task_info}"),
                                Style::default().fg(theme.muted),
                            ),
                        ]))])
                        .style(Style::default().fg(theme.muted)),
                    )
                } else {
                    None
                }
            })
        } else {
            None
        }
    } else {
        None
    };

    let sort_label = app.run_sort.label();
    let footer_info = if count == 0 {
        "no runs".into()
    } else {
        format!(
            "{} of {count}",
            app.runs_state.selected().unwrap_or_default() + 1
        )
    };

    let title = if app.runs_search_active {
        Line::from(vec![
            Span::styled(" Runs ", Style::default().fg(theme.highlight)),
            Span::styled(
                format!("/{}\u{258F}", app.runs_search_query),
                Style::default().fg(theme.foreground),
            ),
        ])
    } else if !app.runs_search_query.is_empty() {
        Line::from(vec![
            Span::styled(" Runs ", Style::default().fg(theme.highlight)),
            Span::styled(
                format!("[{}] ", app.runs_search_query),
                Style::default().fg(theme.info),
            ),
        ])
    } else {
        Line::from(Span::styled(
            " Runs ",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ))
    };

    let mut table = Table::new(rows, [Constraint::Fill(1)])
        .row_highlight_style(selected_style)
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::WhenSelected);
    if let Some(header_row) = sticky_header {
        table = table.header(header_row);
    }
    let table = table.block(
        Block::default()
            .title(title)
            .title_bottom(
                Line::from(vec![
                    Span::styled(footer_info, Style::default().fg(theme.muted)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("f:", Style::default().fg(theme.muted)),
                    Span::styled("feedback", Style::default().fg(theme.highlight)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("sorted by ", Style::default().fg(theme.muted)),
                    Span::styled(sort_label, Style::default().fg(theme.highlight)),
                ])
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if app.list_border_focused {
                theme.highlight
            } else {
                theme.border
            }))
            .style(Style::default().bg(theme.panel)),
    );

    frame.render_stateful_widget(table, area, &mut app.runs_state);

    if count > 0 {
        let content_height = area.height.saturating_sub(3) as usize;
        if count > content_height {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.runs_state.selected().unwrap_or(0))
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

pub(crate) fn run_kind_label(kind: polyphony_core::RunKind) -> &'static str {
    match kind {
        polyphony_core::RunKind::IssueDelivery => "issue",
        polyphony_core::RunKind::PullRequestReview => "pr_review",
        polyphony_core::RunKind::PullRequestCommentReview => "pr_comment",
    }
}

pub(crate) fn run_target_label(run: &polyphony_core::RunRow) -> String {
    if let Some(target) = &run.review_target {
        format!("{}#{}", target.repository, target.number)
    } else {
        run.issue_identifier.clone().unwrap_or_default()
    }
}

fn run_status_emoji(
    status: &polyphony_core::RunStatus,
    theme: crate::theme::Theme,
) -> (&'static str, ratatui::style::Color) {
    use polyphony_core::RunStatus;
    match status {
        RunStatus::Pending => ("…", theme.info),
        RunStatus::Planning => ("◌", theme.info),
        RunStatus::InProgress => ("◐", theme.success),
        RunStatus::Review => ("◑", theme.highlight),
        RunStatus::Delivered => ("✓", theme.success),
        RunStatus::Failed => ("✕", theme.danger),
        RunStatus::Cancelled => ("⊘", theme.muted),
    }
}

fn run_output_emoji(
    run: &polyphony_core::RunRow,
    theme: crate::theme::Theme,
) -> (&'static str, ratatui::style::Color) {
    if let Some(deliverable) = &run.deliverable {
        match deliverable.decision {
            polyphony_core::DeliverableDecision::Waiting => ("◷", theme.warning),
            polyphony_core::DeliverableDecision::Accepted => ("✓", theme.success),
            polyphony_core::DeliverableDecision::Rejected => ("✕", theme.danger),
        }
    } else if matches!(run.kind, polyphony_core::RunKind::PullRequestReview) {
        ("⟐", theme.highlight)
    } else {
        ("—", theme.muted)
    }
}

fn render_event_line(
    event: &polyphony_core::RuntimeEvent,
    theme: crate::theme::Theme,
) -> Line<'static> {
    let ts = event
        .at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M");
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
            format!("{:<10} ", event.scope),
            Style::default().fg(scope_color),
        ),
        Span::styled(event.message.clone(), Style::default().fg(theme.foreground)),
    ])
}

pub(crate) fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> usize {
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

pub(crate) fn run_status_emoji_pub(
    status: &polyphony_core::RunStatus,
    theme: crate::theme::Theme,
) -> (&'static str, ratatui::style::Color) {
    run_status_emoji(status, theme)
}

pub(crate) fn run_status_color_pub(
    status: &polyphony_core::RunStatus,
    theme: crate::theme::Theme,
) -> ratatui::style::Color {
    run_status_color(status, theme)
}

pub(crate) fn run_status_label(status: &polyphony_core::RunStatus) -> &'static str {
    use polyphony_core::RunStatus;
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Planning => "planning",
        RunStatus::InProgress => "in progress",
        RunStatus::Review => "review",
        RunStatus::Delivered => "delivered",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn run_status_color(
    status: &polyphony_core::RunStatus,
    theme: crate::theme::Theme,
) -> ratatui::style::Color {
    use polyphony_core::RunStatus;
    match status {
        RunStatus::Pending | RunStatus::Planning => theme.info,
        RunStatus::InProgress => theme.success,
        RunStatus::Review => theme.highlight,
        RunStatus::Delivered => theme.success,
        RunStatus::Failed => theme.danger,
        RunStatus::Cancelled => theme.muted,
    }
}

#[allow(dead_code)]
fn _draw_events_panel(
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

// ---------------------------------------------------------------------------
// Compact recent events (for inline detail views — 3 most recent)
// ---------------------------------------------------------------------------

const MAX_INLINE_EVENTS: usize = 3;

/// Build a compact "Recent Events" section: at most 3 most-recent matching
/// events (newest at the bottom), each truncated to a single line, plus a
/// hint to open the full event log.
pub(crate) fn compact_recent_event_lines(
    snapshot: &RuntimeSnapshot,
    filter: &str,
    theme: crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Recent Events",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));

    // Collect matching events newest-first, take 3, then reverse so newest is at bottom.
    let matching: Vec<_> = snapshot
        .recent_events
        .iter()
        .filter(|event| event.message.contains(filter))
        .take(MAX_INLINE_EVENTS)
        .collect();

    if matching.is_empty() {
        lines.push(Line::from(Span::styled(
            "No recent events.",
            Style::default().fg(theme.muted),
        )));
    } else {
        for event in matching.into_iter().rev() {
            lines.push(render_event_line(event, theme));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Full-screen filtered events detail view
// ---------------------------------------------------------------------------

pub(crate) fn draw_filtered_events(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    filter: &str,
    snapshot: &RuntimeSnapshot,
    app: &mut crate::app::AppState,
) {
    let theme = app.theme;
    let content_height = area.height.saturating_sub(2) as usize;

    // Oldest first, newest at bottom (log-style).
    let lines: Vec<Line> = snapshot
        .recent_events
        .iter()
        .rev()
        .filter(|event| event.message.contains(filter))
        .map(|event| render_event_line(event, theme))
        .collect();

    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(content_height) as u16;

    // Clamp / auto-scroll to bottom.
    let scroll = app.current_detail_mut().map(|d| d.scroll_mut());
    if let Some(s) = scroll
        && (*s >= max_scroll || *s == u16::MAX)
    {
        *s = max_scroll;
    }
    let scroll_pos = app.current_detail().map(|d| d.scroll()).unwrap_or(0);

    let count_label = format!(" {total_lines} events ");

    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_pos, 0)).block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled(
                        " Events ",
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({filter}) "), Style::default().fg(theme.muted)),
                ]))
                .title(
                    Line::from(Span::styled(count_label, Style::default().fg(theme.muted)))
                        .right_aligned(),
                )
                .title_bottom(
                    Line::from(vec![
                        Span::styled("j/k", Style::default().fg(theme.highlight)),
                        Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                        Span::styled("g/G", Style::default().fg(theme.highlight)),
                        Span::styled(":top/bottom  ", Style::default().fg(theme.muted)),
                        Span::styled("Esc", Style::default().fg(theme.highlight)),
                        Span::styled(":back ", Style::default().fg(theme.muted)),
                    ])
                    .right_aligned(),
                )
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.panel_alt)),
        ),
        area,
    );

    // Scrollbar
    if total_lines > content_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .position(scroll_pos as usize)
            .viewport_content_length(content_height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("║"))
                .thumb_symbol("█"),
            area,
            &mut scrollbar_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use polyphony_core::{ReviewProviderKind, ReviewTarget, RunKind, RunRow, RunStatus};

    use crate::{
        render::orchestrator::{render_event_line, run_output_emoji, run_target_label},
        theme::default_theme,
    };

    #[test]
    fn run_target_label_prefers_review_target() {
        let run = RunRow {
            repo_id: String::new(),
            id: "run-1".to_string(),
            kind: RunKind::PullRequestReview,
            issue_identifier: Some("xm7".to_string()),
            title: "Review PR".to_string(),
            status: RunStatus::InProgress,
            task_count: 0,
            tasks_completed: 0,
            deliverable: None,
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
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
        };

        assert_eq!(run_target_label(&run), "penso/polyphony#123");
    }

    #[test]
    fn pull_request_review_runs_use_compact_review_icon() {
        let run = RunRow {
            repo_id: String::new(),
            id: "run-1".to_string(),
            kind: RunKind::PullRequestReview,
            issue_identifier: Some("penso/polyphony#123".to_string()),
            title: "Review PR".to_string(),
            status: RunStatus::InProgress,
            task_count: 0,
            tasks_completed: 0,
            deliverable: None,
            has_deliverable: false,
            review_target: None,
            workspace_key: None,
            workspace_path: None,
            created_at: chrono::Utc::now(),
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
        };

        let (icon, _) = run_output_emoji(&run, default_theme());
        assert_eq!(icon, "⟐");
    }

    #[test]
    fn render_event_line_inserts_separator_between_scope_and_message() {
        let event = polyphony_core::RuntimeEvent {
            at: chrono::Utc::now(),
            scope: polyphony_core::EventScope::Dispatch,
            message: "manual dispatch: w1b -> default".to_string(),
        };

        let line = render_event_line(&event, crate::theme::default_theme());
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.contains("dispatch manual dispatch: w1b -> default"));
    }
}
