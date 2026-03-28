use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Wrap},
};

use crate::app::AppState;

pub(crate) fn draw_create_issue_modal(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let Some(modal) = app.create_issue_modal.as_ref() else {
        return;
    };

    let theme = app.theme;
    let area = centered_rect(frame.area(), 78, frame.area().height.min(20));
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Create Issue ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(theme.highlight)),
                Span::styled(":switch field  ", Style::default().fg(theme.muted)),
                Span::styled("^D", Style::default().fg(theme.highlight)),
                Span::styled(":create  ", Style::default().fg(theme.muted)),
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
            Constraint::Length(3), // Title field
            Constraint::Length(1), // Spacer
            Constraint::Min(4),    // Description field
        ])
        .split(inner);

    // Title field
    let title_focused = modal.cursor_field == 0;
    let title_border_color = if title_focused {
        theme.highlight
    } else {
        theme.border
    };
    let title_block = Block::default()
        .title(Line::from(Span::styled(
            " Title ",
            Style::default().fg(if title_focused {
                theme.info
            } else {
                theme.muted
            }),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(title_border_color))
        .style(Style::default().bg(theme.panel_alt));
    let title_inner = title_block.inner(rows[0]);
    frame.render_widget(title_block, rows[0]);

    if modal.title.is_empty() && !title_focused {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Issue title (required)",
                Style::default().fg(theme.muted),
            )),
            title_inner,
        );
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                modal.title.clone(),
                Style::default().fg(theme.foreground),
            )),
            title_inner,
        );
    }

    // Description field
    let desc_focused = modal.cursor_field == 1;
    let desc_border_color = if desc_focused {
        theme.highlight
    } else {
        theme.border
    };
    let desc_block = Block::default()
        .title(Line::from(Span::styled(
            " Description ",
            Style::default().fg(if desc_focused {
                theme.info
            } else {
                theme.muted
            }),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(desc_border_color))
        .style(Style::default().bg(theme.panel_alt));
    let desc_inner = desc_block.inner(rows[2]);
    frame.render_widget(desc_block, rows[2]);

    if modal.description.is_empty() && !desc_focused {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Optional description",
                Style::default().fg(theme.muted),
            )),
            desc_inner,
        );
    } else {
        frame.render_widget(
            Paragraph::new(super::popups::wrap_text_for_textarea(
                &modal.description,
                desc_inner.width as usize,
                None,
            ))
            .wrap(Wrap { trim: false }),
            desc_inner,
        );
    }

    // Cursor positioning
    let (cursor_area, text, pos) = if title_focused {
        (title_inner, &modal.title, modal.cursor_pos)
    } else {
        (desc_inner, &modal.description, modal.cursor_pos)
    };
    let (line, col) = super::popups::visual_cursor_position(text, pos, cursor_area.width as usize);
    let cursor_x = cursor_area.x + (col as u16).min(cursor_area.width.saturating_sub(1));
    let cursor_y = cursor_area.y + (line as u16).min(cursor_area.height.saturating_sub(1));
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
