use chrono::Utc;
use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
};

use crate::app::AppState;
use crate::theme::Theme;

pub fn draw_logs_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(6)])
        .split(area);

    draw_logs_panel(frame, chunks[0], app);
    draw_network_panel(frame, chunks[1], snapshot, app);
}

fn draw_logs_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut AppState) {
    let theme = app.theme;
    let inner_width = area.width.saturating_sub(3) as usize; // borders + scrollbar

    let raw_lines = app.log_buffer.recent_lines(500);

    let display_lines: Vec<Line<'_>> = raw_lines
        .iter()
        .map(|line| parse_log_line(line, theme))
        .collect();

    let visible_height = area.height.saturating_sub(2) as usize;

    // Estimate total wrapped lines for scroll offset
    let total_wrapped: usize = display_lines
        .iter()
        .map(|line| {
            let line_len: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if inner_width > 0 {
                (line_len / inner_width) + 1
            } else {
                1
            }
        })
        .sum();

    let scroll_offset = total_wrapped.saturating_sub(visible_height);

    let block = Block::default()
        .title(Span::styled(
            " Logs ",
            Style::default().fg(theme.highlight),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    let paragraph = Paragraph::new(display_lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset as u16, 0));

    frame.render_widget(paragraph, area);

    if total_wrapped > visible_height {
        let mut scrollbar_state =
            ScrollbarState::new(total_wrapped.saturating_sub(visible_height))
                .position(scroll_offset);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut scrollbar_state,
        );
    }
}

/// Parse a log line (JSON from tracing_subscriber or plain text) into a colorized Line.
fn parse_log_line<'a>(raw: &str, theme: Theme) -> Line<'a> {
    // Try JSON parse first (tracing_subscriber json format)
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        return format_json_log(&v, theme);
    }

    // Fallback: plain text with simple level detection
    let style = if raw.contains(" WARN ") || raw.contains(" WARN:") {
        Style::default().fg(Color::Yellow)
    } else if raw.contains(" ERROR ") || raw.contains(" ERROR:") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(theme.muted)
    };
    Line::from(Span::styled(raw.to_string(), style))
}

/// Format a JSON tracing log entry into a colorized Line.
fn format_json_log<'a>(v: &serde_json::Value, theme: Theme) -> Line<'a> {
    let level = v
        .get("level")
        .and_then(|l| l.as_str())
        .unwrap_or("INFO");

    let target = v
        .get("target")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    // tracing_subscriber puts message in fields.message
    let message = v
        .get("fields")
        .and_then(|f| f.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
        .unwrap_or("");

    // Extract timestamp - just show HH:MM:SS
    let time_str = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| {
            // "2024-01-01T12:34:56.789Z" -> "12:34:56"
            t.find('T')
                .and_then(|pos| t.get(pos + 1..pos + 9))
        })
        .unwrap_or("");

    let (level_color, level_tag) = match level.to_uppercase().as_str() {
        "ERROR" => (Color::Red, "ERROR"),
        "WARN" => (Color::Yellow, " WARN"),
        "INFO" => (Color::Green, " INFO"),
        "DEBUG" => (Color::Cyan, "DEBUG"),
        "TRACE" => (Color::Magenta, "TRACE"),
        _ => (theme.muted, "  ???"),
    };

    // Shorten target: "polyphony_orchestrator::runtime" -> "runtime"
    let short_target = target
        .rsplit("::")
        .next()
        .unwrap_or(target);

    // Collect extra fields (skip timestamp, level, target, fields.message, span, spans)
    let mut extras: Vec<String> = Vec::new();
    if let Some(fields) = v.get("fields").and_then(|f| f.as_object()) {
        for (k, val) in fields {
            if k == "message" {
                continue;
            }
            let val_str = match val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => continue,
            };
            extras.push(format!("{k}={val_str}"));
        }
    }

    let mut spans = vec![
        Span::styled(
            format!("{time_str} "),
            Style::default().fg(theme.muted),
        ),
        Span::styled(
            format!("{level_tag} "),
            Style::default()
                .fg(level_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if !short_target.is_empty() {
        spans.push(Span::styled(
            format!("{short_target}: "),
            Style::default().fg(Color::Blue),
        ));
    }

    let msg_style = match level.to_uppercase().as_str() {
        "ERROR" => Style::default().fg(Color::Red),
        "WARN" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(theme.foreground),
    };
    spans.push(Span::styled(message.to_string(), msg_style));

    if !extras.is_empty() {
        spans.push(Span::styled(
            format!(" {}", extras.join(" ")),
            Style::default().fg(theme.muted),
        ));
    }

    Line::from(spans)
}

// ─── Network panel ───────────────────────────────────────────────────

fn draw_network_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;

    // Row 1: Req/sec and pending count
    let pending = pending_count(snapshot);
    let req_sec = format!("{:.1}", app.requests_per_sec);

    let row1 = Line::from(vec![
        Span::styled("Req/sec: ", Style::default().fg(theme.muted)),
        Span::styled(&req_sec, Style::default().fg(theme.foreground)),
        Span::styled("   Pending: ", Style::default().fg(theme.muted)),
        Span::styled(
            pending.to_string(),
            Style::default().fg(if pending > 0 {
                Color::Yellow
            } else {
                theme.foreground
            }),
        ),
    ]);

    // Row 2: Per-component rate limits
    let budget_lines = budget_spans(snapshot, theme);

    // Row 3: Throttles with remaining time
    let throttle_line = throttle_spans(snapshot, theme);

    let mut lines = vec![row1];
    lines.extend(budget_lines);
    lines.push(throttle_line);

    let block = Block::default()
        .title(Span::styled(
            " Network ",
            Style::default().fg(theme.highlight),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn pending_count(snapshot: &RuntimeSnapshot) -> usize {
    let mut count = snapshot.running.len();
    let loading = &snapshot.loading;
    if loading.fetching_issues {
        count += 1;
    }
    if loading.fetching_budgets {
        count += 1;
    }
    if loading.fetching_models {
        count += 1;
    }
    if loading.reconciling {
        count += 1;
    }
    count
}

fn budget_spans<'a>(snapshot: &RuntimeSnapshot, theme: Theme) -> Vec<Line<'a>> {
    if snapshot.budgets.is_empty() {
        return vec![Line::from(Span::styled(
            "No rate limit data",
            Style::default().fg(theme.muted),
        ))];
    }

    snapshot
        .budgets
        .iter()
        .map(|b| {
            let remaining = b.credits_remaining.unwrap_or(0.0);
            let total = b.credits_total.unwrap_or(1.0);
            let ratio = if total > 0.0 {
                remaining / total
            } else {
                0.0
            };

            let color = if ratio > 0.5 {
                Color::Green
            } else if ratio > 0.2 {
                Color::Yellow
            } else {
                Color::Red
            };

            let remaining_str = format_number(remaining as u64);
            let total_str = format_number(total as u64);

            // Show reset countdown inline if available
            let mut spans = vec![
                Span::styled(
                    format!("{}: ", short_component(&b.component)),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{remaining_str}/{total_str}"),
                    Style::default().fg(color),
                ),
            ];

            if let Some(reset_at) = b.reset_at {
                let secs = reset_at
                    .signed_duration_since(Utc::now())
                    .num_seconds()
                    .max(0);
                if secs > 0 {
                    let mins = secs / 60;
                    let s = secs % 60;
                    spans.push(Span::styled(
                        format!(" resets {mins}m{s:02}s"),
                        Style::default().fg(theme.muted),
                    ));
                }
            }

            Line::from(spans)
        })
        .collect()
}

fn throttle_spans<'a>(snapshot: &RuntimeSnapshot, theme: Theme) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();

    if snapshot.throttles.is_empty() {
        spans.push(Span::styled(
            "Throttles: none",
            Style::default().fg(theme.muted),
        ));
    } else {
        spans.push(Span::styled(
            format!("Throttles: {} ", snapshot.throttles.len()),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
        let now = Utc::now();
        for (i, t) in snapshot.throttles.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(", ", Style::default().fg(theme.muted)));
            }
            let remaining_secs = t.until.signed_duration_since(now).num_seconds().max(0);
            spans.push(Span::styled(
                format!("{} {}s left", short_component(&t.component), remaining_secs),
                Style::default().fg(Color::Red),
            ));
        }
    }

    Line::from(spans)
}

/// Shorten component names like "tracker:github" -> "github"
fn short_component(component: &str) -> &str {
    component
        .rsplit(':')
        .next()
        .unwrap_or(component)
}

fn format_number(n: u64) -> String {
    if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        n.to_string()
    }
}
