use {
    chrono::{DateTime, Utc},
    polyphony_core::VisibleIssueRow,
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

use crate::app::AppState;
use crate::theme::Theme;

pub fn draw_issue_detail_modal(
    frame: &mut ratatui::Frame<'_>,
    issue: &VisibleIssueRow,
    app: &mut AppState,
) {
    let theme = app.theme;
    let max_w = frame.area().width.saturating_sub(4).min(100);
    let max_h = frame.area().height.saturating_sub(2).min(40);
    let area = centered_rect(frame.area(), max_w, max_h);
    frame.render_widget(Clear, area);

    let source_label = if issue.issue_identifier.starts_with("GH-")
        || issue.issue_identifier.contains('#')
    {
        " GitHub"
    } else {
        "◆ Linear"
    };

    // Border title: source + identifier
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                format!(" {source_label} "),
                Style::default().fg(theme.info),
            ),
            Span::styled(
                format!("{} ", issue.issue_identifier),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled(" j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("o", Style::default().fg(theme.highlight)),
                Span::styled(":open  ", Style::default().fg(theme.muted)),
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

    // Compute title height: wrap title into available width
    let title_width = inner.width as usize;
    let title_lines_count = if title_width > 0 {
        (issue.title.len() + title_width - 1) / title_width
    } else {
        1
    }
    .max(1)
    .min(3); // cap at 3 lines

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(title_lines_count as u16), // Title (wraps) + created time
            Constraint::Length(1),                         // Status, priority, labels, author, updated
            Constraint::Length(1),                         // Separator
            Constraint::Min(1),                            // Description
        ])
        .split(inner);

    // Row 1: Title (wrapping) with created time right-aligned on first line
    let state_color = super::issues::state_color(&issue.state, theme);
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
        format!(" {} ", issue.state),
        Style::default().fg(state_color),
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

    // Separator
    render_separator(frame, rows[2], inner.width, theme);

    // Description with markdown rendering and scroll
    let desc_text = issue
        .description
        .as_deref()
        .unwrap_or("No description.");

    let desc_lines = render_markdown(desc_text, theme);
    let desc_area = rows[3];
    let visible_height = desc_area.height as usize;
    let total_lines = desc_lines.len();

    let max_scroll = total_lines.saturating_sub(visible_height);
    if (app.detail_scroll as usize) > max_scroll {
        app.detail_scroll = max_scroll as u16;
    }

    frame.render_widget(
        Paragraph::new(desc_lines)
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

/// Render markdown text into styled ratatui Lines.
fn render_markdown<'a>(text: &str, theme: Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut in_code_block = false;

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();

        // Code block fences
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                format!("  {raw_line}"),
                Style::default().fg(Color::Cyan),
            )));
            continue;
        }

        // Blank line
        if trimmed.is_empty() {
            lines.push(Line::default());
            continue;
        }

        // Headers
        if let Some(header_text) = trimmed.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                format!("  {header_text}"),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(header_text) = trimmed.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                header_text.to_string(),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(header_text) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                header_text.to_string(),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }

        // Checkboxes
        if let Some(rest) = trimmed.strip_prefix("- [x] ").or(trimmed.strip_prefix("- [X] ")) {
            lines.push(Line::from(vec![
                Span::styled("  ✓ ", Style::default().fg(Color::Green)),
                Span::styled(
                    inline_markdown(rest),
                    Style::default().fg(theme.foreground),
                ),
            ]));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            lines.push(Line::from(vec![
                Span::styled("  ☐ ", Style::default().fg(theme.muted)),
                Span::styled(
                    inline_markdown(rest),
                    Style::default().fg(theme.foreground),
                ),
            ]));
            continue;
        }

        // List items
        if let Some(rest) = trimmed.strip_prefix("- ").or(trimmed.strip_prefix("* ")) {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(theme.highlight)),
                Span::styled(
                    inline_markdown(rest),
                    Style::default().fg(theme.foreground),
                ),
            ]));
            continue;
        }

        // Numbered list
        if let Some(dot_pos) = trimmed.find(". ") {
            if dot_pos <= 3 && trimmed[..dot_pos].chars().all(|c| c.is_ascii_digit()) {
                let num = &trimmed[..dot_pos];
                let rest = &trimmed[dot_pos + 2..];
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {num}. "),
                        Style::default().fg(theme.highlight),
                    ),
                    Span::styled(
                        inline_markdown(rest),
                        Style::default().fg(theme.foreground),
                    ),
                ]));
                continue;
            }
        }

        // Blockquote
        if let Some(rest) = trimmed.strip_prefix("> ") {
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(theme.border)),
                Span::styled(
                    inline_markdown(rest),
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
            continue;
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "─".repeat(40),
                Style::default().fg(theme.border),
            )));
            continue;
        }

        // Regular paragraph text with inline markdown
        lines.push(Line::from(render_inline_spans(trimmed, theme)));
    }

    lines
}

/// Render inline markdown (bold, italic, code, links) into spans.
fn render_inline_spans<'a>(text: &str, theme: Theme) -> Vec<Span<'a>> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Bold: **text**
        if let Some(pos) = remaining.find("**") {
            if pos > 0 {
                spans.push(Span::styled(
                    remaining[..pos].to_string(),
                    Style::default().fg(theme.foreground),
                ));
            }
            remaining = &remaining[pos + 2..];
            if let Some(end) = remaining.find("**") {
                spans.push(Span::styled(
                    remaining[..end].to_string(),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ));
                remaining = &remaining[end + 2..];
                continue;
            }
            spans.push(Span::styled(
                "**".to_string(),
                Style::default().fg(theme.foreground),
            ));
            continue;
        }

        // Inline code: `text`
        if let Some(pos) = remaining.find('`') {
            if pos > 0 {
                spans.push(Span::styled(
                    remaining[..pos].to_string(),
                    Style::default().fg(theme.foreground),
                ));
            }
            remaining = &remaining[pos + 1..];
            if let Some(end) = remaining.find('`') {
                spans.push(Span::styled(
                    remaining[..end].to_string(),
                    Style::default().fg(Color::Cyan),
                ));
                remaining = &remaining[end + 1..];
                continue;
            }
            spans.push(Span::styled(
                "`".to_string(),
                Style::default().fg(theme.foreground),
            ));
            continue;
        }

        // Link: [text](url)
        if let Some(pos) = remaining.find('[') {
            if pos > 0 {
                spans.push(Span::styled(
                    remaining[..pos].to_string(),
                    Style::default().fg(theme.foreground),
                ));
            }
            remaining = &remaining[pos + 1..];
            if let Some(bracket_end) = remaining.find("](") {
                let link_text = &remaining[..bracket_end];
                remaining = &remaining[bracket_end + 2..];
                if let Some(paren_end) = remaining.find(')') {
                    spans.push(Span::styled(
                        link_text.to_string(),
                        Style::default()
                            .fg(theme.info)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    remaining = &remaining[paren_end + 1..];
                    continue;
                }
            }
            spans.push(Span::styled(
                "[".to_string(),
                Style::default().fg(theme.foreground),
            ));
            continue;
        }

        // Plain text — rest of the line
        spans.push(Span::styled(
            remaining.to_string(),
            Style::default().fg(theme.foreground),
        ));
        break;
    }

    spans
}

/// Simple inline markdown stripping for list items (returns plain string).
fn inline_markdown(text: &str) -> String {
    text.replace("**", "")
        .replace('`', "")
        .replace("__", "")
}

pub fn draw_leaving_modal(frame: &mut ratatui::Frame<'_>, theme: Theme) {
    let area = centered_rect(frame.area(), 24, 3);
    frame.render_widget(Clear, area);
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
