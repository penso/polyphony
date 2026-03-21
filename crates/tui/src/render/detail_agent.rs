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

pub(crate) fn draw_agent_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    agent_index: usize,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Agent Session ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("c", Style::default().fg(theme.highlight)),
                Span::styled(":cast  ", Style::default().fg(theme.muted)),
                Span::styled("S", Style::default().fg(theme.highlight)),
                Span::styled(":stop  ", Style::default().fg(theme.muted)),
                Span::styled("w", Style::default().fg(theme.highlight)),
                Span::styled(":workspace  ", Style::default().fg(theme.muted)),
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

    // Resolve agent from sorted display index
    let agent = app.resolve_agent(snapshot, agent_index);
    let is_running = agent
        .as_ref()
        .is_some_and(|a| matches!(a, crate::app::SelectedAgentRow::Running(_)));

    // Get artifact cache from the detail stack
    let artifact_context =
        if let Some(crate::app::DetailView::Agent { artifact_cache, .. }) = app.current_detail() {
            artifact_cache
                .as_ref()
                .as_ref()
                .and_then(|a| a.saved_context.as_ref())
        } else {
            None
        };

    let lines = agent
        .map(|a| super::agents::build_agent_detail_lines(snapshot, a, artifact_context, theme, app.frame_count))
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "No agent run selected.",
                Style::default().fg(theme.muted),
            ))]
        });

    let visible_height = inner.height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);

    // Auto-scroll for running agents. Content grows between frames so we allow
    // a tolerance: if the user was within 10 lines of the previous bottom, they
    // were "following" and we keep them at the new bottom. Scrolling further up
    // (e.g. pressing k multiple times) disengages auto-scroll.
    let current_scroll = app.current_detail_scroll();
    if is_running
        && (current_scroll == u16::MAX
            || max_scroll.saturating_sub(current_scroll as usize) <= 10)
    {
        app.set_current_detail_scroll(max_scroll as u16);
    } else if current_scroll as usize > max_scroll {
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
