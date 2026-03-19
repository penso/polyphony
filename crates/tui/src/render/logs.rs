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
use serde_json::Value;

use crate::{app::AppState, theme::Theme};

#[derive(Debug)]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub target: String,
    pub message: String,
    pub extras: String,
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

    // Only re-parse log entries when the buffer has grown.
    let current_len = app.log_buffer.len();
    if current_len != app.cached_log_entry_count {
        let raw_lines = app.log_buffer.all_lines();
        app.cached_log_entries = raw_lines.iter().map(|l| parse_log_entry(l)).collect();
        app.cached_log_entry_count = current_len;
    }

    let filtered: Vec<&LogEntry> = if app.logs_search_query.is_empty() {
        app.cached_log_entries.iter().collect()
    } else {
        let q = app.logs_search_query.to_lowercase();
        app.cached_log_entries
            .iter()
            .filter(|e| e.matches(&q))
            .collect()
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
    let msg_width =
        (area.width as usize).saturating_sub(2 + 2 + 9 + 6 + max_target_len as usize + 3);

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
    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let table = Table::new(rows, [
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(max_target_len),
        Constraint::Fill(1),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol("▸ ")
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
    let all_chars: Vec<char> = msg_chars
        .iter()
        .chain(extras_chars.iter())
        .copied()
        .collect();
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
        Line::from(Span::styled(" Logs ", Style::default().fg(theme.highlight)))
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

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Max(48),
            Constraint::Length(34),
            Constraint::Min(10),
        ])
        .split(area);

    let budget_block = Block::default()
        .title(Span::styled(
            " Budgets ",
            Style::default().fg(theme.highlight),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));
    frame.render_widget(
        Paragraph::new(provider_budget_lines(snapshot, theme)).block(budget_block),
        chunks[0],
    );

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
    let throttle_line = throttle_spans(snapshot, theme);
    let mut lines = vec![row1];
    lines.push(fetch_status_line(snapshot, theme));
    lines.push(throttle_line);

    let stats_block = Block::default()
        .title(Span::styled(
            " Network ",
            Style::default().fg(theme.highlight),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    frame.render_widget(Paragraph::new(lines).block(stats_block), chunks[1]);

    let data: Vec<u64> = app.rps_history.iter().rev().copied().collect();

    let sparkline_block = Block::default()
        .title(Span::styled(" Pace ", Style::default().fg(theme.highlight)))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));

    let sparkline = Sparkline::default()
        .block(sparkline_block)
        .data(&data)
        .max(data.iter().copied().max().unwrap_or(1).max(1))
        .direction(RenderDirection::RightToLeft)
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(sparkline, chunks[2]);
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
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
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

fn short_component(component: &str) -> &str {
    component.rsplit(':').next().unwrap_or(component)
}

fn fetch_status_line<'a>(snapshot: &RuntimeSnapshot, theme: Theme) -> Line<'a> {
    let loading = &snapshot.loading;
    if loading.fetching_budgets {
        return Line::from(vec![
            Span::styled("Budgets: ", Style::default().fg(theme.muted)),
            Span::styled("refreshing", Style::default().fg(theme.info)),
        ]);
    }
    if loading.fetching_issues || loading.fetching_models || loading.reconciling {
        return Line::from(vec![
            Span::styled("Runtime: ", Style::default().fg(theme.muted)),
            Span::styled("busy", Style::default().fg(theme.warning)),
        ]);
    }
    Line::from(vec![
        Span::styled("Runtime: ", Style::default().fg(theme.muted)),
        Span::styled("idle", Style::default().fg(theme.foreground)),
    ])
}

fn provider_budget_lines<'a>(snapshot: &RuntimeSnapshot, theme: Theme) -> Vec<Line<'a>> {
    let mut providers = provider_budget_summaries(snapshot);
    providers.sort_by(|left, right| {
        provider_rank(left.provider.as_str())
            .cmp(&provider_rank(right.provider.as_str()))
            .then_with(|| left.provider.cmp(&right.provider))
    });

    if providers.is_empty() {
        return vec![Line::from(Span::styled(
            "No provider budget data",
            Style::default().fg(theme.muted),
        ))];
    }

    let mut lines = Vec::new();
    for provider in providers.into_iter().take(2) {
        let label = capitalize(provider.provider.as_str());
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label:<7}"),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("S ", Style::default().fg(theme.muted)),
            Span::styled(
                format!(
                    "{:>3.0}% ",
                    provider.session_remaining_percent.unwrap_or(0.0)
                ),
                Style::default().fg(ratio_color(provider.session_remaining_percent, theme)),
            ),
            Span::styled(
                provider
                    .session_reset_at
                    .map(short_reset_label)
                    .unwrap_or_else(|| "n/a".into()),
                Style::default().fg(theme.muted),
            ),
        ]));

        let pace_label = if provider.weekly_deficit_percent > 0.0 {
            format!("Δ{:>2.0}%", provider.weekly_deficit_percent)
        } else if provider.weekly_reserve_percent > 0.0 {
            format!("R{:>2.0}%", provider.weekly_reserve_percent)
        } else {
            "On pace".into()
        };
        lines.push(Line::from(vec![
            Span::styled("       W ", Style::default().fg(theme.muted)),
            Span::styled(
                format!(
                    "{:>3.0}% ",
                    provider.weekly_remaining_percent.unwrap_or(0.0)
                ),
                Style::default().fg(ratio_color(provider.weekly_remaining_percent, theme)),
            ),
            Span::styled(
                pace_label,
                Style::default().fg(if provider.weekly_deficit_percent > 0.0 {
                    theme.danger
                } else {
                    theme.info
                }),
            ),
            Span::styled(" ", Style::default().fg(theme.muted)),
            Span::styled(
                provider
                    .weekly_eta_seconds
                    .map(short_eta_label)
                    .or_else(|| provider.weekly_reset_at.map(short_reset_label))
                    .unwrap_or_else(|| "n/a".into()),
                Style::default().fg(theme.muted),
            ),
        ]));
    }
    lines
}

#[derive(Clone)]
struct ProviderBudgetSummary {
    provider: String,
    session_remaining_percent: Option<f64>,
    session_reset_at: Option<chrono::DateTime<Utc>>,
    weekly_remaining_percent: Option<f64>,
    weekly_deficit_percent: f64,
    weekly_reserve_percent: f64,
    weekly_eta_seconds: Option<i64>,
    weekly_reset_at: Option<chrono::DateTime<Utc>>,
}

fn provider_budget_summaries(snapshot: &RuntimeSnapshot) -> Vec<ProviderBudgetSummary> {
    use std::collections::BTreeMap;

    let mut providers = BTreeMap::new();
    for budget in &snapshot.budgets {
        let Some(raw) = budget.raw.as_ref() else {
            continue;
        };
        let Some(provider) = raw.get("provider").and_then(Value::as_str) else {
            continue;
        };
        providers
            .entry(provider.to_string())
            .and_modify(|existing: &mut ProviderBudgetSummary| {
                if budget.captured_at > existing.weekly_reset_at.unwrap_or(budget.captured_at) {
                    *existing = provider_budget_summary(provider, budget);
                }
            })
            .or_insert_with(|| provider_budget_summary(provider, budget));
    }
    providers.into_values().collect()
}

fn provider_budget_summary(
    provider: &str,
    budget: &polyphony_core::BudgetSnapshot,
) -> ProviderBudgetSummary {
    let raw = budget.raw.as_ref();
    ProviderBudgetSummary {
        provider: provider.to_string(),
        session_remaining_percent: raw
            .and_then(|value| value.pointer("/session/remaining_percent"))
            .and_then(Value::as_f64)
            .or(budget.credits_remaining),
        session_reset_at: raw
            .and_then(|value| value.pointer("/session/reset_at"))
            .and_then(Value::as_str)
            .and_then(parse_rfc3339),
        weekly_remaining_percent: raw
            .and_then(|value| value.pointer("/weekly/remaining_percent"))
            .and_then(Value::as_f64),
        weekly_deficit_percent: raw
            .and_then(|value| value.pointer("/weekly/deficit_percent"))
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        weekly_reserve_percent: raw
            .and_then(|value| value.pointer("/weekly/reserve_percent"))
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        weekly_eta_seconds: raw
            .and_then(|value| value.pointer("/weekly/eta_seconds"))
            .and_then(Value::as_i64),
        weekly_reset_at: raw
            .and_then(|value| value.pointer("/weekly/reset_at"))
            .and_then(Value::as_str)
            .and_then(parse_rfc3339),
    }
}

fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn short_reset_label(reset_at: chrono::DateTime<Utc>) -> String {
    short_eta_label(reset_at.signed_duration_since(Utc::now()).num_seconds())
}

fn short_eta_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn ratio_color(value: Option<f64>, theme: Theme) -> Color {
    match value.unwrap_or(0.0) {
        value if value > 50.0 => Color::Green,
        value if value > 20.0 => Color::Yellow,
        value if value > 0.0 => theme.warning,
        _ => theme.danger,
    }
}

fn provider_rank(provider: &str) -> u8 {
    match provider {
        "codex" => 0,
        "claude" => 1,
        _ => 2,
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
