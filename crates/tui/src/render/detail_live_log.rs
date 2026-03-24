use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::{detail_common::render_scroll_indicator, orchestrator::wrapped_line_count};
use crate::app::AppState;

pub(crate) fn draw_live_log_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let (agent_name, issue_identifier, cached_content, is_running) =
        if let Some(crate::app::DetailView::LiveLog {
            agent_name,
            issue_identifier,
            cached_content,
            log_path,
            task_id,
            ..
        }) = app.current_detail()
        {
            let running_row_active = snapshot
                .running
                .iter()
                .any(|r| r.agent_name == *agent_name && r.issue_identifier == *issue_identifier);
            let task_active = task_id.as_ref().is_some_and(|task_id| {
                snapshot.tasks.iter().any(|task| {
                    task.id == *task_id && task.status == polyphony_core::TaskStatus::InProgress
                })
            });
            (
                agent_name.clone(),
                issue_identifier.clone(),
                cached_content.clone(),
                task_active || (running_row_active && log_path.exists()),
            )
        } else {
            return;
        };

    const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let spinner = BRAILLE_SPINNER[(app.frame_count / 4) as usize % BRAILLE_SPINNER.len()];

    let (status_label, status_color) = if is_running {
        (format!("{spinner} streaming"), theme.success)
    } else {
        ("finished".to_owned(), theme.muted)
    };

    let title = format!(" {agent_name} on {issue_identifier} ");
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{status_label}]"),
                Style::default().fg(status_color),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("G", Style::default().fg(theme.highlight)),
                Span::styled(":bottom  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":back ", Style::default().fg(theme.muted)),
            ])
            .right_aligned(),
        )
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

    let mut lines: Vec<Line<'_>> = if cached_content.is_empty() {
        if is_running {
            vec![Line::from(vec![
                Span::styled(format!("{spinner} "), Style::default().fg(theme.info)),
                Span::styled(
                    "Agent is thinking... waiting for terminal output",
                    Style::default().fg(theme.muted),
                ),
            ])]
        } else {
            vec![Line::from(Span::styled(
                "No output recorded.",
                Style::default().fg(theme.muted),
            ))]
        }
    } else {
        cached_content
            .lines()
            .map(|line| {
                let style = classify_log_line_style(line, theme);
                Line::from(Span::styled(line.to_owned(), style))
            })
            .collect()
    };

    // Append a separator when the agent has finished so the user can see the
    // end of the transcript without wondering whether more output is coming.
    if !is_running && !cached_content.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "── agent finished ──",
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::DIM),
        )));
    }

    // Reserve 1 column on the right for the scrollbar so it doesn't overlap text.
    let text_width = inner.width.saturating_sub(1).max(1);
    let text_area = Rect {
        width: text_width,
        ..inner
    };
    let visible_height = inner.height as usize;
    // Account for line wrapping when computing scroll limits.
    let total_lines = wrapped_line_count(&lines, text_width);
    let max_scroll = total_lines.saturating_sub(visible_height);
    let current_scroll = app.current_detail_scroll();
    if current_scroll as usize > max_scroll {
        app.set_current_detail_scroll(max_scroll as u16);
    }
    let scroll_pos = app.current_detail_scroll();

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_pos, 0)),
        text_area,
    );

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_pos as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            inner,
            &mut scrollbar_state,
        );
    }

    render_scroll_indicator(frame, inner, scroll_pos, total_lines, visible_height, theme);
}

/// Classify a transcript log line by content and return the appropriate style.
/// Mirrors the CSS classes used in the HTML cast replay (line-sent, line-agent, etc.).
fn classify_log_line_style(line: &str, theme: crate::theme::Theme) -> Style {
    if line.contains('→') {
        Style::default().fg(theme.success)
    } else if line.contains("✓ turn completed") || line.contains("✓ Tool:") {
        Style::default()
            .fg(theme.success)
            .add_modifier(Modifier::BOLD)
    } else if line.contains('✕') || line.contains("turn failed") {
        Style::default()
            .fg(theme.danger)
            .add_modifier(Modifier::BOLD)
    } else if line.contains("Agent:") {
        Style::default().fg(theme.warning)
    } else if line.contains("Prompt:") || line.contains("Plan:") {
        Style::default().fg(ratatui::style::Color::Magenta)
    } else if line.contains("Diff:") {
        Style::default().fg(theme.info)
    } else if line.contains("Output:") || line.starts_with("              ") {
        Style::default().fg(theme.muted)
    } else if line.contains("Tool:") || line.contains("Exec:") {
        Style::default()
            .fg(theme.info)
            .add_modifier(Modifier::BOLD)
    } else if line.contains('←') {
        Style::default().fg(theme.info)
    } else {
        Style::default().fg(theme.foreground)
    }
}
