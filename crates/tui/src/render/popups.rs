use {
    chrono::{DateTime, Utc},
    polyphony_core::{DispatchMode, RuntimeSnapshot, TrackerKind, VisibleIssueRow},
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
    issue: &VisibleIssueRow,
    tracker_kind: TrackerKind,
    app: &mut AppState,
) {
    let theme = app.theme;
    let max_w = frame.area().width.saturating_sub(4).min(100);
    let max_h = frame.area().height.saturating_sub(2).min(40);
    let area = centered_rect(frame.area(), max_w, max_h);
    frame.render_widget(Clear, area);

    let source_label = format!("{tracker_kind:?}");

    // Border title: source + identifier
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(format!(" {source_label} "), Style::default().fg(theme.info)),
            Span::styled(
                format!("{} ", issue.issue_id),
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
            Constraint::Length(1), // Separator
            Constraint::Min(1),    // Description
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

    // Description with markdown rendering and scroll.
    // Strip HTML tags first — GitHub issues often contain <details>, <img>, etc.
    // which tui-markdown doesn't support and would log warnings for.
    let desc_raw = issue.description.as_deref().unwrap_or("No description.");
    let desc_cleaned = strip_html_tags(desc_raw);

    let desc_widget = tui_markdown::from_str(&desc_cleaned);
    let desc_area = rows[3];
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

const MODE_OPTIONS: [(DispatchMode, &str, &str); 3] = [
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
];

pub fn draw_mode_modal(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot, app: &AppState) {
    let theme = app.theme;
    let area = centered_rect(frame.area(), 52, 11);
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

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // spacer
            Constraint::Length(2), // option 0
            Constraint::Length(1), // spacer
            Constraint::Length(2), // option 1
            Constraint::Length(1), // spacer
            Constraint::Length(2), // option 2
            Constraint::Min(0),    // remainder
        ])
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
        let row_lines = vec![
            Line::from(vec![
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::styled(*label, label_style),
            ]),
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(*desc, Style::default().fg(theme.muted)),
            ]),
        ];

        let bg = if is_selected {
            Style::default().bg(theme.panel_alt)
        } else {
            Style::default()
        };

        frame.render_widget(Paragraph::new(row_lines).style(bg), row_area);
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
