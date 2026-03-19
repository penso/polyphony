use chrono::Utc;
use polyphony_core::{IssueApprovalState, RuntimeSnapshot, VisibleTriggerRow};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::detail_common::{
    format_relative_time, label_color, render_scroll_indicator, render_separator, strip_html_tags,
};
use crate::app::{AppState, DetailSection, DetailView};

pub(crate) fn draw_trigger_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    trigger_id: &str,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let Some(issue) = snapshot
        .visible_triggers
        .iter()
        .find(|t| t.trigger_id == trigger_id)
    else {
        draw_not_found(frame, area, "Trigger no longer available", theme);
        return;
    };

    let hint_spans = detail_hint_spans(issue, theme);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                format!("{} {} ", issue.source, issue.kind),
                Style::default().fg(theme.info),
            ),
            Span::styled(
                format!("{} ", issue.trigger_id),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(Line::from(hint_spans).right_aligned())
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.detail_border_focused {
            theme.highlight
        } else {
            theme.border
        }))
        .style(Style::default().bg(theme.panel_alt));

    frame.render_widget(&block, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });

    // Compute title height
    let title_width = inner.width as usize;
    let title_lines_count = (if title_width > 0 {
        issue.title.len().div_ceil(title_width)
    } else {
        1
    })
    .clamp(1, 3);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(title_lines_count as u16),
            Constraint::Length(1), // Meta
            Constraint::Length(1), // Indicator
            Constraint::Length(1), // Separator
            Constraint::Min(1),    // Body
        ])
        .split(inner);

    // Row 0: Title with created time
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

    // Row 1: Status | Priority | Labels | Author | Updated
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

    let approval_icon = match issue.approval_state {
        IssueApprovalState::Approved => "✓",
        IssueApprovalState::Waiting => "◷",
    };
    let approval_color = match issue.approval_state {
        IssueApprovalState::Approved => theme.success,
        IssueApprovalState::Waiting => theme.warning,
    };

    let sep = Span::styled("  ", Style::default());
    let mut meta_spans: Vec<Span<'_>> = vec![
        Span::styled(" status:", Style::default().fg(theme.muted)),
        Span::styled(
            format!("{} ", issue.status),
            Style::default().fg(state_color),
        ),
        sep.clone(),
        Span::styled("approval:", Style::default().fg(theme.muted)),
        Span::styled(
            format!("{approval_icon} "),
            Style::default().fg(approval_color),
        ),
        sep.clone(),
        Span::styled("priority:", Style::default().fg(theme.muted)),
        Span::styled(
            format!("{priority_str} "),
            Style::default().fg(priority_color),
        ),
    ];

    if !issue.labels.is_empty() {
        meta_spans.push(sep.clone());
        meta_spans.push(Span::styled("labels:", Style::default().fg(theme.muted)));
        for (i, label) in issue.labels.iter().enumerate() {
            if i > 0 {
                meta_spans.push(Span::styled(",", Style::default().fg(theme.muted)));
            }
            meta_spans.push(Span::styled(
                label.clone(),
                Style::default().fg(label_color(label, theme)),
            ));
        }
        meta_spans.push(Span::styled(" ", Style::default()));
    }
    if let Some(author) = &issue.author {
        meta_spans.push(sep.clone());
        meta_spans.push(Span::styled("author:", Style::default().fg(theme.muted)));
        meta_spans.push(Span::styled(
            format!("@{author} "),
            Style::default().fg(theme.highlight),
        ));
    }
    if let Some(updated) = issue.updated_at {
        let age = format_relative_time(updated, Utc::now());
        meta_spans.push(sep);
        meta_spans.push(Span::styled("updated:", Style::default().fg(theme.muted)));
        meta_spans.push(Span::styled(
            format!("{age} ago "),
            Style::default().fg(theme.muted),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(meta_spans)), rows[1]);

    // Row 2: Indicator legend
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
        legend_spans.push(Span::styled(
            "workspace active",
            Style::default().fg(theme.muted),
        ));
    } else {
        legend_spans.push(Span::styled(
            "  no workspace",
            Style::default().fg(theme.muted),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(legend_spans)), rows[2]);

    // Row 3: Separator
    render_separator(frame, rows[3], inner.width, theme);

    // Row 4: Scrollable body — description + related entities
    let desc_raw = issue.description.as_deref().unwrap_or("No description.");
    let desc_cleaned = strip_html_tags(desc_raw);
    let desc_widget = tui_markdown::from_str(&desc_cleaned);

    let mut body_lines: Vec<Line<'_>> = desc_widget.lines;

    // Read focus state from the detail stack
    let (focus, movements_selected, agents_selected) = match app.current_detail() {
        Some(DetailView::Trigger {
            focus,
            movements_selected,
            agents_selected,
            ..
        }) => (*focus, *movements_selected, *agents_selected),
        _ => (DetailSection::Body, 0, 0),
    };
    let movements_focused = focus == DetailSection::Section(0);
    let agents_focused = focus == DetailSection::Section(1);

    // Related movements — sorted by creation time (oldest first, newest at bottom)
    let mut related_movements: Vec<_> = snapshot
        .movements
        .iter()
        .filter(|m| m.issue_identifier.as_deref() == Some(&*issue.identifier))
        .collect();
    related_movements.sort_by_key(|m| m.created_at);
    if !related_movements.is_empty() {
        body_lines.push(Line::default());
        let section_marker = if movements_focused {
            "▸ "
        } else {
            "  "
        };
        body_lines.push(Line::from(vec![
            Span::styled(section_marker, Style::default().fg(theme.highlight)),
            Span::styled(
                "Related Movements",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        // Pre-compute max column widths for alignment
        let max_kind_len = related_movements
            .iter()
            .map(|m| super::orchestrator::movement_kind_label(m.kind).len())
            .max()
            .unwrap_or(0);
        let max_target_len = related_movements
            .iter()
            .map(|m| super::orchestrator::movement_target_label(m).len())
            .max()
            .unwrap_or(0);
        let max_status_len = related_movements
            .iter()
            .map(|m| m.status.to_string().len())
            .max()
            .unwrap_or(0);
        let max_completed_len = related_movements
            .iter()
            .map(|m| format!("{}", m.tasks_completed).len())
            .max()
            .unwrap_or(1);
        let max_total_len = related_movements
            .iter()
            .map(|m| format!("{}", m.task_count).len())
            .max()
            .unwrap_or(1);

        for (i, m) in related_movements.iter().enumerate() {
            let (status_emoji, emoji_color) =
                super::orchestrator::movement_status_emoji_pub(&m.status, theme);
            let status_color = super::orchestrator::movement_status_color_pub(&m.status, theme);
            let is_selected = movements_focused && i == movements_selected;
            let prefix = if is_selected {
                "▸ "
            } else {
                "  "
            };
            let name_style = if is_selected {
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            let kind_label = super::orchestrator::movement_kind_label(m.kind);
            let target = super::orchestrator::movement_target_label(m);
            let status_str = m.status.to_string();
            let ts = m.created_at.format("%Y-%m-%d %H:%M").to_string();
            body_lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.highlight)),
                Span::styled(format!("{ts} "), Style::default().fg(theme.muted)),
                Span::styled(format!("{status_emoji} "), Style::default().fg(emoji_color)),
                Span::styled(
                    format!("{kind_label:<max_kind_len$}  "),
                    Style::default().fg(theme.info),
                ),
                Span::styled(format!("{target:<max_target_len$}  "), name_style),
                Span::styled(
                    format!("{status_str:<max_status_len$}  "),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    format!(
                        "{:>max_completed_len$}/{:<max_total_len$}",
                        m.tasks_completed, m.task_count
                    ),
                    Style::default().fg(theme.muted),
                ),
            ]));
        }
    }

    // Running agents for this trigger
    let running_agents: Vec<_> = snapshot
        .running
        .iter()
        .filter(|r| r.issue_id == issue.trigger_id)
        .collect();
    if !running_agents.is_empty() {
        body_lines.push(Line::default());
        let section_marker = if agents_focused {
            "▸ "
        } else {
            "  "
        };
        body_lines.push(Line::from(vec![
            Span::styled(section_marker, Style::default().fg(theme.highlight)),
            Span::styled(
                "Running Agents",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        for (i, agent) in running_agents.iter().enumerate() {
            let is_selected = agents_focused && i == agents_selected;
            let prefix = if is_selected {
                "▸ "
            } else {
                "  "
            };
            let name_style = if is_selected {
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            body_lines.push(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default().fg(if is_selected {
                        theme.highlight
                    } else {
                        theme.success
                    }),
                ),
                Span::styled(agent.agent_name.clone(), name_style),
                Span::styled(
                    format!("  turn {}/{} ", agent.turn_count, agent.max_turns),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
                    agent.model.as_deref().unwrap_or("-"),
                    Style::default().fg(theme.muted),
                ),
            ]));
        }
    }

    // Recent events for this trigger (compact: 3 most recent)
    body_lines.extend(super::orchestrator::compact_recent_event_lines(
        snapshot,
        &issue.identifier,
        theme,
    ));

    // Scrollable rendering
    let body_area = rows[4];
    let visible_height = body_area.height as usize;
    let total_lines = body_lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    let current_scroll = app.current_detail_scroll();
    if (current_scroll as usize) > max_scroll {
        app.set_current_detail_scroll(max_scroll as u16);
    }
    let scroll_pos = app.current_detail_scroll();

    frame.render_widget(
        Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_pos, 0)),
        body_area,
    );

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_pos as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            body_area,
            &mut scrollbar_state,
        );
    }

    render_scroll_indicator(
        frame,
        body_area,
        scroll_pos,
        total_lines,
        visible_height,
        theme,
    );
}

fn detail_hint_spans(issue: &VisibleTriggerRow, theme: crate::theme::Theme) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(" Tab", Style::default().fg(theme.highlight)),
        Span::styled(":focus  ", Style::default().fg(theme.muted)),
        Span::styled("j/k", Style::default().fg(theme.highlight)),
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
    spans.push(Span::styled("e", Style::default().fg(theme.highlight)));
    spans.push(Span::styled(":events  ", Style::default().fg(theme.muted)));
    spans.push(Span::styled("Esc", Style::default().fg(theme.highlight)));
    spans.push(Span::styled(":back ", Style::default().fg(theme.muted)));
    spans
}

fn draw_not_found(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    message: &str,
    theme: crate::theme::Theme,
) {
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_alt));
    frame.render_widget(&block, area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    frame.render_widget(
        Paragraph::new(Span::styled(message, Style::default().fg(theme.muted))),
        inner,
    );
}
