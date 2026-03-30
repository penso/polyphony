use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use crate::{app::AppState, theme::Theme};

pub(crate) fn draw_repo_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    repo_id: &str,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let Some(repo) = snapshot
        .repo_registrations
        .iter()
        .find(|repo| repo.repo_id == repo_id)
    else {
        draw_not_found(frame, area, "Repository is no longer registered", theme);
        return;
    };

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled("Repo ", Style::default().fg(theme.info)),
            Span::styled(
                format!("{} ", repo.repo_id),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .title_bottom(
            Line::from(vec![
                Span::styled("w", Style::default().fg(theme.highlight)),
                Span::styled(":shell  ", Style::default().fg(theme.muted)),
                Span::styled("d", Style::default().fg(theme.warning)),
                Span::styled(":remove  ", Style::default().fg(theme.muted)),
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
    let body = vec![
        super::detail_common::kv_line("Label", &repo.label, theme),
        super::detail_common::kv_line("Tracker", &format!("{:?}", repo.tracker_kind), theme),
        super::detail_common::kv_line("Branch", &repo.default_branch, theme),
        super::detail_common::kv_line("Added", &super::format_detail_time(repo.added_at), theme),
        super::detail_common::kv_line("Path", &repo.worktree_path.display().to_string(), theme),
        super::detail_common::kv_line(
            "Clone URL",
            repo.clone_url.as_deref().unwrap_or("local-only"),
            theme,
        ),
    ];

    let content = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(body.len() as u16 + 2),
            Constraint::Min(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(Line::from(Span::styled(
                    " Registration ",
                    Style::default().fg(theme.info),
                )))
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.panel)),
        ),
        content[0],
    );

    let mut notes = vec![Line::from(vec![
        Span::styled("Dispatch scope ", Style::default().fg(theme.muted)),
        Span::styled(
            "This repo participates in polling, routing, handoff, and snapshot history.",
            Style::default().fg(theme.foreground),
        ),
    ])];
    if repo.clone_url.is_some() {
        notes.push(Line::from(vec![
            Span::styled("Clone mode ", Style::default().fg(theme.muted)),
            Span::styled(
                "The daemon will create or reuse the managed worktree under the stored path.",
                Style::default().fg(theme.foreground),
            ),
        ]));
    } else {
        notes.push(Line::from(vec![
            Span::styled("Local mode ", Style::default().fg(theme.muted)),
            Span::styled(
                "This registration points at an existing checkout and does not require cloning.",
                Style::default().fg(theme.foreground),
            ),
        ]));
    }

    let body_text_height = notes.len();
    let scroll_pos = app.current_detail_scroll();
    frame.render_widget(
        Paragraph::new(notes)
            .wrap(Wrap { trim: false })
            .scroll((scroll_pos, 0))
            .block(
                Block::default()
                    .title(Line::from(Span::styled(
                        " Notes ",
                        Style::default().fg(theme.info),
                    )))
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.panel)),
            ),
        content[1],
    );

    let viewport_height = content[1].height.saturating_sub(2) as usize;
    if body_text_height > viewport_height {
        let mut scrollbar_state =
            ScrollbarState::new(body_text_height.saturating_sub(viewport_height))
                .position(scroll_pos as usize)
                .viewport_content_length(viewport_height);
        frame.render_stateful_widget(
            Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight),
            content[1],
            &mut scrollbar_state,
        );
    }
}

fn draw_not_found(frame: &mut ratatui::Frame<'_>, area: Rect, message: &str, theme: Theme) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(theme.muted),
        )))
        .block(
            Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.panel_alt)),
        ),
        area,
    );
}
