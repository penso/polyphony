use {
    chrono::{DateTime, Utc},
    polyphony_core::{RuntimeSnapshot, VisibleTriggerKind},
    ratatui::{
        layout::{Alignment, Constraint, Rect},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{
            Block, BorderType, Cell, HighlightSpacing, Padding, Row, Scrollbar,
            ScrollbarOrientation, ScrollbarState, Table,
        },
    },
};

use crate::app::AppState;

const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw_triggers_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let indices = &app.sorted_issue_indices;
    let now = Utc::now();

    let trigger_data: Vec<_> = indices
        .iter()
        .enumerate()
        .filter_map(|(display_index, &index)| {
            snapshot.visible_triggers.get(index).map(|trigger| {
                let age = trigger
                    .created_at
                    .map(|created| format_relative_time(created, now));
                let depth = app.tree_depth.get(display_index).copied().unwrap_or(0);
                let is_last = app
                    .tree_last_child
                    .get(display_index)
                    .copied()
                    .unwrap_or(false);
                (trigger, age, depth, is_last)
            })
        })
        .collect();

    let max_id_len = trigger_data
        .iter()
        .map(|(trigger, ..)| trigger.identifier.len())
        .max()
        .unwrap_or(2)
        .max(2) as u16
        + 1;
    let max_source_len = trigger_data
        .iter()
        .map(|(trigger, ..)| trigger.source.len())
        .max()
        .unwrap_or(6)
        .max(6) as u16
        + 1;
    let max_kind_len = trigger_data
        .iter()
        .map(|(trigger, ..)| trigger.kind.to_string().len())
        .max()
        .unwrap_or(4)
        .max(4) as u16
        + 1;
    let any_has_workspace = trigger_data
        .iter()
        .any(|(trigger, ..)| trigger.has_workspace);
    let workspace_indicator_width: u16 = if any_has_workspace {
        2
    } else {
        0
    };
    let max_status_len = trigger_data
        .iter()
        .map(|(trigger, ..)| trigger.status.len())
        .max()
        .unwrap_or(6)
        .max(6) as u16
        + 1
        + workspace_indicator_width;
    let max_age_len = trigger_data
        .iter()
        .filter_map(|(_, age, ..)| age.as_ref().map(|value| value.len()))
        .max()
        .unwrap_or(3)
        .max(3) as u16
        + 1;

    let header = Row::new(vec![
        Cell::from(
            Line::from(Span::styled("ID", Style::default().fg(theme.muted)))
                .alignment(Alignment::Center),
        ),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
        Cell::from(
            Line::from(Span::styled("Source", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
        Cell::from(
            Line::from(Span::styled("Kind", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
        Cell::from(
            Line::from(Span::styled("Status", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ),
        Cell::from(
            Line::from(vec![
                Span::styled("Age", Style::default().fg(theme.muted)),
                Span::raw(" "),
            ])
            .alignment(Alignment::Right),
        ),
    ])
    .height(1)
    .style(Style::default().add_modifier(Modifier::BOLD));

    let title_max_width = (area.width as usize).saturating_sub(
        2 + 1
            + 2
            + max_id_len as usize
            + max_source_len as usize
            + max_kind_len as usize
            + max_status_len as usize
            + max_age_len as usize
            + 5,
    );

    let rows: Vec<Row> = trigger_data
        .iter()
        .map(|(trigger, age, depth, is_last)| {
            let state_color = state_color(&trigger.status, theme);
            let source_color = source_color(&trigger.source, theme);
            let kind_color = kind_color(trigger.kind, theme);

            let (tree_prefix, tree_prefix_width) = if *depth > 0 {
                let connector = if *is_last {
                    "└── "
                } else {
                    "├── "
                };
                (connector, 4)
            } else {
                ("", 0)
            };
            let effective_title_width = title_max_width.saturating_sub(tree_prefix_width);
            let display_title = truncate_with_ellipsis(&trigger.title, effective_title_width);

            let title_spans = if tree_prefix_width > 0 {
                vec![
                    Span::styled(tree_prefix, Style::default().fg(theme.muted)),
                    Span::styled(display_title, Style::default().fg(theme.foreground)),
                ]
            } else {
                vec![Span::styled(
                    display_title,
                    Style::default().fg(theme.foreground),
                )]
            };

            Row::new(vec![
                Cell::from(
                    Line::from(Span::styled(
                        trigger.identifier.clone(),
                        Style::default().fg(theme.info),
                    ))
                    .alignment(Alignment::Center),
                ),
                Cell::from(Line::from(title_spans)),
                Cell::from(
                    Line::from(Span::styled(
                        trigger.source.clone(),
                        Style::default().fg(source_color),
                    ))
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(Span::styled(
                        trigger.kind.to_string(),
                        Style::default().fg(kind_color),
                    ))
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(if trigger.has_workspace {
                        vec![
                            Span::styled("● ", Style::default().fg(theme.highlight)),
                            Span::styled(trigger.status.clone(), Style::default().fg(state_color)),
                            Span::raw(" "),
                        ]
                    } else {
                        vec![
                            Span::styled(trigger.status.clone(), Style::default().fg(state_color)),
                            Span::raw(" "),
                        ]
                    })
                    .alignment(Alignment::Right),
                ),
                Cell::from(
                    Line::from(vec![
                        Span::styled(
                            age.clone().unwrap_or_default(),
                            Style::default().fg(theme.muted),
                        ),
                        Span::raw(" "),
                    ])
                    .alignment(Alignment::Right),
                ),
            ])
        })
        .collect();

    let selected_style = Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD);

    let count = indices.len();
    let footer_info = selection_info(app.issues_state.selected(), count);
    let sort_label = app.issue_sort.label();

    let title_spans = if app.search_active {
        vec![
            Span::styled(
                " Triggers ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("/", Style::default().fg(theme.highlight)),
            Span::styled(&app.search_query, Style::default().fg(theme.foreground)),
            Span::styled("▏", Style::default().fg(theme.highlight)),
        ]
    } else if !app.search_query.is_empty() {
        vec![
            Span::styled(
                " Triggers ",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{}] ", app.search_query),
                Style::default().fg(theme.highlight),
            ),
        ]
    } else {
        vec![Span::styled(
            " Triggers ",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )]
    };

    let table = Table::new(rows, [
        Constraint::Length(max_id_len),
        Constraint::Fill(1),
        Constraint::Length(max_source_len),
        Constraint::Length(max_kind_len),
        Constraint::Length(max_status_len),
        Constraint::Length(max_age_len),
    ])
    .header(header)
    .row_highlight_style(selected_style)
    .highlight_spacing(HighlightSpacing::Always)
    .highlight_symbol("▸ ")
    .block({
        let mut block = Block::default().title(Line::from(title_spans));
        if app.refresh_requested || snapshot.loading.fetching_issues {
            let spinner = BRAILLE_SPINNER[(app.frame_count / 4) as usize % BRAILLE_SPINNER.len()];
            block = block.title(
                Line::from(vec![
                    Span::styled(format!(" {spinner} "), Style::default().fg(theme.highlight)),
                    Span::styled("refreshing ", Style::default().fg(theme.muted)),
                ])
                .right_aligned(),
            );
        }
        block
            .title_bottom(
                Line::from(vec![
                    Span::styled("─s:", Style::default().fg(theme.muted)),
                    Span::styled(sort_label, Style::default().fg(theme.highlight)),
                    Span::styled(format!(" {footer_info}─"), Style::default().fg(theme.muted)),
                ])
                .right_aligned(),
            )
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .padding(Padding::right(1))
            .style(Style::default().bg(theme.panel))
    });

    frame.render_stateful_widget(table, area, &mut app.issues_state);
    draw_scrollbar(frame, area, count, app.issues_state.selected().unwrap_or(0));
}

fn source_color(source: &str, theme: crate::theme::Theme) -> ratatui::style::Color {
    match source {
        "github" => theme.info,
        "linear" => theme.success,
        "beads" => theme.highlight,
        _ => theme.muted,
    }
}

fn kind_color(kind: VisibleTriggerKind, theme: crate::theme::Theme) -> ratatui::style::Color {
    match kind {
        VisibleTriggerKind::Issue => theme.success,
        VisibleTriggerKind::PullRequestReview => theme.highlight,
        VisibleTriggerKind::PullRequestComment => theme.warning,
        VisibleTriggerKind::PullRequestConflict => theme.warning,
    }
}

pub fn state_color(state: &str, theme: crate::theme::Theme) -> ratatui::style::Color {
    match state.to_ascii_lowercase().as_str() {
        "open" | "in progress" | "started" | "in_progress" | "ready" => theme.success,
        "todo" | "unstarted" | "backlog" | "debouncing" | "waiting_label" => theme.info,
        "closed" | "done" | "completed" | "cancelled" | "canceled" | "reviewed"
        | "already_fixed" => theme.muted,
        "retrying" | "draft" => theme.warning,
        "ignored_author" | "ignored_bot" | "ignored_label" => theme.muted,
        _ => theme.foreground,
    }
}

fn selection_info(selected: Option<usize>, total: usize) -> String {
    if total == 0 {
        return "empty".into();
    }
    format!("{} of {total}", selected.unwrap_or_default() + 1)
}

fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.chars().count() <= max_width {
        return s.to_string();
    }
    let end = s.floor_char_boundary(max_width.saturating_sub(1));
    format!("{}…", &s[..end])
}

fn format_relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(dt).num_seconds().max(0) as u64;
    if secs < 60 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else if secs < 604_800 {
        format!("{}d", secs / 86_400)
    } else if secs < 2_592_000 {
        format!("{}w", secs / 604_800)
    } else if secs < 31_536_000 {
        format!("{}mo", secs / 2_592_000)
    } else {
        format!("{}y", secs / 31_536_000)
    }
}

fn draw_scrollbar(frame: &mut ratatui::Frame<'_>, area: Rect, count: usize, position: usize) {
    let content_height = area.height.saturating_sub(3) as usize;
    if count > content_height {
        let mut scrollbar_state = ScrollbarState::new(count)
            .position(position)
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
