use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};

use crate::app::AppState;

pub(crate) fn draw_add_repo_modal(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let Some(modal) = app.add_repo_modal.as_ref() else {
        return;
    };

    let theme = app.theme;
    let area = centered_rect(frame.area(), 82, 12);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Add Repository ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )))
        .title_bottom(
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(theme.highlight)),
                Span::styled(":switch field  ", Style::default().fg(theme.muted)),
                Span::styled("^D", Style::default().fg(theme.highlight)),
                Span::styled(":register  ", Style::default().fg(theme.muted)),
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
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Register a local checkout or remote URL. The daemon will build a repo context on the next command tick.",
                Style::default().fg(theme.foreground),
            )),
            Line::from(Span::styled(
                "Examples: /code/repo, https://github.com/owner/repo.git, git@github.com:owner/repo.git",
                Style::default().fg(theme.muted),
            )),
        ]),
        rows[0],
    );

    let source_focused = modal.cursor_field == 0;
    let source_block = input_block(" Source / URL ", source_focused, theme);
    let source_inner = source_block.inner(rows[1]);
    frame.render_widget(source_block, rows[1]);
    frame.render_widget(
        Paragraph::new(if modal.source.is_empty() && !source_focused {
            Line::from(Span::styled(
                "Path to an existing checkout, or a remote URL",
                Style::default().fg(theme.muted),
            ))
        } else {
            Line::from(Span::styled(
                modal.source.clone(),
                Style::default().fg(theme.foreground),
            ))
        }),
        source_inner,
    );

    let branch_focused = modal.cursor_field == 1;
    let branch_block = input_block(" Default branch ", branch_focused, theme);
    let branch_inner = branch_block.inner(rows[3]);
    frame.render_widget(branch_block, rows[3]);
    frame.render_widget(
        Paragraph::new(if modal.branch.is_empty() && !branch_focused {
            Line::from(Span::styled("main", Style::default().fg(theme.muted)))
        } else {
            Line::from(Span::styled(
                modal.branch.clone(),
                Style::default().fg(theme.foreground),
            ))
        }),
        branch_inner,
    );

    let (cursor_area, text, pos) = if source_focused {
        (source_inner, &modal.source, modal.cursor_pos)
    } else {
        (branch_inner, &modal.branch, modal.cursor_pos)
    };
    let (line, col) = super::popups::visual_cursor_position(text, pos, cursor_area.width as usize);
    let cursor_x = cursor_area.x + (col as u16).min(cursor_area.width.saturating_sub(1));
    let cursor_y = cursor_area.y + (line as u16).min(cursor_area.height.saturating_sub(1));
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn input_block(title: &'static str, focused: bool, theme: crate::theme::Theme) -> Block<'static> {
    Block::default()
        .title(Line::from(Span::styled(
            title,
            Style::default().fg(if focused {
                theme.info
            } else {
                theme.muted
            }),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            theme.highlight
        } else {
            theme.border
        }))
        .style(Style::default().bg(theme.panel_alt))
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
