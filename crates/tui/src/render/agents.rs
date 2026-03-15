use {
    chrono::Utc,
    polyphony_core::{AgentHistoryRow, EventScope, RunningRow, RuntimeSnapshot},
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

use crate::app::{AppState, SelectedAgentRow};

pub fn draw_agents_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let has_retrying = !snapshot.retrying.is_empty();

    let mut constraints = vec![Constraint::Length(10), Constraint::Min(6)];
    if has_retrying {
        constraints.push(Constraint::Length(6));
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    draw_agent_table(frame, sections[0], snapshot, app);
    draw_agent_detail(frame, sections[1], snapshot, app);

    if has_retrying {
        draw_retrying_table(frame, sections[2], snapshot, app);
    }
}

fn draw_agent_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let header = Row::new(vec![
        Cell::from(Span::styled("Issue", Style::default().fg(theme.muted))),
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
    let mut rows = Vec::with_capacity(snapshot.running.len() + snapshot.agent_history.len());
    for running in &snapshot.running {
        rows.push(agent_table_row(
            running.issue_identifier.clone(),
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
    for history in &snapshot.agent_history {
        rows.push(agent_table_row(
            history.issue_identifier.clone(),
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

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = snapshot.running.len() + snapshot.agent_history.len();
    let footer_info = if count == 0 {
        "no agents".into()
    } else {
        format!(
            "{} of {count}",
            app.agents_state.selected().unwrap_or_default() + 1
        )
    };

    let table = Table::new(rows, [
        Constraint::Length(14),
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
    .highlight_symbol("▸ ")
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Agents ",
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

    frame.render_stateful_widget(table, area, &mut app.agents_state);
}

fn agent_table_row(
    issue_identifier: String,
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
        Cell::from(Span::styled(
            issue_identifier,
            Style::default().fg(theme.info),
        )),
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

fn format_history_span(agent: &AgentHistoryRow, now: chrono::DateTime<Utc>) -> String {
    let finished_at = agent.finished_at.unwrap_or(now);
    format_duration(finished_at.signed_duration_since(agent.started_at))
}

fn draw_agent_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    app.agents_detail_area = area;

    let lines = app
        .selected_agent(snapshot)
        .map(|agent| build_agent_detail_lines(snapshot, agent, theme))
        .unwrap_or_else(|| {
            vec![
                Line::from(Span::styled(
                    "No agent run selected.",
                    Style::default().fg(theme.muted),
                )),
                Line::from(Span::styled(
                    "Use j/k to choose an agent, Shift+J/Shift+K or the mouse wheel to scroll this pane.",
                    Style::default().fg(theme.muted),
                )),
            ]
        });

    let content_height = area.height.saturating_sub(2) as usize;
    let content_width = area.width.saturating_sub(2).max(1);
    let total_lines = wrapped_line_count(&lines, content_width);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.agents_detail_scroll, 0))
        .block(
            Block::default()
                .title(Line::from(Span::styled(
                    " Agent Detail ",
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
        );

    let max_scroll = total_lines.saturating_sub(content_height) as u16;
    if app.agents_detail_scroll > max_scroll {
        app.agents_detail_scroll = max_scroll;
    }

    frame.render_widget(paragraph, area);

    if total_lines > content_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .position(app.agents_detail_scroll as usize)
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

fn draw_retrying_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;

    let header = Row::new(vec![
        Cell::from(Span::styled("Issue", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Attempt", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Due In", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Error", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let now = Utc::now();
    let rows: Vec<Row> = snapshot
        .retrying
        .iter()
        .map(|r| {
            let due_in = {
                let remaining = r.due_at.signed_duration_since(now);
                if remaining.num_seconds() <= 0 {
                    "due now".into()
                } else {
                    format_duration(remaining)
                }
            };
            let error = r
                .error
                .as_deref()
                .map(|e| truncate(e, 40))
                .unwrap_or_default();

            Row::new(vec![
                Cell::from(Span::styled(
                    r.issue_identifier.clone(),
                    Style::default().fg(theme.info),
                )),
                Cell::from(Span::styled(
                    r.attempt.to_string(),
                    Style::default().fg(theme.warning),
                )),
                Cell::from(Span::styled(due_in, Style::default().fg(theme.foreground))),
                Cell::from(Span::styled(error, Style::default().fg(theme.danger))),
            ])
        })
        .collect();

    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Fill(1),
    ])
    .header(header)
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Retrying ",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border)),
    );

    frame.render_widget(table, area);
}

fn build_agent_detail_lines(
    snapshot: &RuntimeSnapshot,
    agent: SelectedAgentRow<'_>,
    theme: crate::theme::Theme,
) -> Vec<Line<'static>> {
    match agent {
        SelectedAgentRow::Running(agent) => {
            build_running_agent_detail_lines(snapshot, agent, theme)
        },
        SelectedAgentRow::History(agent) => {
            build_history_agent_detail_lines(snapshot, agent, theme)
        },
    }
}

fn build_running_agent_detail_lines(
    snapshot: &RuntimeSnapshot,
    agent: &RunningRow,
    theme: crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let now = Utc::now();
    let elapsed = format_duration(now.signed_duration_since(agent.started_at));
    let model = agent.model.as_deref().unwrap_or("unknown");
    let turns = format!("turn {}/{}", agent.turn_count, agent.max_turns);

    let header = format!(
        "{} - {} ({model}) - {turns} - {elapsed}",
        agent.issue_identifier, agent.agent_name,
    );
    if let Some(session_id) = &agent.session_id {
        lines.push(Line::from(vec![
            Span::styled(session_id.clone(), Style::default().fg(theme.highlight)),
            Span::styled(" | ", Style::default().fg(theme.border)),
            Span::styled(header, Style::default().fg(theme.foreground)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("tmux attach -t ", Style::default().fg(theme.muted)),
            Span::styled(session_id.clone(), Style::default().fg(theme.info)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            header,
            Style::default().fg(theme.foreground),
        )));
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
            "No agent output yet.",
            Style::default().fg(theme.muted),
        )));
    }

    lines.push(Line::default());
    append_agent_availability_lines(&mut lines, snapshot, agent, theme);
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Recent Events",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));

    let mut event_count = 0usize;
    for event in snapshot.recent_events.iter().rev() {
        if !message_mentions_issue(&event.message, &agent.issue_identifier) {
            continue;
        }
        event_count += 1;
        lines.push(render_event_line(event, theme));
    }

    if event_count == 0 {
        lines.push(Line::from(Span::styled(
            "No recent events for this issue.",
            Style::default().fg(theme.muted),
        )));
    }

    lines
}

fn build_history_agent_detail_lines(
    snapshot: &RuntimeSnapshot,
    agent: &AgentHistoryRow,
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

    if let Some(saved_context) = &agent.saved_context
        && !saved_context.transcript.is_empty()
    {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Transcript",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )));
        for entry in &saved_context.transcript {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", entry.at.format("%H:%M:%S")),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
                    format!("{:?} ", entry.kind),
                    Style::default().fg(theme.info),
                ),
                Span::styled(entry.message.clone(), Style::default().fg(theme.foreground)),
            ]));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Recent Events",
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )));

    let mut event_count = 0usize;
    for event in snapshot.recent_events.iter().rev() {
        if !message_mentions_issue(&event.message, &agent.issue_identifier) {
            continue;
        }
        event_count += 1;
        lines.push(render_event_line(event, theme));
    }

    if event_count == 0 {
        lines.push(Line::from(Span::styled(
            "No recent events for this issue.",
            Style::default().fg(theme.muted),
        )));
    }

    lines
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

fn render_event_line(
    event: &polyphony_core::RuntimeEvent,
    theme: crate::theme::Theme,
) -> Line<'static> {
    let ts = event.at.format("%H:%M:%S");
    let scope_color = match event.scope {
        EventScope::Dispatch => theme.info,
        EventScope::Handoff => theme.highlight,
        EventScope::Worker | EventScope::Agent => theme.success,
        EventScope::Retry => theme.warning,
        EventScope::Throttle => theme.danger,
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

fn message_mentions_issue(message: &str, issue_identifier: &str) -> bool {
    if issue_identifier.is_empty() {
        return false;
    }

    let mut start = 0usize;
    while let Some(offset) = message[start..].find(issue_identifier) {
        let matched = start + offset;
        let end = matched + issue_identifier.len();
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
    let width = usize::from(width.max(1));
    lines
        .iter()
        .map(|line| {
            let line_width: usize = line
                .spans
                .iter()
                .map(|span| span.content.chars().count())
                .sum();
            line_width.max(1).div_ceil(width)
        })
        .sum()
}

fn format_duration(duration: chrono::Duration) -> String {
    let total_secs = duration.num_seconds().max(0);
    if total_secs < 60 {
        format!("{total_secs}s")
    } else if total_secs < 3600 {
        format!("{}m{}s", total_secs / 60, total_secs % 60)
    } else {
        format!("{}h{}m", total_secs / 3600, (total_secs % 3600) / 60)
    }
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
