use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table,
    },
};

use crate::app::AppState;

pub fn draw_repos_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let repos = &snapshot.repo_registrations;

    let header = Row::new(vec![
        Cell::from(Span::styled("", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Repository", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Tracker", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Branch", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Path", Style::default().fg(theme.muted))),
        Cell::from(
            Line::from(Span::styled("Added", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = repos
        .iter()
        .map(|repo| {
            let tracker_icon = match repo.tracker_kind {
                polyphony_core::TrackerKind::Github => "⊙",
                polyphony_core::TrackerKind::Gitlab => "◈",
                polyphony_core::TrackerKind::Linear => "◉",
                polyphony_core::TrackerKind::Beads => "●",
                polyphony_core::TrackerKind::None | polyphony_core::TrackerKind::Mock => "○",
            };
            let tracker_color = match repo.tracker_kind {
                polyphony_core::TrackerKind::Github => theme.foreground,
                polyphony_core::TrackerKind::Gitlab => theme.warning,
                polyphony_core::TrackerKind::Linear => theme.info,
                polyphony_core::TrackerKind::Beads => theme.highlight,
                polyphony_core::TrackerKind::None | polyphony_core::TrackerKind::Mock => {
                    theme.muted
                },
            };

            let label = &repo.label;
            let path_display = repo.worktree_path.display().to_string();
            let added = super::format_listing_time(repo.added_at);

            Row::new(vec![
                Cell::from(Span::styled(
                    tracker_icon.to_string(),
                    Style::default().fg(tracker_color),
                )),
                Cell::from(Span::styled(
                    label.clone(),
                    Style::default().fg(theme.foreground),
                )),
                Cell::from(Span::styled(
                    format!("{:?}", repo.tracker_kind),
                    Style::default().fg(theme.muted),
                )),
                Cell::from(Span::styled(
                    repo.default_branch.clone(),
                    Style::default().fg(theme.muted),
                )),
                Cell::from(Span::styled(path_display, Style::default().fg(theme.muted))),
                Cell::from(
                    Line::from(Span::styled(added, Style::default().fg(theme.muted)))
                        .alignment(Alignment::Right),
                ),
            ])
        })
        .collect();

    let selected_style = Style::default().add_modifier(Modifier::BOLD);

    let count = repos.len();
    let footer_info = if count == 0 {
        "no repositories".into()
    } else {
        format!(
            "{} of {count}",
            app.repos_state.selected().unwrap_or_default() + 1
        )
    };

    let table = Table::new(rows, [
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Fill(1),
        Constraint::Length(16),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::WhenSelected)
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Repos ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(vec![
                    Span::styled(footer_info, Style::default().fg(theme.muted)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("Enter:view", Style::default().fg(theme.highlight)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("n:add", Style::default().fg(theme.highlight)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("d:remove", Style::default().fg(theme.warning)),
                    Span::styled(" • ", Style::default().fg(theme.border)),
                    Span::styled("w:shell", Style::default().fg(theme.info)),
                ])
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if app.list_border_focused {
                theme.highlight
            } else {
                theme.border
            }))
            .style(Style::default().bg(theme.panel)),
    );

    frame.render_stateful_widget(table, area, &mut app.repos_state);

    if count > 0 {
        let content_height = area.height.saturating_sub(3) as usize;
        if count > content_height {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.repos_state.selected().unwrap_or(0))
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
