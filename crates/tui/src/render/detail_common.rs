use {
    chrono::{DateTime, Utc},
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::Rect,
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::{
    app::{AppState, DetailView},
    theme::Theme,
};

pub(crate) fn kv_line<'a>(label: &'static str, value: &str, theme: Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(theme.muted)),
        Span::styled(value.to_string(), Style::default().fg(theme.foreground)),
    ])
}

/// Strip HTML tags from text, preserving content between tags.
pub(crate) fn strip_html_tags(input: &str) -> String {
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

/// Pick a color for a label based on common keywords.
pub(crate) fn label_color(label: &str, theme: Theme) -> Color {
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

pub(crate) fn format_relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
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

pub(crate) fn render_separator(frame: &mut ratatui::Frame<'_>, area: Rect, width: u16, theme: Theme) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(width as usize),
            Style::default().fg(theme.border),
        ))),
        area,
    );
}

/// Build a breadcrumb Line from the detail_stack entries and snapshot data.
pub(crate) fn build_breadcrumb<'a>(app: &AppState, snapshot: &RuntimeSnapshot) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    let theme = app.theme;

    // Give each breadcrumb entry more room when there are fewer entries.
    let max_title = match app.detail_stack.len() {
        0 | 1 => 80,
        2 => 40,
        _ => 25,
    };

    for (i, view) in app.detail_stack.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" › ", Style::default().fg(theme.muted)));
        }
        match view {
            DetailView::Trigger { trigger_id, .. } => {
                let title = snapshot
                    .visible_triggers
                    .iter()
                    .find(|t| t.trigger_id == *trigger_id)
                    .map(|t| truncate_str(&t.title, max_title))
                    .unwrap_or_else(|| trigger_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Movement { movement_id, .. } => {
                let title = snapshot
                    .movements
                    .iter()
                    .find(|m| m.id == *movement_id)
                    .map(|m| truncate_str(&m.title, max_title))
                    .unwrap_or_else(|| movement_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Task { task_id, .. } => {
                let title = snapshot
                    .tasks
                    .iter()
                    .find(|t| t.id == *task_id)
                    .map(|t| truncate_str(&t.title, max_title))
                    .unwrap_or_else(|| task_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Agent { agent_index, .. } => {
                let label = if let Some(running) = snapshot.running.get(*agent_index) {
                    format!("{} ({})", running.agent_name, running.issue_identifier)
                } else if let Some(history) = snapshot.agent_history.get(
                    agent_index.saturating_sub(snapshot.running.len()),
                ) {
                    format!("{} ({})", history.agent_name, history.issue_identifier)
                } else {
                    format!("Agent #{agent_index}")
                };
                spans.push(Span::styled(
                    label,
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Deliverable { movement_id, .. } => {
                let title = snapshot
                    .movements
                    .iter()
                    .find(|m| m.id == *movement_id)
                    .map(|m| truncate_str(&m.title, max_title))
                    .unwrap_or_else(|| movement_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Events { .. } => {
                spans.push(Span::styled(
                    "Events",
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                ));
            },
        }
    }

    Line::from(spans)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let keep = max_len.saturating_sub(1);
        let truncated: String = s.chars().take(keep).collect();
        format!("{truncated}…")
    }
}

/// Render a scroll position indicator ("line X/Y") at the bottom-right of the area.
/// Only shown when content exceeds the visible area.
pub(crate) fn render_scroll_indicator(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    scroll_pos: u16,
    total_lines: usize,
    visible_height: usize,
    theme: Theme,
) {
    if total_lines <= visible_height {
        return;
    }
    let label = format!(" {}/{} ", scroll_pos as usize + 1, total_lines);
    let label_len = label.len() as u16;
    if label_len >= area.width || area.height == 0 {
        return;
    }
    let indicator_area = Rect {
        x: area.x + area.width - label_len,
        y: area.y + area.height.saturating_sub(1),
        width: label_len,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Span::styled(label, Style::default().fg(theme.muted))),
        indicator_area,
    );
}

