use chrono::Utc;
use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, HighlightSpacing, Padding, Paragraph, RenderDirection,
        Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline, Table,
    },
};

use crate::app::AppState;
use crate::theme::Theme;

struct LogEntry {
    time: String,
    level: String,
    target: String,
    message: String,
    extras: String,
}

impl LogEntry {
    fn matches(&self, query: &str) -> bool {
        self.time.to_lowercase().contains(query)
            || self.level.to_lowercase().contains(query)
            || self.target.to_lowercase().contains(query)
            || self.message.to_lowercase().contains(query)
            || self.extras.to_lowercase().contains(query)
    }
}

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
    let raw_lines = app.log_buffer.recent_lines(500);

    let entries: Vec<LogEntry> = raw_lines.iter().map(|l| parse_log_entry(l)).collect();

    let filtered: Vec<&LogEntry> = if app.logs_search_query.is_empty() {
        entries.iter().collect()
    } else {
        let q = app.logs_search_query.to_lowercase();
        entries.iter().filter(|e| e.matches(&q)).collect()
    };

    let count = filtered.len();

    // Auto-scroll: keep selection at bottom when enabled
    if app.logs_auto_scroll && count > 0 {
        app.logs_state.select(Some(count - 1));
    }

    // Clamp selection to valid range
    if count == 0 {
        app.logs_state.select(None);
    } else if let Some(sel) = app.logs_state.selected() {
        if sel >= count {
            app.logs_state.select(Some(count - 1));
        }
    } else {
        app.logs_state.select(Some(count - 1));
    }

    // Compute max target width (capped at 20, min 6 for header)
    let max_target_len = filtered
        .iter()
        .map(|e| e.target.len())
        .max()
        .unwrap_or(6)
        .clamp(6, 20) as u16;

    // Compute available message column width for wrapping
    // area.width - 2 (borders) - 2 (L+R padding) - 9 - 6 - target - 3 (column gaps)
    let msg_width = (area.width as usize)
        .saturating_sub(2 + 2 + 9 + 6 + max_target_len as usize + 3);

    let header = Row::new(vec![
        Cell::from(
            Line::from(Span::styled("Time", Style::default().fg(theme.muted)))
                .alignment(Alignment::Center),
        ),
        Cell::from(Span::styled("Level", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Target", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Message", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = filtered
        .iter()
        .map(|entry| {
            let (level_color, msg_style) = level_styles(&entry.level, theme);
            let (msg_cell, row_height) =
                wrap_message_cell(&entry.message, &entry.extras, msg_width, theme, msg_style);
            Row::new(vec![
                Cell::from(Span::styled(
                    entry.time.clone(),
                    Style::default().fg(theme.muted),
                )),
                Cell::from(Span::styled(
                    format_level_tag(&entry.level),
                    Style::default()
                        .fg(level_color)
                        .add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    entry.target.clone(),
                    Style::default().fg(Color::Blue),
                )),
                msg_cell,
            ])
            .height(row_height)
        })
        .collect();

    let title = build_logs_title(&app.logs_search_query, app.logs_search_active, theme);

    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(max_target_len),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .highlight_spacing(HighlightSpacing::Always)
    .block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .padding(Padding::new(1, 1, 0, 0)),
    );

    frame.render_stateful_widget(table, area, &mut app.logs_state);
    draw_scrollbar(frame, area, count, app.logs_state.selected().unwrap_or(0));
}

// ─── Log parsing ─────────────────────────────────────────────────────

fn parse_log_entry(raw: &str) -> LogEntry {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        return parse_json_log_entry(&v);
    }
    // Fallback: plain text with simple level detection
    let level = if raw.contains(" ERROR ") || raw.contains(" ERROR:") {
        "ERROR"
    } else if raw.contains(" WARN ") || raw.contains(" WARN:") {
        "WARN"
    } else {
        "INFO"
    };
    LogEntry {
        time: String::new(),
        level: level.to_string(),
        target: String::new(),
        message: raw.to_string(),
        extras: String::new(),
    }
}

fn parse_json_log_entry(v: &serde_json::Value) -> LogEntry {
    let level = v
        .get("level")
        .and_then(|l| l.as_str())
        .unwrap_or("INFO")
        .to_string();

    let target = v.get("target").and_then(|t| t.as_str()).unwrap_or("");
    let short_target = target
        .strip_prefix("polyphony_")
        .or_else(|| target.strip_prefix("polyphony"))
        .unwrap_or(target)
        .rsplit("::")
        .next()
        .unwrap_or(target)
        .to_string();

    let message = v
        .get("fields")
        .and_then(|f| f.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
        .unwrap_or("")
        .to_string();

    let time = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| t.find('T').and_then(|pos| t.get(pos + 1..pos + 9)))
        .unwrap_or("")
        .to_string();

    let mut extras_parts: Vec<String> = Vec::new();
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
            extras_parts.push(format!("{k}={val_str}"));
        }
    }

    LogEntry {
        time,
        level,
        target: short_target,
        message,
        extras: extras_parts.join(" "),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn wrap_message_cell<'a>(
    message: &str,
    extras: &str,
    width: usize,
    theme: Theme,
    msg_style: Style,
) -> (Cell<'a>, u16) {
    if width == 0 {
        return (Cell::from(""), 1);
    }

    let extras_style = Style::default().fg(theme.muted);
    let msg_chars: Vec<char> = message.chars().collect();
    let extras_chars: Vec<char> = if extras.is_empty() {
        Vec::new()
    } else {
        format!(" {extras}").chars().collect()
    };

    // Combine all chars; msg_chars.len() is the boundary
    let all_chars: Vec<char> = msg_chars.iter().chain(extras_chars.iter()).copied().collect();
    let msg_boundary = msg_chars.len();

    if all_chars.is_empty() {
        return (Cell::from(""), 1);
    }

    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut pos = 0;
    while pos < all_chars.len() {
        let end = (pos + width).min(all_chars.len());
        let mut spans: Vec<Span<'a>> = Vec::new();

        if pos < msg_boundary {
            let msg_end = end.min(msg_boundary);
            spans.push(Span::styled(
                all_chars[pos..msg_end].iter().collect::<String>(),
                msg_style,
            ));
            if msg_end < end {
                spans.push(Span::styled(
                    all_chars[msg_end..end].iter().collect::<String>(),
                    extras_style,
                ));
            }
        } else {
            spans.push(Span::styled(
                all_chars[pos..end].iter().collect::<String>(),
                extras_style,
            ));
        }

        lines.push(Line::from(spans));
        pos = end;
    }

    let height = lines.len().max(1) as u16;
    (Cell::from(Text::from(lines)), height)
}

fn level_styles(level: &str, theme: Theme) -> (Color, Style) {
    match level.to_uppercase().as_str() {
        "ERROR" => (Color::Red, Style::default().fg(Color::Red)),
        "WARN" => (Color::Yellow, Style::default().fg(Color::Yellow)),
        "INFO" => (Color::Green, Style::default().fg(theme.foreground)),
        "DEBUG" => (Color::Cyan, Style::default().fg(theme.foreground)),
        "TRACE" => (Color::Magenta, Style::default().fg(theme.foreground)),
        _ => (theme.muted, Style::default().fg(theme.foreground)),
    }
}

fn format_level_tag(level: &str) -> String {
    match level.to_uppercase().as_str() {
        "ERROR" => "ERROR".to_string(),
        "WARN" => " WARN".to_string(),
        "INFO" => " INFO".to_string(),
        "DEBUG" => "DEBUG".to_string(),
        "TRACE" => "TRACE".to_string(),
        _ => "  ???".to_string(),
    }
}

fn build_logs_title<'a>(query: &str, typing: bool, theme: Theme) -> Line<'a> {
    if typing {
        Line::from(vec![
            Span::styled(" Logs ", Style::default().fg(theme.highlight)),
            Span::styled(
                format!("/{query}\u{258F}"),
                Style::default().fg(theme.foreground),
            ),
        ])
    } else if !query.is_empty() {
        Line::from(vec![
            Span::styled(" Logs ", Style::default().fg(theme.highlight)),
            Span::styled(format!("[{query}] "), Style::default().fg(theme.info)),
        ])
    } else {
        Line::from(Span::styled(
            " Logs ",
            Style::default().fg(theme.highlight),
        ))
    }
}

fn draw_scrollbar(frame: &mut ratatui::Frame<'_>, area: Rect, count: usize, position: usize) {
    let content_height = area.height.saturating_sub(3) as usize;
    if count > content_height {
        let mut scrollbar_state = ScrollbarState::new(count)
            .position(position)
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

// ─── Network panel ───────────────────────────────────────────────────

fn draw_network_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;

    // Split: stats left, sparkline right
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(38), Constraint::Min(10)])
        .split(area);

    // ── Left: stats ──
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

    let budget_lines = budget_spans(snapshot, theme);
    let throttle_line = throttle_spans(snapshot, theme);

    let mut lines = vec![row1];
    lines.extend(budget_lines);
    lines.push(throttle_line);

    let stats_block = Block::default()
        .title(Span::styled(
            " Network ",
            Style::default().fg(theme.highlight),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    frame.render_widget(Paragraph::new(lines).block(stats_block), chunks[0]);

    // ── Right: sparkline ──
    let data: Vec<u64> = app.rps_history.iter().rev().copied().collect();

    let sparkline_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    let sparkline = Sparkline::default()
        .block(sparkline_block)
        .data(&data)
        .max(data.iter().copied().max().unwrap_or(1).max(1))
        .direction(RenderDirection::RightToLeft)
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(sparkline, chunks[1]);
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
    component.rsplit(':').next().unwrap_or(component)
}

fn format_number(n: u64) -> String {
    if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        n.to_string()
    }
}
