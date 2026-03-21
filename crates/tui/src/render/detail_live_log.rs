use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::detail_common::render_scroll_indicator;
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
            ..
        }) = app.current_detail()
        {
            // Check if the agent is still running
            let still_running = snapshot
                .running
                .iter()
                .any(|r| r.agent_name == *agent_name && r.issue_identifier == *issue_identifier);
            // Also check if the log file is still growing (fallback: file exists)
            let file_exists = log_path.exists();
            (
                agent_name.clone(),
                issue_identifier.clone(),
                cached_content.clone(),
                still_running && file_exists,
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

    let lines: Vec<Line<'_>> = if cached_content.is_empty() {
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
            .map(|line| Line::from(Span::styled(line.to_owned(), Style::default().fg(theme.foreground))))
            .collect()
    };

    let visible_height = inner.height as usize;
    let total_lines = lines.len();
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
        inner,
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
