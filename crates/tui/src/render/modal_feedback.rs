use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Wrap},
};

use crate::app::AppState;

pub(crate) fn draw_feedback_modal(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let Some(modal) = app.feedback_modal.as_ref() else {
        return;
    };

    let theme = app.theme;
    let area = centered_rect(frame.area(), 78, frame.area().height.min(18));
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Inject Feedback ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(theme.highlight)),
                Span::styled(":agent  ", Style::default().fg(theme.muted)),
                Span::styled("Enter", Style::default().fg(theme.highlight)),
                Span::styled(":newline  ", Style::default().fg(theme.muted)),
                Span::styled("^D", Style::default().fg(theme.highlight)),
                Span::styled(":submit  ", Style::default().fg(theme.muted)),
                Span::styled("Esc", Style::default().fg(theme.highlight)),
                Span::styled(":cancel ", Style::default().fg(theme.muted)),
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
            Constraint::Length(2), // Run info
            Constraint::Length(1), // Agent label
            Constraint::Min(6),    // Textarea
        ])
        .split(inner);

    // Run info
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("run: ", Style::default().fg(theme.muted)),
                Span::styled(
                    modal.run_title.clone(),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "Instructions for continuing this run. The agent will work in the same workspace.",
                Style::default().fg(theme.muted),
            )),
        ])
        .wrap(Wrap { trim: false }),
        rows[0],
    );

    // Agent label
    let agent_label = modal.agent_name.as_deref().unwrap_or("default");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("agent: ", Style::default().fg(theme.muted)),
            Span::styled(agent_label.to_string(), Style::default().fg(theme.info)),
        ])),
        rows[1],
    );

    // Textarea
    let textarea_block = Block::default()
        .title(Line::from(Span::styled(
            " Feedback ",
            Style::default().fg(theme.info),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.highlight))
        .style(Style::default().bg(theme.panel_alt));
    let textarea_inner = textarea_block.inner(rows[2]);
    frame.render_widget(textarea_block, rows[2]);

    if modal.prompt.is_empty() {
        frame.render_widget(
            Paragraph::new(super::popups::wrap_text_for_textarea(
                "Describe what needs to change...",
                textarea_inner.width as usize,
                Some(Style::default().fg(theme.muted)),
            )),
            textarea_inner,
        );
    } else {
        frame.render_widget(
            Paragraph::new(super::popups::wrap_text_for_textarea(
                &modal.prompt,
                textarea_inner.width as usize,
                None,
            )),
            textarea_inner,
        );
    }

    let (line, col) = super::popups::visual_cursor_position(
        &modal.prompt,
        modal.cursor,
        textarea_inner.width as usize,
    );
    let cursor_x = textarea_inner.x + (col as u16).min(textarea_inner.width.saturating_sub(1));
    let cursor_y = textarea_inner.y + (line as u16).min(textarea_inner.height.saturating_sub(1));
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
