use chrono::Utc;
use polyphony_core::{AgentHistoryRow, RunningRow, RuntimeSnapshot};
use ratatui::{
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table,
    },
};

use crate::app::AppState;

pub fn draw_agents_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let header = Row::new(vec![
        Cell::from(Span::styled("Time", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Agent", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Model", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Status", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Turns", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Span", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Tokens", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Last Event", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let now = Utc::now();
    let mut rows = Vec::with_capacity(app.sorted_agent_indices.len());
    for &(is_running, orig_idx) in &app.sorted_agent_indices {
        if is_running {
            if let Some(running) = snapshot.running.get(orig_idx) {
                rows.push(agent_table_row(
                    super::format_listing_time(running.started_at),
                    running.agent_name.clone(),
                    running.model.clone(),
                    "running".into(),
                    format!("{}/{}", running.turn_count, running.max_turns),
                    format_duration(now.signed_duration_since(running.started_at)),
                    format_tokens(running.tokens.total_tokens),
                    running
                        .last_event
                        .as_deref()
                        .map(|event| truncate(event, 30))
                        .unwrap_or_default(),
                    theme,
                ));
            }
        } else if let Some(history) = snapshot.agent_history.get(orig_idx) {
            rows.push(agent_table_row(
                super::format_listing_time(history.started_at),
                history.agent_name.clone(),
                history.model.clone(),
                history.status.to_string(),
                format!("{}/{}", history.turn_count, history.max_turns),
                format_history_span(history, now),
                format_tokens(history.tokens.total_tokens),
                history
                    .last_event
                    .as_deref()
                    .map(|event| truncate(event, 30))
                    .unwrap_or_default(),
                theme,
            ));
        }
    }

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = app.sorted_agent_indices.len();
    let footer_info = if count == 0 {
        "no sessions".into()
    } else {
        format!(
            "{} of {count}",
            app.agents_state.selected().unwrap_or_default() + 1
        )
    };

    let has_retrying = !snapshot.retrying.is_empty();
    let retrying_suffix = if has_retrying {
        format!(" | {} retrying", snapshot.retrying.len())
    } else {
        String::new()
    };

    let table = Table::new(rows, [
        Constraint::Length(18),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Fill(1),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Agent Sessions ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(Span::styled(
                    format!("─{footer_info}{retrying_suffix}─"),
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

    frame.render_stateful_widget(table, area, &mut app.agents_state);

    if count > 0 {
        let content_height = area.height.saturating_sub(3) as usize;
        if count > content_height {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.agents_state.selected().unwrap_or(0))
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

fn agent_table_row(
    time: String,
    agent_name: String,
    model: Option<String>,
    status: String,
    turns: String,
    span: String,
    tokens: String,
    last_event: String,
    theme: crate::theme::Theme,
) -> Row<'static> {
    Row::new(vec![
        Cell::from(Span::styled(time, Style::default().fg(theme.muted))),
        Cell::from(Span::styled(
            agent_name,
            Style::default().fg(theme.foreground),
        )),
        Cell::from(Span::styled(
            model.unwrap_or_else(|| "-".into()),
            Style::default().fg(theme.muted),
        )),
        Cell::from(Span::styled(status, Style::default().fg(theme.foreground))),
        Cell::from(Span::styled(turns, Style::default().fg(theme.foreground))),
        Cell::from(Span::styled(span, Style::default().fg(theme.foreground))),
        Cell::from(Span::styled(tokens, Style::default().fg(theme.foreground))),
        Cell::from(Span::styled(last_event, Style::default().fg(theme.muted))),
    ])
}

pub(crate) fn format_history_span(agent: &AgentHistoryRow, now: chrono::DateTime<Utc>) -> String {
    let finished_at = agent.finished_at.unwrap_or(now);
    format_duration(finished_at.signed_duration_since(agent.started_at))
}

pub(crate) fn build_agent_detail_lines(
    snapshot: &RuntimeSnapshot,
    agent: crate::app::SelectedAgentRow<'_>,
    artifact_saved_context: Option<&polyphony_core::AgentContextSnapshot>,
    theme: crate::theme::Theme,
    frame_count: u64,
) -> Vec<Line<'static>> {
    match agent {
        crate::app::SelectedAgentRow::Running(agent) => build_running_agent_detail_lines(
            snapshot,
            agent,
            artifact_saved_context,
            theme,
            frame_count,
        ),
        crate::app::SelectedAgentRow::History(agent) => {
            build_history_agent_detail_lines(snapshot, agent, artifact_saved_context, theme)
        },
    }
}

const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn build_running_agent_detail_lines(
    snapshot: &RuntimeSnapshot,
    agent: &RunningRow,
    _artifact_saved_context: Option<&polyphony_core::AgentContextSnapshot>,
    theme: crate::theme::Theme,
    frame_count: u64,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let now = Utc::now();
    let elapsed = format_duration(now.signed_duration_since(agent.started_at));
    let model = agent.model.as_deref().unwrap_or("unknown");
    let turns = format!("turn {}/{}", agent.turn_count, agent.max_turns);

    let spinner = BRAILLE_SPINNER[(frame_count / 4) as usize % BRAILLE_SPINNER.len()];
    let header = format!(
        "{} - {} ({model}) - {turns} - {elapsed}",
        agent.issue_identifier, agent.agent_name,
    );
    if let Some(session_id) = &agent.session_id {
        lines.push(Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(theme.info)),
            Span::styled(session_id.clone(), Style::default().fg(theme.highlight)),
            Span::styled(" | ", Style::default().fg(theme.border)),
            Span::styled(header, Style::default().fg(theme.foreground)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("tmux attach -t ", Style::default().fg(theme.muted)),
            Span::styled(session_id.clone(), Style::default().fg(theme.info)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(format!("{spinner} "), Style::default().fg(theme.info)),
            Span::styled(header, Style::default().fg(theme.foreground)),
        ]));
    }

    // Status line: what the agent is currently doing
    let status_line = build_agent_status_line(agent, now, spinner, theme);
    lines.push(status_line);

    // Live terminal output — primary content, shown first
    append_live_output(&mut lines, &agent.workspace_path, &agent.agent_name, theme);

    lines.push(Line::default());
    append_agent_availability_lines(&mut lines, snapshot, agent, theme);
    lines.push(Line::default());
    append_recent_events(&mut lines, snapshot, &agent.issue_identifier, theme);

    lines
}

/// Build a status line showing what the agent is currently doing.
fn build_agent_status_line(
    agent: &RunningRow,
    now: chrono::DateTime<Utc>,
    spinner: char,
    theme: crate::theme::Theme,
) -> Line<'static> {
    let last_event = agent.last_event.as_deref().unwrap_or("starting");
    let since_last = agent
        .last_event_at
        .map(|at| now.signed_duration_since(at))
        .unwrap_or_else(|| now.signed_duration_since(agent.started_at));
    let since_str = format_duration(since_last);

    // Infer what's happening from the last event
    let (status_text, status_color) = match last_event {
        "session started" | "pty session started" | "tmux session started" => {
            ("starting up", theme.info)
        },
        "turn started" => ("thinking", theme.warning),
        "usage updated" => ("thinking", theme.warning),
        "tool call" | "tool call started" => ("running tool", theme.info),
        "tool done" | "tool call completed" => ("thinking", theme.warning),
        "tool failed" | "tool call failed" => ("thinking (tool failed)", theme.danger),
        _ => (last_event, theme.muted),
    };

    Line::from(vec![
        Span::styled(format!("{spinner} "), Style::default().fg(theme.info)),
        Span::styled(status_text.to_owned(), Style::default().fg(status_color)),
        Span::styled(format!("  ({since_str})"), Style::default().fg(theme.muted)),
    ])
}

/// Read the raw PTY/tmux log and append terminal output as the primary live view.
fn append_live_output(
    lines: &mut Vec<Line<'static>>,
    workspace_path: &std::path::Path,
    agent_name: &str,
    theme: crate::theme::Theme,
) {
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            "Live Output",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  c:fullscreen", Style::default().fg(theme.muted)),
    ]));

    let run_dir = workspace_path.join(".polyphony");
    let log_path = ["pty.log", "tmux.log", "appserver.log"]
        .iter()
        .map(|suffix| run_dir.join(format!("{agent_name}-{suffix}")))
        .find(|p| p.exists());

    let Some(log_path) = log_path else {
        lines.push(Line::from(Span::styled(
            "Waiting for agent output…",
            Style::default().fg(theme.muted),
        )));
        return;
    };
    let Ok(raw) = std::fs::read(&log_path) else {
        lines.push(Line::from(Span::styled(
            "Waiting for agent output…",
            Style::default().fg(theme.muted),
        )));
        return;
    };
    if raw.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for agent output…",
            Style::default().fg(theme.muted),
        )));
        return;
    }

    // For PTY/tmux logs, parse through vt100 to strip ANSI escapes.
    // For appserver logs (plain text), use content directly.
    let is_plain_text = log_path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.contains("appserver"));
    let content = if is_plain_text {
        String::from_utf8_lossy(&raw).into_owned()
    } else {
        let mut parser = vt100::Parser::new(500, 120, 0);
        parser.process(&raw);
        parser.screen().contents()
    };
    let output_lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    // Show all available lines — the panel scrolls
    if output_lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for agent output…",
            Style::default().fg(theme.muted),
        )));
        return;
    }

    for line in output_lines {
        lines.push(Line::from(Span::styled(
            line.to_owned(),
            Style::default().fg(theme.foreground),
        )));
    }
}

fn build_history_agent_detail_lines(
    snapshot: &RuntimeSnapshot,
    agent: &AgentHistoryRow,
    artifact_saved_context: Option<&polyphony_core::AgentContextSnapshot>,
    theme: crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let model = agent.model.as_deref().unwrap_or("unknown");
    let turns = format!("turn {}/{}", agent.turn_count, agent.max_turns);
    let finished_at = agent.finished_at.unwrap_or(agent.started_at);
    let span = format_history_span(agent, Utc::now());
    let header = format!(
        "{} - {} ({model}) - {} - {turns} - {span}",
        agent.issue_identifier, agent.agent_name, agent.status
    );

    if let Some(session_id) = &agent.session_id {
        lines.push(Line::from(vec![
            Span::styled(session_id.clone(), Style::default().fg(theme.highlight)),
            Span::styled(" | ", Style::default().fg(theme.border)),
            Span::styled(header, Style::default().fg(theme.foreground)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            header,
            Style::default().fg(theme.foreground),
        )));
    }

    lines.push(Line::from(vec![
        Span::styled("Started ", Style::default().fg(theme.muted)),
        Span::styled(
            agent.started_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            Style::default().fg(theme.info),
        ),
        Span::styled(" | Finished ", Style::default().fg(theme.muted)),
        Span::styled(
            finished_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            Style::default().fg(theme.info),
        ),
    ]));

    if let Some(workspace_path) = &agent.workspace_path {
        lines.push(Line::from(vec![
            Span::styled("Workspace ", Style::default().fg(theme.muted)),
            Span::styled(
                workspace_path.display().to_string(),
                Style::default().fg(theme.foreground),
            ),
        ]));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Last Message",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));

    if let Some(last_message) = agent.last_message.as_deref() {
        extend_plain_lines(&mut lines, last_message, theme.foreground);
    } else {
        lines.push(Line::from(Span::styled(
            "No agent output was captured.",
            Style::default().fg(theme.muted),
        )));
    }

    if let Some(error) = agent.error.as_deref() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Error",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )));
        extend_plain_lines(&mut lines, error, theme.danger);
    }

    append_transcript(
        &mut lines,
        artifact_saved_context.or(agent.saved_context.as_ref()),
        theme,
    );

    lines.push(Line::default());
    append_recent_events(&mut lines, snapshot, &agent.issue_identifier, theme);

    lines
}

fn append_recent_events(
    lines: &mut Vec<Line<'static>>,
    snapshot: &RuntimeSnapshot,
    issue_identifier: &str,
    theme: crate::theme::Theme,
) {
    lines.extend(super::orchestrator::compact_recent_event_lines(
        snapshot,
        issue_identifier,
        theme,
    ));
}

fn append_transcript(
    lines: &mut Vec<Line<'static>>,
    saved_context: Option<&polyphony_core::AgentContextSnapshot>,
    theme: crate::theme::Theme,
) {
    use polyphony_core::AgentEventKind;

    let Some(ctx) = saved_context else {
        return;
    };
    if ctx.transcript.is_empty() {
        return;
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Transcript",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));

    for (i, entry) in ctx.transcript.iter().enumerate() {
        let (kind_label, kind_color) = match entry.kind {
            AgentEventKind::SessionStarted => ("session started", theme.info),
            AgentEventKind::TurnStarted => ("turn started", theme.info),
            AgentEventKind::TurnCompleted => ("turn completed", theme.success),
            AgentEventKind::TurnFailed => ("turn failed", theme.danger),
            AgentEventKind::TurnCancelled => ("cancelled", theme.warning),
            AgentEventKind::ToolCallStarted => ("tool call", theme.muted),
            AgentEventKind::ToolCallCompleted => ("tool done", theme.muted),
            AgentEventKind::ToolCallFailed => ("tool failed", theme.danger),
            AgentEventKind::Notification => ("notice", theme.foreground),
            AgentEventKind::UsageUpdated => ("usage", theme.muted),
            AgentEventKind::RateLimitsUpdated => ("rate limit", theme.warning),
            AgentEventKind::StartupFailed => ("startup failed", theme.danger),
            AgentEventKind::OtherMessage => ("message", theme.foreground),
            AgentEventKind::Outcome => ("outcome", theme.success),
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{} ",
                    entry.at.with_timezone(&chrono::Local).format("%H:%M:%S")
                ),
                Style::default().fg(theme.muted),
            ),
            Span::styled(
                format!("{kind_label:>16} "),
                Style::default().fg(kind_color),
            ),
        ]));

        // Message on indented continuation lines
        if !entry.message.is_empty() {
            for msg_line in entry.message.lines() {
                lines.push(Line::from(Span::styled(
                    format!("                    {msg_line}"),
                    Style::default().fg(theme.foreground),
                )));
            }
        }

        // Separator between entries (except last)
        if i + 1 < ctx.transcript.len() {
            let next_kind = ctx.transcript[i + 1].kind;
            // Add blank line before turn boundaries for visual grouping
            if matches!(
                next_kind,
                AgentEventKind::TurnStarted | AgentEventKind::SessionStarted
            ) {
                lines.push(Line::default());
            }
        }
    }
}

fn append_agent_availability_lines(
    lines: &mut Vec<Line<'static>>,
    snapshot: &RuntimeSnapshot,
    agent: &RunningRow,
    theme: crate::theme::Theme,
) {
    lines.push(Line::from(Span::styled(
        "Availability",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));

    let component = format!("agent:{}", agent.agent_name);
    if let Some(throttle) = snapshot
        .throttles
        .iter()
        .find(|throttle| throttle.component == component)
    {
        let remaining = throttle.until.signed_duration_since(Utc::now());
        lines.push(Line::from(vec![
            Span::styled("Throttled until ", Style::default().fg(theme.muted)),
            Span::styled(
                format!(
                    "{} ({})",
                    throttle.until.format("%H:%M UTC"),
                    format_duration(remaining)
                ),
                Style::default().fg(theme.danger),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            throttle.reason.clone(),
            Style::default().fg(theme.warning),
        )));
        return;
    }

    if let Some(budget) = snapshot
        .budgets
        .iter()
        .find(|budget| budget.component == component)
    {
        let remaining = budget.credits_remaining.unwrap_or(0.0);
        let total = budget.credits_total.unwrap_or(0.0);
        let used_up = total > 0.0 && remaining <= 0.0;
        let color = if used_up {
            theme.danger
        } else {
            theme.foreground
        };
        let summary = if total > 0.0 {
            format!("{remaining:.0}/{total:.0} credits remaining")
        } else {
            format!("{remaining:.0} credits remaining")
        };
        lines.push(Line::from(Span::styled(
            summary,
            Style::default().fg(color),
        )));
        if let Some(reset_at) = budget.reset_at {
            let remaining = reset_at.signed_duration_since(Utc::now());
            lines.push(Line::from(vec![
                Span::styled("Resets at ", Style::default().fg(theme.muted)),
                Span::styled(
                    format!(
                        "{} ({})",
                        reset_at.format("%H:%M UTC"),
                        format_duration(remaining)
                    ),
                    Style::default().fg(theme.info),
                ),
            ]));
        }
        return;
    }

    lines.push(Line::from(Span::styled(
        "No budget or throttle data for this agent yet.",
        Style::default().fg(theme.muted),
    )));
}

fn extend_plain_lines(lines: &mut Vec<Line<'static>>, content: &str, color: ratatui::style::Color) {
    if content.is_empty() {
        lines.push(Line::default());
        return;
    }

    for raw_line in content.lines() {
        lines.push(Line::from(Span::styled(
            raw_line.to_string(),
            Style::default().fg(color),
        )));
    }
}

pub(crate) fn format_duration(duration: chrono::Duration) -> String {
    let total_secs = duration.num_seconds().max(0);
    if total_secs < 60 {
        format!("{total_secs}s")
    } else if total_secs < 3600 {
        format!("{}m{}s", total_secs / 60, total_secs % 60)
    } else {
        format!("{}h{}m", total_secs / 3600, (total_secs % 3600) / 60)
    }
}

pub(crate) fn format_tokens_pub(tokens: u64) -> String {
    format_tokens(tokens)
}

fn format_tokens(tokens: u64) -> String {
    if tokens == 0 {
        "-".into()
    } else if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 1_000_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let keep = max_len.saturating_sub(3);
        let truncated: String = s.chars().take(keep).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use crate::render::agents::truncate;

    #[test]
    fn truncate_handles_unicode_without_panicking() {
        let message = "rate_limited: You've hit your limit · resets 2am (Europe/Lisbon)";

        assert_eq!(
            truncate(message, 40),
            "rate_limited: You've hit your limit ·..."
        );
    }
}
