use polyphony_core::{InboxItemRow, RunStatus, RuntimeSnapshot, TaskRow};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget, Wrap,
    },
};

use crate::{
    app::{AppState, DetailInputMode},
    format::item_time_label,
    rows::display_rows_matching,
    session, theme, tracker,
    widgets::{InputBottomPanel, LeftRailPanel},
};

pub(crate) fn draw_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let Some(item) = selected_item(snapshot, app) else {
        Line::styled("No inbox item selected", Style::new().fg(theme::muted()))
            .render(area.inner(Margin::new(2, 1)), frame.buffer_mut());
        return;
    };

    let content_area = Rect::new(
        area.x + 2,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    let show_sidebar = content_area.width >= 96;
    let (main_area, sidebar_area) = if show_sidebar {
        let [main, sidebar] = Layout::horizontal([Constraint::Min(48), Constraint::Length(42)])
            .spacing(2)
            .areas(content_area);
        (main, Some(sidebar))
    } else {
        (content_area, None)
    };

    let [main_content, bottom_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(6)])
        .spacing(1)
        .areas(main_area);
    let [input_area, footer_area] =
        Layout::vertical([Constraint::Length(5), Constraint::Length(1)]).areas(bottom_area);

    draw_main(frame, main_content, snapshot, item, app);
    draw_action_bar(frame, input_area, app);
    draw_detail_footer(frame, footer_area, snapshot, item, app);
    if let Some(sidebar) = sidebar_area {
        draw_sidebar(frame, sidebar, snapshot, item, app);
    } else {
        app.workspace_path_rect = Rect::default();
        app.workspace_path_to_copy = None;
        app.sidebar_text_rect = Rect::default();
        app.sidebar_visible_lines.clear();
    }
}

fn draw_action_bar(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let hijacking = app.detail_input_mode == DetailInputMode::Hijack;
    let input = if hijacking {
        app.input.as_str()
    } else {
        ""
    };
    let (mode, submit, escape) = if hijacking {
        ("hijack", "Enter submit", "Esc cancel")
    } else {
        (
            "Enter dispatch",
            "p pause/resume",
            "s stop  r retry  x clear  h hijack",
        )
    };

    InputBottomPanel::new(input)
        .focused(hijacking)
        .cursor_visible(hijacking)
        .blink_on((app.tick / 6).is_multiple_of(2))
        .border_color(match hijacking {
            true => theme::secondary(),
            false => theme::primary(),
        })
        .content_bg(theme::element())
        .text_color(theme::text())
        .muted_color(theme::muted())
        .label_accent(match hijacking {
            true => theme::secondary(),
            false => theme::primary(),
        })
        .bottom_half_bg(theme::bg())
        .padding(Padding::new(1, 1, 1, 0))
        .labels(mode, submit, escape)
        .render(area, frame.buffer_mut());
}

fn draw_detail_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    item: &InboxItemRow,
    app: &AppState,
) {
    let state = issue_control_state(snapshot, item);
    let session_active = current_session_work_active(snapshot, item);
    let text_x = if session_active && area.width > 10 {
        frame.render_widget(&app.loader, Rect::new(area.x, area.y, 8, 1));
        area.x.saturating_add(10)
    } else {
        area.x
    };
    let text_area = Rect::new(
        text_x,
        area.y,
        area.width.saturating_sub(text_x.saturating_sub(area.x)),
        area.height,
    );
    let mut spans = vec![
        Span::styled("Ctrl+P", Style::new().fg(theme::text()).bold()),
        Span::styled(":commands ", Style::new().fg(theme::muted())),
        Span::styled("y", Style::new().fg(theme::text()).bold()),
        Span::styled(":copy session ", Style::new().fg(theme::muted())),
        Span::styled("Esc", Style::new().fg(theme::text()).bold()),
        Span::styled(":back ", Style::new().fg(theme::muted())),
        Span::styled("orchestrator:", Style::new().fg(theme::muted())),
        Span::styled(
            format!("{} ", snapshot.dispatch_mode),
            Style::new().fg(orchestrator_status_color(snapshot)),
        ),
        Span::styled("state:", Style::new().fg(theme::muted())),
        Span::styled(state, Style::new().fg(theme::primary())),
    ];
    if let Some(message) = app.status_message.as_deref() {
        spans.push(Span::styled("  ", Style::new().fg(theme::muted())));
        spans.push(Span::styled(
            message.to_string(),
            Style::new().fg(theme::secondary()),
        ));
    }
    Line::from(spans).render(text_area, frame.buffer_mut());
}

fn current_session_work_active(snapshot: &RuntimeSnapshot, item: &InboxItemRow) -> bool {
    snapshot.running.iter().any(|agent| {
        repo_matches(agent.repo_id.as_str(), item.repo_id.as_str())
            && (agent.issue_id == item.item_id || agent.issue_identifier == item.identifier)
    }) || snapshot.runs.iter().any(|run| {
        repo_matches(run.repo_id.as_str(), item.repo_id.as_str())
            && run.issue_identifier.as_deref() == Some(item.identifier.as_str())
            && matches!(
                run.status,
                RunStatus::Pending
                    | RunStatus::Planning
                    | RunStatus::InProgress
                    | RunStatus::Review
            )
    })
}

fn repo_matches(left: &str, right: &str) -> bool {
    left.is_empty() || right.is_empty() || left == right
}

fn orchestrator_status_color(snapshot: &RuntimeSnapshot) -> ratatui::style::Color {
    match snapshot.dispatch_mode {
        polyphony_core::DispatchMode::Stop => theme::error(),
        polyphony_core::DispatchMode::Idle | polyphony_core::DispatchMode::Manual => {
            theme::secondary()
        },
        polyphony_core::DispatchMode::Automatic | polyphony_core::DispatchMode::Nightshift => {
            theme::done()
        },
    }
}

fn draw_main(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    item: &InboxItemRow,
    app: &mut AppState,
) {
    app.children_expand_rect = Rect::default();

    let session = session::build_issue_session(
        snapshot,
        item,
        app.children_expanded,
        &app.interventions,
        &app.notices,
        app.tick,
    );
    let panel_styles = session
        .blocks
        .iter()
        .map(|block| match block.lines.len() {
            1 => session::SessionBlockStyle::Plain,
            _ => block.style,
        })
        .collect::<Vec<_>>();
    let panels = session
        .blocks
        .iter()
        .map(|block| {
            let mut panel = LeftRailPanel::new(block.lines.clone()).border_color(block.accent);
            if app.settings.show_widget_timestamps {
                panel = panel.timestamp_label(block.timestamp.map(widget_timestamp_label));
            }
            if let Some(max_height) = block.max_height {
                panel = panel.max_height(max_height);
            }
            panel
        })
        .collect::<Vec<_>>();

    let panel_width = area.width.saturating_sub(2);
    let panel_heights = panels
        .iter()
        .zip(panel_styles.iter())
        .map(|(panel, style)| match style {
            session::SessionBlockStyle::Plain => panel.plain_height(panel_width) as usize,
            session::SessionBlockStyle::Subtle | session::SessionBlockStyle::Full => {
                panel.visible_height(panel_width) as usize
            },
        })
        .collect::<Vec<_>>();
    let panel_offsets = panel_offsets(&panel_heights);
    let rendered_height = total_panel_height(&panel_heights);
    let max_scroll = rendered_height.saturating_sub(area.height as usize) as u16;
    app.detail_scroll_max = max_scroll;
    app.detail_scrollbar_rect = Rect::default();
    if app.detail_follow_bottom {
        app.detail_scroll = bottom_aligned_scroll(&panel_offsets, &panel_heights, area.height)
            .min(max_scroll as usize) as u16;
    } else {
        app.detail_scroll = app.detail_scroll.min(max_scroll);
    }

    let rendered_areas = render_panels(
        frame,
        area,
        &panels,
        &panel_styles,
        &panel_heights,
        &panel_offsets,
        app,
        session.expand_block_index.filter(|_| session.expandable),
    );
    if let Some(children_panel_index) = session.expand_block_index.filter(|_| session.expandable) {
        app.children_expand_rect = rendered_areas
            .get(children_panel_index)
            .copied()
            .flatten()
            .unwrap_or_default();
    }

    if max_scroll > 0 {
        app.detail_scrollbar_rect = Rect::new(
            area.x + area.width.saturating_sub(2),
            area.y,
            2,
            area.height,
        );
        render_scrollbar(
            frame,
            area,
            rendered_height,
            area.height as usize,
            app.detail_scroll as usize,
        );
    }
}

fn widget_timestamp_label(timestamp: chrono::DateTime<chrono::Utc>) -> String {
    timestamp
        .with_timezone(&chrono::Local)
        .format("%H:%M:%S")
        .to_string()
}

fn render_panels(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    panels: &[LeftRailPanel],
    panel_styles: &[session::SessionBlockStyle],
    panel_heights: &[usize],
    panel_offsets: &[usize],
    app: &mut AppState,
    hover_panel: Option<usize>,
) -> Vec<Option<Rect>> {
    let scroll = app.detail_scroll;
    let mut rendered_areas = vec![None; panels.len()];
    let panel_width = area.width.saturating_sub(2);
    let content_height = total_panel_height(panel_heights).min(u16::MAX as usize) as u16;
    let mut scratch = Buffer::empty(Rect::new(0, 0, panel_width, content_height));
    Block::default()
        .style(Style::new().bg(theme::bg()))
        .render(scratch.area, &mut scratch);

    for (idx, panel) in panels.iter().enumerate() {
        let height = panel_heights
            .get(idx)
            .copied()
            .unwrap_or_default()
            .min(u16::MAX as usize) as u16;
        let content_y = panel_offsets
            .get(idx)
            .copied()
            .unwrap_or_default()
            .min(u16::MAX as usize) as u16;
        let bg = match hover_panel == Some(idx) {
            true => theme::element_hover(),
            false => theme::element(),
        };
        match panel_styles
            .get(idx)
            .copied()
            .unwrap_or(session::SessionBlockStyle::Full)
        {
            session::SessionBlockStyle::Plain => {
                panel.render_plain(
                    Rect::new(0, content_y, panel_width, height),
                    theme::bg(),
                    &mut scratch,
                );
            },
            session::SessionBlockStyle::Subtle => {
                panel.render_plain_with_rail(
                    Rect::new(0, content_y, panel_width, height),
                    theme::bg(),
                    &mut scratch,
                );
            },
            session::SessionBlockStyle::Full => {
                panel.render_clipped_with_bg(
                    Rect::new(0, content_y, panel_width, height),
                    0,
                    bg,
                    &mut scratch,
                );
            },
        }
    }

    app.session_text_rect = Rect::new(area.x, area.y, panel_width, area.height);
    app.session_visible_lines.clear();
    let selection = selection_bounds(app, app.session_text_rect);
    let multi_line_selection = selection
        .map(|((start_row, _), (end_row, _))| start_row != end_row)
        .unwrap_or(false);

    let scroll = usize::from(scroll);
    for y in 0..area.height {
        let source_y = scroll.saturating_add(usize::from(y));
        if source_y >= usize::from(content_height) {
            break;
        }
        let mut visible_line = String::new();
        for x in 0..panel_width {
            let Some(source) = scratch.cell((x, source_y as u16)) else {
                continue;
            };
            visible_line.push_str(source.symbol());
        }
        let highlight_left = selection_highlight_left(&visible_line, multi_line_selection);
        for x in 0..panel_width {
            let Some(source) = scratch.cell((x, source_y as u16)) else {
                continue;
            };
            if let Some(dest) = frame.buffer_mut().cell_mut((area.x + x, area.y + y)) {
                *dest = source.clone();
                if position_selected(selection, y, x, highlight_left) {
                    dest.set_style(
                        dest.style()
                            .fg(theme::bg())
                            .bg(theme::primary())
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
        }
        app.session_visible_lines
            .push(visible_line.trim_end().to_string());
    }

    for (idx, height) in panel_heights.iter().copied().enumerate() {
        let content_y = panel_offsets.get(idx).copied().unwrap_or_default();
        let top = content_y as i32 - scroll as i32;

        let bottom = top + height as i32;
        if bottom > 0 && top < i32::from(area.height) {
            let visible_y = area.y + top.max(0) as u16;
            let visible_height = height.min((area.y + area.height - visible_y) as usize) as u16;
            let panel_area = Rect::new(area.x, visible_y, panel_width, visible_height);
            rendered_areas[idx] = Some(panel_area);
        }
    }

    rendered_areas
}

fn selection_bounds(app: &AppState, area: Rect) -> Option<((u16, u16), (u16, u16))> {
    let mut start = app.session_selection_start?;
    let mut end = app.session_selection_end?;
    if area.is_empty() {
        return None;
    }
    if position_after(start, end) {
        std::mem::swap(&mut start, &mut end);
    }
    let scroll = app.detail_scroll;
    let viewport_bottom = scroll.saturating_add(area.height.saturating_sub(1));
    if end.1 < scroll || start.1 > viewport_bottom {
        return None;
    }
    let start_row = start.1.saturating_sub(scroll);
    let end_row = end
        .1
        .saturating_sub(scroll)
        .min(area.height.saturating_sub(1));
    let start_col = if start.1 < scroll {
        0
    } else {
        start.0
    };
    let end_col = if end.1 > viewport_bottom {
        u16::MAX
    } else {
        end.0
    };
    Some(((start_row, start_col), (end_row, end_col)))
}

fn position_selected(
    selection: Option<((u16, u16), (u16, u16))>,
    row: u16,
    col: u16,
    highlight_left: u16,
) -> bool {
    let Some(((start_row, start_col), (end_row, end_col))) = selection else {
        return false;
    };
    if row < start_row || row > end_row {
        return false;
    }
    let left = if row == start_row {
        start_col
    } else {
        0
    }
    .max(highlight_left);
    let right = if row == end_row {
        end_col
    } else {
        u16::MAX
    };
    col >= left && col <= right
}

fn selection_highlight_left(line: &str, multi_line_selection: bool) -> u16 {
    if !multi_line_selection {
        return 0;
    }
    let Some((border_col, border)) = line.chars().enumerate().find(|(_, c)| !c.is_whitespace())
    else {
        return 0;
    };
    if !is_left_border_glyph(border) {
        return 0;
    }
    line.chars()
        .enumerate()
        .skip(border_col.saturating_add(1))
        .find(|(_, c)| !c.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| line.chars().count())
        .min(u16::MAX as usize) as u16
}

fn is_left_border_glyph(c: char) -> bool {
    matches!(c, '┃' | '│' | '║' | '▕' | '▏' | '▌' | '▐' | '█' | '|')
}

fn panel_offsets(panel_heights: &[usize]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(panel_heights.len());
    let mut y = 0usize;
    for height in panel_heights {
        offsets.push(y);
        y = y.saturating_add(*height).saturating_add(1);
    }
    offsets
}

fn bottom_aligned_scroll(
    panel_offsets: &[usize],
    panel_heights: &[usize],
    viewport_height: u16,
) -> usize {
    let Some((&last_offset, &last_height)) = panel_offsets.last().zip(panel_heights.last()) else {
        return 0;
    };
    let viewport_height = viewport_height as usize;
    last_offset.saturating_sub(viewport_height.saturating_sub(last_height))
}

fn total_panel_height(panel_heights: &[usize]) -> usize {
    panel_heights
        .iter()
        .copied()
        .sum::<usize>()
        .saturating_add(panel_heights.len().saturating_sub(1))
}

fn render_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    total: usize,
    visible_rows: usize,
    scroll: usize,
) {
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some(" "))
        .track_style(Style::new().bg(theme::element()))
        .thumb_symbol(" ")
        .thumb_style(Style::new().bg(theme::border()));
    let scrollbar_area = Rect::new(area.x, area.y, area.width, area.height);
    let track_height = scrollbar_area.height;
    let max_scroll = total.saturating_sub(visible_rows);
    let thumb_height = ((u32::from(track_height) * visible_rows as u32) / total as u32)
        .max(1)
        .min(u32::from(track_height.saturating_sub(1)));
    let scroll_scale = u32::from(track_height).saturating_sub(thumb_height);
    let content_length = max_scroll.saturating_mul(scroll_scale as usize) + 1;
    let viewport_length = thumb_height.saturating_mul(max_scroll as u32).max(1);
    let mut state = ScrollbarState::new(content_length)
        .position(scroll.saturating_mul(scroll_scale as usize))
        .viewport_content_length(viewport_length as usize);
    frame.render_stateful_widget(scrollbar, scrollbar_area, &mut state);
}

fn draw_sidebar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    item: &InboxItemRow,
    app: &mut AppState,
) {
    frame.render_widget(
        Block::default().style(Style::new().bg(theme::element())),
        area,
    );
    let inner = area.inner(Margin::new(2, 1));
    let workspace_path = workspace_path(snapshot, item);
    let footer_height = sidebar_footer_height(inner.width, workspace_path.as_deref());
    let [content, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer_height)]).areas(inner);
    app.workspace_path_to_copy = workspace_path.clone();
    app.workspace_path_rect = workspace_path_rect(footer, workspace_path.as_deref());
    let label_width = 10usize;
    let value_width = content.width.saturating_sub(label_width as u16 + 1) as usize;
    let mut metadata = vec![
        ("created", item_time_label(item)),
        ("id", item.identifier.clone()),
        ("source", item.source.clone()),
        ("state", item.status.clone()),
    ];
    if !item.repo_id.is_empty() {
        metadata.push(("repo", item.repo_id.clone()));
    }
    if let Some(priority) = item.priority {
        metadata.push(("priority", format!("P{priority}")));
    }
    if !item.labels.is_empty() {
        metadata.push(("labels", item.labels.join(", ")));
    }
    if let Some(author) = item.author.as_deref() {
        metadata.push(("author", author.to_string()));
    }
    metadata.push(("workspace", workspace_label(snapshot, item, app.tick)));
    metadata.sort_by_key(|(label, _)| *label);

    let mut lines = vec![Line::styled("Issue", Style::new().fg(theme::text()).bold())];
    lines.extend(
        metadata
            .iter()
            .map(|(label, value)| sidebar_kv(label, value, label_width, value_width)),
    );

    let mut runs = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .collect::<Vec<_>>();
    runs.sort_by_key(|run| run.created_at);
    if let Some(run) = runs.last() {
        let agent_count = snapshot
            .agent_run_history
            .iter()
            .filter(|history| polyphony_core::agent_history_matches_run(run, history))
            .count()
            + snapshot
                .running
                .iter()
                .filter(|agent| polyphony_core::running_agent_matches_run(run, agent))
                .count();
        lines.push(Line::raw(""));
        lines.push(Line::styled("Flow", Style::new().fg(theme::text()).bold()));
        lines.push(sidebar_kv_styled(
            "status",
            &run.status.to_string(),
            label_width,
            value_width,
            run_status_color(&run.status),
        ));
        lines.push(sidebar_kv(
            "tasks",
            &format!("{}/{}", run.tasks_completed, run.task_count),
            label_width,
            value_width,
        ));
        lines.push(sidebar_kv(
            "agents",
            &agent_count.to_string(),
            label_width,
            value_width,
        ));
        if let Some(workspace) = &run.workspace_key {
            lines.push(sidebar_kv("branch", workspace, label_width, value_width));
        }
    }

    let tasks = tasks_for_item(snapshot, item);
    if !tasks.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Tasks", Style::new().fg(theme::text()).bold()));
        for task in tasks {
            let icon = match task.status.to_string().as_str() {
                "completed" => "✓",
                "in_progress" => "●",
                "failed" => "⊘",
                _ => "·",
            };
            let mut spans = vec![Span::styled(
                format!("{icon} "),
                Style::new().fg(theme::primary()),
            )];
            if let Some(agent) = task.agent_name.as_deref() {
                spans.push(Span::styled(
                    format!("[{agent}] "),
                    Style::new().fg(theme::secondary()),
                ));
            }
            spans.push(Span::styled(&task.title, Style::new().fg(theme::muted())));
            lines.push(Line::from(spans));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("·", Style::new().fg(theme::muted())),
            Span::styled(" todo  ", Style::new().fg(theme::muted())),
            Span::styled("●", Style::new().fg(theme::primary())),
            Span::styled(" running  ", Style::new().fg(theme::muted())),
            Span::styled("⊘", Style::new().fg(theme::primary())),
            Span::styled(" blocked  ", Style::new().fg(theme::muted())),
            Span::styled("✓", Style::new().fg(theme::done())),
            Span::styled(" done", Style::new().fg(theme::muted())),
        ]));
    }

    let running = snapshot
        .running
        .iter()
        .filter(|run| run.issue_id == item.item_id || run.issue_identifier == item.identifier)
        .collect::<Vec<_>>();
    if !running.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Running",
            Style::new().fg(theme::text()).bold(),
        ));
        for run in running {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(theme::primary())),
                Span::styled(&run.agent_name, Style::new().fg(theme::text())),
                Span::styled(format!(" {}", run.state), Style::new().fg(theme::muted())),
            ]));
            if let Some(message) = run.last_message.as_deref() {
                lines.push(Line::styled(message, Style::new().fg(theme::muted())));
            }
        }
    }

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .style(Style::new().bg(theme::element()))
        .render(content, frame.buffer_mut());

    render_sidebar_footer(frame, footer, snapshot, workspace_path.as_deref());
    capture_and_highlight_sidebar(frame, area, app);
}

fn capture_and_highlight_sidebar(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut AppState) {
    app.sidebar_text_rect = area;
    app.sidebar_visible_lines.clear();
    let selection = sidebar_selection_bounds(app);
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let Some(cell) = frame.buffer_mut().cell_mut((area.x + x, area.y + y)) else {
                continue;
            };
            line.push_str(cell.symbol());
            if position_selected(selection, y, x, 0) {
                cell.set_style(
                    cell.style()
                        .fg(theme::bg())
                        .bg(theme::primary())
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
        app.sidebar_visible_lines.push(line.trim_end().to_string());
    }
}

fn sidebar_selection_bounds(app: &AppState) -> Option<((u16, u16), (u16, u16))> {
    let mut start = app.sidebar_selection_start?;
    let mut end = app.sidebar_selection_end?;
    let area = app.sidebar_text_rect;
    if area.is_empty() {
        return None;
    }
    if position_after(start, end) {
        std::mem::swap(&mut start, &mut end);
    }
    let viewport_bottom = area.y.saturating_add(area.height.saturating_sub(1));
    if end.1 < area.y || start.1 > viewport_bottom {
        return None;
    }
    let start_row = start.1.saturating_sub(area.y);
    let end_row = end
        .1
        .saturating_sub(area.y)
        .min(area.height.saturating_sub(1));
    let start_col = if start.1 < area.y {
        0
    } else {
        start.0.saturating_sub(area.x)
    };
    let end_col = if end.1 > viewport_bottom {
        u16::MAX
    } else {
        end.0.saturating_sub(area.x)
    };
    Some(((start_row, start_col), (end_row, end_col)))
}

fn position_after(a: (u16, u16), b: (u16, u16)) -> bool {
    (a.1, a.0) > (b.1, b.0)
}

fn render_sidebar_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    workspace_path: Option<&str>,
) {
    let mut lines = Vec::new();
    if let Some(path) = workspace_path {
        lines.push(Line::styled("workspace", Style::new().fg(theme::muted())));
        lines.push(Line::styled(
            path.to_string(),
            Style::new().fg(theme::primary()),
        ));
    }
    lines.push(Line::from(vec![
        Span::styled("tracker:", Style::new().fg(theme::muted())),
        Span::styled(tracker::label(snapshot), Style::new().fg(theme::primary())),
    ]));

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .style(Style::new().bg(theme::element()))
        .render(area, frame.buffer_mut());
}

fn sidebar_footer_height(width: u16, workspace_path: Option<&str>) -> u16 {
    let Some(path) = workspace_path else {
        return 1;
    };
    let path_lines = (path.chars().count() as u16).div_ceil(width.max(1)).max(1);
    path_lines.saturating_add(2)
}

fn workspace_path_rect(area: Rect, workspace_path: Option<&str>) -> Rect {
    let Some(path) = workspace_path else {
        return Rect::default();
    };
    let path_lines = (path.chars().count() as u16)
        .div_ceil(area.width.max(1))
        .max(1);
    Rect::new(area.x, area.y.saturating_add(1), area.width, path_lines)
}

fn workspace_path(snapshot: &RuntimeSnapshot, item: &InboxItemRow) -> Option<String> {
    latest_run(snapshot, item)
        .and_then(|run| run.workspace_path.as_ref())
        .map(|path| path.display().to_string())
        .or_else(|| {
            snapshot
                .running
                .iter()
                .find(|agent| {
                    agent.issue_id == item.item_id || agent.issue_identifier == item.identifier
                })
                .map(|agent| agent.workspace_path.display().to_string())
        })
        .or_else(|| {
            snapshot
                .agent_run_history
                .iter()
                .filter(|history| {
                    history.issue_id == item.item_id || history.issue_identifier == item.identifier
                })
                .max_by_key(|history| history.started_at)
                .and_then(|history| history.workspace_path.as_ref())
                .map(|path| path.display().to_string())
        })
}

fn workspace_label(snapshot: &RuntimeSnapshot, item: &InboxItemRow, tick: u32) -> String {
    if snapshot.loading.any_active() {
        return theme::BRAILLE_SPINNER[(tick / 4) as usize % theme::BRAILLE_SPINNER.len()]
            .to_string();
    }
    if item.has_workspace {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn issue_control_state(snapshot: &RuntimeSnapshot, item: &InboxItemRow) -> &'static str {
    if snapshot
        .running
        .iter()
        .any(|agent| agent.issue_id == item.item_id || agent.issue_identifier == item.identifier)
    {
        return "agent running";
    }
    match latest_run(snapshot, item).map(|run| run.status) {
        Some(
            RunStatus::Pending | RunStatus::Planning | RunStatus::InProgress | RunStatus::Review,
        ) => "orchestrating",
        Some(RunStatus::Failed | RunStatus::Cancelled) => "paused",
        Some(RunStatus::Delivered) => "delivered",
        None => "ready",
    }
}

fn latest_run<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &InboxItemRow,
) -> Option<&'a polyphony_core::RunRow> {
    snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .max_by_key(|run| run.created_at)
}

fn sidebar_kv(label: &str, value: &str, label_width: usize, value_width: usize) -> Line<'static> {
    sidebar_kv_styled(label, value, label_width, value_width, theme::text())
}

fn sidebar_kv_styled(
    label: &str,
    value: &str,
    label_width: usize,
    value_width: usize,
    value_color: ratatui::style::Color,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<label_width$}"),
            Style::new().fg(theme::muted()),
        ),
        Span::styled(" ", Style::new().fg(theme::muted())),
        Span::styled(
            format!(
                "{:>value_width$}",
                crate::format::truncate(value, value_width)
            ),
            Style::new().fg(value_color),
        ),
    ])
}

fn run_status_color(status: &RunStatus) -> ratatui::style::Color {
    match status {
        RunStatus::Delivered => theme::done(),
        RunStatus::Failed | RunStatus::Cancelled => theme::error(),
        RunStatus::InProgress | RunStatus::Planning | RunStatus::Review => theme::primary(),
        RunStatus::Pending => theme::muted(),
    }
}

fn selected_item<'a>(snapshot: &'a RuntimeSnapshot, app: &AppState) -> Option<&'a InboxItemRow> {
    display_rows_matching(snapshot, &app.search_query)
        .get(app.selected)
        .and_then(|row| snapshot.inbox_items.get(row.item_idx))
}

fn tasks_for_item<'a>(snapshot: &'a RuntimeSnapshot, item: &InboxItemRow) -> Vec<&'a TaskRow> {
    let run_ids = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .map(|run| &run.id)
        .collect::<Vec<_>>();
    snapshot
        .tasks
        .iter()
        .filter(|task| run_ids.contains(&&task.run_id))
        .collect()
}
