use {
    polyphony_core::RuntimeSnapshot,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Cell, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation,
            ScrollbarState, Table,
        },
    },
};

use crate::app::AppState;

pub fn draw_deliverables_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;

    let deliverables: Vec<_> = snapshot
        .movements
        .iter()
        .filter(|m| m.has_deliverable)
        .collect();

    let header = Row::new(vec![
        Cell::from(Span::styled("Issue", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Status", Style::default().fg(theme.muted))),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = deliverables
        .iter()
        .map(|m| {
            let status_color = movement_status_color(&m.status, theme);

            Row::new(vec![
                Cell::from(Span::styled(
                    m.issue_identifier.clone().unwrap_or_default(),
                    Style::default().fg(theme.info),
                )),
                Cell::from(Span::styled(
                    m.title.clone(),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    m.status.to_string(),
                    Style::default().fg(status_color),
                )),
            ])
        })
        .collect();

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = deliverables.len();
    let footer_info = if count == 0 {
        "no deliverables".into()
    } else {
        format!(
            "{} of {count}",
            app.deliverables_state.selected().unwrap_or_default() + 1
        )
    };

    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Fill(1),
        Constraint::Length(14),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol("▸ ")
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Outputs ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(Span::styled(
                    format!("─{footer_info}─"),
                    Style::default().fg(theme.muted),
                ))
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border)),
    );

    frame.render_stateful_widget(table, area, &mut app.deliverables_state);

    if count > 0 {
        let content_height = area.height.saturating_sub(3) as usize;
        if count > content_height {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.deliverables_state.selected().unwrap_or(0))
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
}

fn movement_status_color(
    status: &polyphony_core::MovementStatus,
    theme: crate::theme::Theme,
) -> ratatui::style::Color {
    use polyphony_core::MovementStatus;
    match status {
        MovementStatus::Pending | MovementStatus::Planning => theme.info,
        MovementStatus::InProgress => theme.success,
        MovementStatus::Review => theme.highlight,
        MovementStatus::Delivered => theme.success,
        MovementStatus::Failed => theme.danger,
        MovementStatus::Cancelled => theme.muted,
    }
}
