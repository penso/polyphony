use std::collections::{HashMap, HashSet};

use polyphony_core::{DispatchApprovalState, InboxItemKind, RuntimeSnapshot};
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

pub fn draw_inbox_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let indices = &app.sorted_issue_indices;

    // Build a map of inbox identifiers -> task counts from runs.
    let mut task_counts: HashMap<&str, usize> = HashMap::new();
    for run in &snapshot.runs {
        if let Some(ref identifier) = run.issue_identifier {
            *task_counts.entry(identifier.as_str()).or_default() += run.task_count;
        }
    }

    // Build set of currently running inbox item IDs.
    let running_ids: HashSet<&str> = snapshot
        .running
        .iter()
        .map(|r| r.issue_id.as_str())
        .collect();

    let item_data: Vec<_> = indices
        .iter()
        .enumerate()
        .scan(
            None::<String>,
            |current_parent_identifier, (display_index, &index)| {
                let item = snapshot.inbox_items.get(index)?;
                let depth = app.tree_depth.get(display_index).copied().unwrap_or(0);
                let is_last = app
                    .tree_last_child
                    .get(display_index)
                    .copied()
                    .unwrap_or(false);
                if depth == 0 {
                    current_parent_identifier.replace(item.identifier.clone());
                }
                Some((item, depth, is_last))
            },
        )
        .collect();
    let compact = area.width < 80;
    let mixed_sources = item_data
        .first()
        .map(|(first, ..)| {
            item_data
                .iter()
                .any(|(item, ..)| item.source != first.source)
        })
        .unwrap_or(false);
    let multi_repo = item_data
        .first()
        .map(|(first, ..)| {
            item_data
                .iter()
                .any(|(item, ..)| item.repo_id != first.repo_id)
        })
        .unwrap_or(false);
    let repo_col_width: u16 = if multi_repo {
        item_data
            .iter()
            .map(|(item, ..)| item.repo_id.len())
            .max()
            .unwrap_or(4)
            .min(20) as u16
            + 1
    } else {
        0
    };
    // Hide the Kind column when all items are the same kind (e.g. all issues).
    let all_same_kind = item_data
        .first()
        .map(|(first, ..)| item_data.iter().all(|(t, ..)| t.kind == first.kind))
        .unwrap_or(true);
    let source_col_width: u16 = if mixed_sources {
        2
    } else {
        0
    };
    let kind_col_width: u16 = if all_same_kind {
        0
    } else if compact {
        2
    } else {
        item_data
            .iter()
            .map(|(item, ..)| item.kind.to_string().len())
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

    let any_activity_indicator = item_data.iter().any(|(item, ..)| {
        item.has_workspace
            || running_ids.contains(item.item_id.as_str())
            || app.dispatching_inbox_items.contains(&item.item_id)
    });
    let workspace_col_width: u16 = if any_activity_indicator {
        1
    } else {
        0
    };

    let mut header_cells = Vec::new();
    if workspace_col_width > 0 {
        header_cells.push(Cell::from(Span::styled(
            "",
            Style::default().fg(theme.muted),
        )));
    }
    header_cells.push(Cell::from(Span::styled(
        "",
        Style::default().fg(theme.muted),
    )));
    if mixed_sources {
        header_cells.push(Cell::from(Span::styled(
            "",
            Style::default().fg(theme.muted),
        )));
    }
    if multi_repo {
        header_cells.push(Cell::from(Span::styled(
            "Repo",
            Style::default().fg(theme.muted),
        )));
    }
    header_cells.push(Cell::from(Span::styled(
        "Title",
        Style::default().fg(theme.muted),
    )));
    if !all_same_kind {
        header_cells.push(Cell::from(Span::styled(
            "",
            Style::default().fg(theme.muted),
        )));
    }
    header_cells.push(Cell::from(
        Line::from(Span::styled("", Style::default().fg(theme.muted))).alignment(Alignment::Right),
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

    let tasks_col_width: u16 = if compact {
        0
    } else {
        6
    }; // "Tasks" + space
    let title_max_width = (area.width as usize).saturating_sub(
        2 // borders
            + workspace_col_width as usize
            + time_col_width as usize
            + source_col_width as usize
            + repo_col_width as usize
            + kind_col_width as usize
            + status_col_width as usize
            + tasks_col_width as usize
            + 1, // padding
    );

    let rows: Vec<Row> = item_data
        .iter()
        .map(|(item, depth, is_last)| {
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
            let approval = approval_marker(item, theme);
            let approval_width = if approval.is_some() {
                2
            } else {
                0
            };
            let effective_title_width = title_max_width
                .saturating_sub(tree_prefix_width)
                .saturating_sub(approval_width);
            let display_title = truncate_with_ellipsis(&item.title, effective_title_width);

            let mut title_spans: Vec<Span<'_>> = Vec::new();
            if tree_prefix_width > 0 {
                title_spans.push(Span::styled(tree_prefix, Style::default().fg(theme.muted)));
            }
            if let Some((icon, color)) = approval {
                title_spans.push(Span::styled(format!("{icon} "), Style::default().fg(color)));
            }
            title_spans.push(Span::styled(
                display_title,
                Style::default().fg(theme.foreground),
            ));

            let time_label = if compact && app.is_split_eligible() {
                item.created_at
                    .map(super::format_short_time)
                    .unwrap_or_else(|| "—".into())
            } else if compact {
                item.created_at
                    .map(|dt| super::detail_common::format_relative_time(dt, chrono::Utc::now()))
                    .unwrap_or_else(|| "—".into())
            } else {
                item.created_at
                    .map(super::format_listing_time)
                    .unwrap_or_else(|| "—".into())
            };

            // Workspace indicator: spinner if running or dispatching, dot if workspace, empty otherwise
            let is_running = running_ids.contains(item.item_id.as_str());
            let is_dispatching = app.dispatching_inbox_items.contains(&item.item_id);
            let workspace_indicator = if is_running || is_dispatching {
                let spinner =
                    BRAILLE_SPINNER[(app.frame_count / 4) as usize % BRAILLE_SPINNER.len()];
                Span::styled(spinner.to_string(), Style::default().fg(theme.info))
            } else if item.has_workspace {
                Span::styled("●", Style::default().fg(theme.highlight))
            } else {
                Span::styled(" ", Style::default())
            };

            let (status_icon, status_color) = status_emoji(&item.status, theme);

            let source_label = source_badge(&item.source).to_string();

            let mut cells = Vec::new();
            if workspace_col_width > 0 {
                cells.push(Cell::from(workspace_indicator));
            }
            cells.push(Cell::from(Span::styled(
                time_label,
                Style::default().fg(theme.muted),
            )));
            if mixed_sources {
                cells.push(Cell::from(
                    Line::from(Span::styled(source_label, Style::default().fg(theme.muted)))
                        .alignment(Alignment::Right),
                ));
            }
            if multi_repo {
                let repo_label = truncate_with_ellipsis(
                    &item.repo_id,
                    repo_col_width.saturating_sub(1) as usize,
                );
                cells.push(Cell::from(Span::styled(
                    repo_label,
                    Style::default().fg(theme.info),
                )));
            }
            cells.push(Cell::from(Line::from(title_spans)));
            if !all_same_kind {
                let kind_color = kind_color(item.kind, theme);
                let kind_label = if compact {
                    kind_emoji(item.kind).to_string()
                } else {
                    item.kind.to_string()
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
                        .get(item.identifier.as_str())
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

    let selected_style = Style::default().add_modifier(Modifier::BOLD);

    let count = indices.len();
    let footer_info = selection_info(app.issues_state.selected(), count);
    let sort_label = app.issue_sort.label();
    let running_count = item_data
        .iter()
        .filter(|(item, ..)| running_ids.contains(item.item_id.as_str()))
        .count();

    let title_spans = if app.search_active {
        vec![
            Span::styled(
                " Inbox ",
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
                " Inbox ",
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
            " Inbox ",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )]
    };

    let mut col_constraints = vec![Constraint::Length(time_col_width)];
    if workspace_col_width > 0 {
        col_constraints.insert(0, Constraint::Length(workspace_col_width));
    }
    if mixed_sources {
        col_constraints.push(Constraint::Length(source_col_width));
    }
    if multi_repo {
        col_constraints.push(Constraint::Length(repo_col_width));
    }
    col_constraints.push(Constraint::Fill(1));
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
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::WhenSelected)
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
                        Span::styled(footer_info, Style::default().fg(theme.muted)),
                        Span::styled(" • ", Style::default().fg(theme.border)),
                        Span::styled("n:", Style::default().fg(theme.muted)),
                        Span::styled("new", Style::default().fg(theme.highlight)),
                        Span::styled(" • ", Style::default().fg(theme.border)),
                        Span::styled("sorted by ", Style::default().fg(theme.muted)),
                        Span::styled(sort_label, Style::default().fg(theme.highlight)),
                        if running_count > 0 {
                            Span::styled(" • ", Style::default().fg(theme.border))
                        } else {
                            Span::raw("")
                        },
                        if running_count > 0 {
                            Span::styled(
                                format!("{running_count} running"),
                                Style::default().fg(theme.warning),
                            )
                        } else {
                            Span::raw("")
                        },
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

fn source_badge(source: &str) -> &'static str {
    match source {
        "github" => "",
        "gitlab" => "gl",
        "beads" => "bd",
        "linear" => "ln",
        "mock" => "mk",
        _ => "??",
    }
}

fn approval_marker(
    item: &polyphony_core::InboxItemRow,
    theme: crate::theme::Theme,
) -> Option<(&'static str, ratatui::style::Color)> {
    Some(match item.approval_state {
        DispatchApprovalState::Approved => (APPROVED_ICON, theme.success),
        DispatchApprovalState::Waiting => (WAITING_ICON, theme.warning),
    })
}

fn kind_emoji(kind: InboxItemKind) -> &'static str {
    match kind {
        InboxItemKind::Issue => "◆",
        InboxItemKind::PullRequestReview => "⟐",
        InboxItemKind::PullRequestComment => "◇",
        InboxItemKind::PullRequestConflict => "⊘",
    }
}

fn kind_color(kind: InboxItemKind, theme: crate::theme::Theme) -> ratatui::style::Color {
    match kind {
        InboxItemKind::Issue => theme.success,
        InboxItemKind::PullRequestReview => theme.highlight,
        InboxItemKind::PullRequestComment => theme.warning,
        InboxItemKind::PullRequestConflict => theme.warning,
    }
}

pub(crate) fn status_emoji_pub(
    state: &str,
    theme: crate::theme::Theme,
) -> (&'static str, ratatui::style::Color) {
    status_emoji(state, theme)
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
