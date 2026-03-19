use std::collections::{HashMap, HashSet};

use polyphony_core::{IssueApprovalState, RuntimeSnapshot, VisibleTriggerKind};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, HighlightSpacing, Padding, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table,
    },
};

use crate::app::AppState;

const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const APPROVED_ICON: &str = "✓";
const WAITING_ICON: &str = "◷";

pub fn draw_triggers_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let indices = &app.sorted_issue_indices;

    // Build a map of trigger_id → task_count from movements
    let mut task_counts: HashMap<&str, usize> = HashMap::new();
    for movement in &snapshot.movements {
        if let Some(ref identifier) = movement.issue_identifier {
            // Match against trigger_id
            *task_counts.entry(identifier.as_str()).or_default() += movement.task_count;
        }
    }

    // Build set of currently running trigger IDs
    let running_ids: HashSet<&str> = snapshot
        .running
        .iter()
        .map(|r| r.issue_id.as_str())
        .collect();

    let trigger_data: Vec<_> = indices
        .iter()
        .enumerate()
        .scan(
            None::<String>,
            |current_parent_identifier, (display_index, &index)| {
                let trigger = snapshot.visible_triggers.get(index)?;
                let depth = app.tree_depth.get(display_index).copied().unwrap_or(0);
                let is_last = app
                    .tree_last_child
                    .get(display_index)
                    .copied()
                    .unwrap_or(false);
                if depth == 0 {
                    current_parent_identifier.replace(trigger.identifier.clone());
                }
                Some((trigger, depth, is_last))
            },
        )
        .collect();
    let compact = area.width < 80;
    // Hide the Kind column when all triggers are the same kind (e.g. all issues).
    let all_same_kind = trigger_data
        .first()
        .map(|(first, ..)| trigger_data.iter().all(|(t, ..)| t.kind == first.kind))
        .unwrap_or(true);
    let kind_col_width: u16 = if all_same_kind {
        0
    } else if compact {
        2
    } else {
        trigger_data
            .iter()
            .map(|(trigger, ..)| trigger.kind.to_string().len())
            .max()
            .unwrap_or(4)
            .max(4) as u16
            + 1
    };
    let status_col_width: u16 = 3; // emoji + space
    let time_col_width: u16 = if compact {
        5
    } else {
        16
    }; // "3d" vs "YYYY-MM-DD HH:MM"

    let mut header_cells = vec![
        Cell::from(Span::styled("", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("", Style::default().fg(theme.muted))),
        Cell::from(Span::styled("Title", Style::default().fg(theme.muted))),
    ];
    if !all_same_kind {
        header_cells.push(Cell::from(Span::styled("", Style::default().fg(theme.muted))));
    }
    header_cells.push(Cell::from(
        Line::from(Span::styled("", Style::default().fg(theme.muted)))
            .alignment(Alignment::Right),
    ));
    if !compact {
        header_cells.push(Cell::from(
            Line::from(Span::styled("Tasks", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        ));
    }

    let header = Row::new(header_cells)
        .height(1)
        .style(Style::default().add_modifier(Modifier::BOLD));

    let workspace_col_width: u16 = 2; // "● " or empty
    let tasks_col_width: u16 = if compact {
        0
    } else {
        6
    }; // "Tasks" + space
    let title_max_width = (area.width as usize).saturating_sub(
        2 + 1
            + 2
            + workspace_col_width as usize
            + time_col_width as usize
            + kind_col_width as usize
            + status_col_width as usize
            + tasks_col_width as usize
            + 4,
    );

    let rows: Vec<Row> = trigger_data
        .iter()
        .map(|(trigger, depth, is_last)| {
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

            // Approval marker takes 1 char when present
            let approval = approval_marker(trigger, theme);
            let approval_width = if approval.is_some() {
                1
            } else {
                0
            };
            let effective_title_width = title_max_width
                .saturating_sub(tree_prefix_width)
                .saturating_sub(approval_width);
            let display_title = truncate_with_ellipsis(&trigger.title, effective_title_width);

            let mut title_spans: Vec<Span<'_>> = Vec::new();
            if tree_prefix_width > 0 {
                title_spans.push(Span::styled(tree_prefix, Style::default().fg(theme.muted)));
            }
            if let Some((icon, color)) = approval {
                title_spans.push(Span::styled(icon, Style::default().fg(color)));
            }
            title_spans.push(Span::styled(
                display_title,
                Style::default().fg(theme.foreground),
            ));

            let time_label = if compact {
                trigger
                    .created_at
                    .map(|dt| super::detail_common::format_relative_time(dt, chrono::Utc::now()))
                    .unwrap_or_else(|| "—".into())
            } else {
                trigger
                    .created_at
                    .map(super::format_listing_time)
                    .unwrap_or_else(|| "—".into())
            };

            // Workspace indicator: spinner if running, dot if workspace, empty otherwise
            let is_running = running_ids.contains(trigger.trigger_id.as_str());
            let workspace_indicator = if is_running {
                let spinner =
                    BRAILLE_SPINNER[(app.frame_count / 4) as usize % BRAILLE_SPINNER.len()];
                Span::styled(spinner.to_string(), Style::default().fg(theme.highlight))
            } else if trigger.has_workspace {
                Span::styled("●", Style::default().fg(theme.highlight))
            } else {
                Span::styled(" ", Style::default())
            };

            let (status_icon, status_color) = status_emoji(&trigger.status, theme);

            let mut cells = vec![
                Cell::from(workspace_indicator),
                Cell::from(Span::styled(time_label, Style::default().fg(theme.muted))),
                Cell::from(Line::from(title_spans)),
            ];
            if !all_same_kind {
                let kind_color = kind_color(trigger.kind, theme);
                let kind_label = if compact {
                    kind_emoji(trigger.kind).to_string()
                } else {
                    trigger.kind.to_string()
                };
                cells.push(Cell::from(
                    Line::from(Span::styled(kind_label, Style::default().fg(kind_color)))
                        .alignment(Alignment::Right),
                ));
            }
            cells.push(Cell::from(
                Line::from(Span::styled(status_icon, Style::default().fg(status_color)))
                    .alignment(Alignment::Right),
            ));
            if !compact {
                cells.push(Cell::from({
                    let count = task_counts
                        .get(trigger.identifier.as_str())
                        .copied()
                        .unwrap_or(0);
                    let label = if count > 0 {
                        count.to_string()
                    } else {
                        "—".into()
                    };
                    let color = if count > 0 {
                        theme.foreground
                    } else {
                        theme.muted
                    };
                    Line::from(Span::styled(label, Style::default().fg(color)))
                        .alignment(Alignment::Right)
                }));
            }

            Row::new(cells)
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

    let mut col_constraints = vec![
        Constraint::Length(workspace_col_width),
        Constraint::Length(time_col_width),
        Constraint::Fill(1),
    ];
    if !all_same_kind {
        col_constraints.push(Constraint::Length(kind_col_width));
    }
    col_constraints.push(Constraint::Length(status_col_width));
    if !compact {
        col_constraints.push(Constraint::Length(tasks_col_width));
    }

    let table = Table::new(rows, col_constraints)
        .header(header)
        .row_highlight_style(selected_style)
        .highlight_spacing(HighlightSpacing::Always)
        .block({
            let mut block = Block::default().title(Line::from(title_spans));
            if app.refresh_requested || snapshot.loading.fetching_issues {
                let spinner =
                    BRAILLE_SPINNER[(app.frame_count / 4) as usize % BRAILLE_SPINNER.len()];
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
                        Span::styled(" ●", Style::default().fg(theme.highlight)),
                        Span::styled("wksp ", Style::default().fg(theme.muted)),
                        Span::styled("✓", Style::default().fg(theme.success)),
                        Span::styled("ok ", Style::default().fg(theme.muted)),
                        Span::styled("◷", Style::default().fg(theme.warning)),
                        Span::styled("wait ", Style::default().fg(theme.muted)),
                        Span::styled("○", Style::default().fg(theme.success)),
                        Span::styled("done ", Style::default().fg(theme.muted)),
                    ]),
                )
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
                .border_style(Style::default().fg(if app.list_border_focused {
                    theme.highlight
                } else {
                    theme.border
                }))
                .padding(Padding::right(1))
                .style(Style::default().bg(theme.panel))
        });

    frame.render_stateful_widget(table, area, &mut app.issues_state);
    draw_scrollbar(frame, area, count, app.issues_state.selected().unwrap_or(0));
}

fn approval_marker(
    trigger: &polyphony_core::VisibleTriggerRow,
    theme: crate::theme::Theme,
) -> Option<(&'static str, ratatui::style::Color)> {
    if trigger.kind != VisibleTriggerKind::Issue
        || !matches!(trigger.source.as_str(), "github" | "gitlab")
    {
        return None;
    }
    Some(match trigger.approval_state {
        IssueApprovalState::Approved => (APPROVED_ICON, theme.success),
        IssueApprovalState::Waiting => (WAITING_ICON, theme.warning),
    })
}

fn kind_emoji(kind: VisibleTriggerKind) -> &'static str {
    match kind {
        VisibleTriggerKind::Issue => "◆",
        VisibleTriggerKind::PullRequestReview => "⟐",
        VisibleTriggerKind::PullRequestComment => "◇",
        VisibleTriggerKind::PullRequestConflict => "⊘",
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

fn status_emoji(state: &str, theme: crate::theme::Theme) -> (&'static str, ratatui::style::Color) {
    match state.to_ascii_lowercase().as_str() {
        "open" | "in progress" | "started" | "in_progress" | "ready" => ("●", theme.success),
        "todo" | "unstarted" | "backlog" => ("○", theme.info),
        "debouncing" | "waiting_label" => ("◷", theme.info),
        "closed" | "done" | "completed" | "reviewed" | "already_fixed" => ("✓", theme.muted),
        "cancelled" | "canceled" => ("⊘", theme.muted),
        "retrying" => ("↻", theme.warning),
        "draft" => ("◌", theme.warning),
        "ignored_author" | "ignored_bot" | "ignored_label" => ("⊘", theme.muted),
        _ => ("·", theme.foreground),
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
