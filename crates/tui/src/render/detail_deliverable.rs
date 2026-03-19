use polyphony_core::{DeliverableDecision, RuntimeSnapshot};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::detail_common::{kv_line, render_scroll_indicator, render_separator};
use crate::app::AppState;

pub(crate) fn draw_deliverable_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    movement_id: &str,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let Some(movement) = snapshot.movements.iter().find(|m| m.id == movement_id) else {
        draw_not_found(frame, area, "Deliverable no longer available", theme);
        return;
    };

    let Some(deliverable) = &movement.deliverable else {
        draw_not_found(frame, area, "No deliverable on this movement", theme);
        return;
    };

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" Outcome ", Style::default().fg(theme.info)),
            Span::styled(
                format!(
                    "{} ",
                    super::deliverables::deliverable_label_pub(deliverable)
                ),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("j/k", Style::default().fg(theme.highlight)),
                Span::styled(":scroll  ", Style::default().fg(theme.muted)),
                Span::styled("Shift+O", Style::default().fg(theme.highlight)),
                Span::styled(":open  ", Style::default().fg(theme.muted)),
                Span::styled("a", Style::default().fg(theme.highlight)),
                Span::styled(":accept  ", Style::default().fg(theme.muted)),
                Span::styled("x", Style::default().fg(theme.highlight)),
                Span::styled(":reject  ", Style::default().fg(theme.muted)),
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

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    // Title
    frame.render_widget(
        Paragraph::new(Span::styled(
            movement.title.clone(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ))
        .wrap(Wrap { trim: false }),
        rows[0],
    );

    // Decision + flow
    let decision_color = match deliverable.decision {
        DeliverableDecision::Waiting => theme.warning,
        DeliverableDecision::Accepted => theme.success,
        DeliverableDecision::Rejected => theme.danger,
    };
    let meta = Line::from(vec![
        Span::styled(
            deliverable.decision.to_string(),
            Style::default().fg(decision_color),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            super::deliverables::flow_label_pub(movement),
            Style::default().fg(theme.info),
        ),
    ]);
    frame.render_widget(Paragraph::new(meta), rows[1]);
    render_separator(frame, rows[2], inner.width, theme);

    // Body
    let format_time = super::format_detail_time;
    let mut lines = vec![
        kv_line("ID", &movement.id, theme),
        kv_line(
            "Output",
            &super::deliverables::deliverable_label_pub(deliverable),
            theme,
        ),
        kv_line("Decision", &deliverable.decision.to_string(), theme),
        kv_line("Created", &format_time(movement.created_at), theme),
    ];

    if let Some(url) = &deliverable.url {
        lines.push(kv_line("URL", url, theme));
    }
    if let Some(workspace_path) = &movement.workspace_path {
        lines.push(kv_line(
            "Path",
            &workspace_path.display().to_string(),
            theme,
        ));
    }

    // Scrollable rendering
    let body_area = rows[3];
    let visible_height = body_area.height as usize;
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
