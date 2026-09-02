use std::collections::HashSet;

use polyphony_core::{DispatchMode, InboxItemRow, RuntimeSnapshot};
use ratatui::{
    layout::{Margin, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
};

use crate::{
    app::{AppState, Route, clamp_selection},
    command_palette, create_issue_modal,
    detail::draw_detail,
    dispatch_mode_picker,
    format::{item_time_label, truncate},
    rows::{display_rows_matching, hierarchy_prefix},
    status::{state_color, state_icon},
    theme, tracker,
};

pub(crate) fn draw(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot, app: &mut AppState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(theme::bg())), area);

    let route = app.route;
    match route {
        Route::Inbox => {
            let panel_width = 112u16.min(area.width.saturating_sub(4)).max(60);
            let list_height = 14u16.min(area.height.saturating_sub(13)).max(5);
            let panel_height = list_height + 12;
            let panel = Rect::new(
                area.x + area.width.saturating_sub(panel_width) / 2,
                area.y + area.height.saturating_sub(panel_height) / 2,
                panel_width,
                panel_height.min(area.height),
            );

            draw_logo(frame, panel);
            draw_inbox(frame, panel, list_height, snapshot, app);
        },
        Route::Detail => draw_detail(frame, area, snapshot, app),
    }
    if route == Route::Inbox {
        draw_footer(frame, area, snapshot);
    }

    if app.command_palette_open {
        command_palette::render(frame, area, app);
    }
    if app.dispatch_mode_picker_open {
        dispatch_mode_picker::render(frame, area, snapshot, app);
    }
    if app.create_issue_modal.is_some() {
        create_issue_modal::render(frame, area, app);
    }
    draw_toast(frame, area, app);
}

fn draw_toast(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut AppState) {
    if area.height < 2 {
        return;
    }
    if app.toast_message.is_some() && app.tick >= app.toast_until_tick {
        app.toast_message = None;
    }
    let Some(message) = app.toast_message.as_deref() else {
        return;
    };
    let width = message.chars().count() as u16 + 4;
    let toast = Rect::new(
        area.x + area.width.saturating_sub(width.saturating_add(2)),
        area.y + 1,
        width.min(area.width),
        3.min(area.height.saturating_sub(1)),
    );
    fill_rect(frame, toast, theme::primary());
    Paragraph::new(Line::styled(
        format!("  {message}  "),
        Style::new().fg(theme::bg()).bg(theme::primary()).bold(),
    ))
    .render(toast.inner(Margin::new(0, 1)), frame.buffer_mut());
}

fn fill_rect(frame: &mut ratatui::Frame<'_>, area: Rect, bg: Color) {
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_symbol(" ").set_style(Style::new().bg(bg));
            }
        }
    }
}

fn draw_logo(frame: &mut ratatui::Frame<'_>, panel: Rect) {
    for (idx, line) in theme::LOGO.iter().enumerate() {
        let line_width = line.chars().count() as u16;
        let x = panel.x + panel.width.saturating_sub(line_width) / 2;
        let fg = if idx < 3 {
            theme::primary()
        } else {
            theme::muted()
        };
        Line::styled(*line, Style::new().fg(fg).bold()).render(
            Rect::new(
                x,
                panel.y + idx as u16,
                panel.width.saturating_sub(x - panel.x),
                1,
            ),
            frame.buffer_mut(),
        );
    }
}

#[allow(clippy::too_many_lines)]
fn draw_inbox(
    frame: &mut ratatui::Frame<'_>,
    panel: Rect,
    list_height: u16,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    draw_search(frame, panel, app);

    let list = Rect::new(panel.x, panel.y + 8, panel.width, list_height);
    let scrollbar_width = 2u16;
    let table_width = list.width.saturating_sub(scrollbar_width);
    let visible_rows = list.height.saturating_sub(2) as usize;
    let rows = display_rows_matching(snapshot, &app.search_query);
    clamp_selection(app, rows.len());
    let max_scroll = rows.len().saturating_sub(visible_rows);
    app.scroll = app.scroll.min(max_scroll);
    if app.selected < app.scroll {
        app.scroll = app.selected;
    } else if app.selected >= app.scroll.saturating_add(visible_rows) {
        app.scroll = app
            .selected
            .saturating_sub(visible_rows.saturating_sub(1))
            .min(max_scroll);
    }
    let scroll = app.scroll;
    app.visible_rows = visible_rows;
    app.list_rect = list;

    let source_width = 7usize;
    let state_width = 5usize;
    let fixed_width = 10u16 + source_width as u16 + state_width as u16 + 1;
    let title_width = table_width.saturating_sub(fixed_width) as usize;
    Line::from(vec![
        Span::styled("    age   ", Style::new().fg(theme::muted())),
        Span::styled(
            format!("{:<source_width$}", "source"),
            Style::new().fg(theme::muted()),
        ),
        Span::styled(
            format!("{:<title_width$} ", "title"),
            Style::new().fg(theme::muted()),
        ),
        Span::styled(
            format!("{:>state_width$}", "state"),
            Style::new().fg(theme::muted()),
        ),
    ])
    .render(
        Rect::new(list.x, list.y, table_width, 1),
        frame.buffer_mut(),
    );

    let running_ids = snapshot
        .running
        .iter()
        .map(|run| run.issue_id.as_str())
        .collect::<HashSet<_>>();
    for row in 0..visible_rows {
        let display_idx = scroll + row;
        let Some(display_row) = rows.get(display_idx).copied() else {
            break;
        };
        let Some(item) = snapshot.inbox_items.get(display_row.item_idx) else {
            continue;
        };
        let y = list.y + 1 + row as u16;
        let selected = display_idx == app.selected;
        let bg = if selected {
            theme::element()
        } else {
            theme::bg()
        };
        for x in list.x..list.x + table_width {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_style(Style::new().bg(bg));
            }
        }

        let marker = if selected {
            "▶"
        } else {
            " "
        };
        let activity = activity_indicator(item, &running_ids, app.tick);
        let title = format!(
            "{}{} {}",
            hierarchy_prefix(display_row.depth, display_row.last_child),
            state_icon(&item.status),
            item.title
        );
        let row_text_color = match display_row.context_only {
            true => theme::secondary(),
            false => theme::text(),
        };
        let row_source_color = match display_row.context_only {
            true => theme::secondary(),
            false => theme::primary(),
        };

        Line::from(vec![
            Span::styled(
                format!("{marker} "),
                Style::new().fg(theme::primary()).bg(bg),
            ),
            Span::styled(
                format!("{} ", activity.0),
                Style::new().fg(activity.1).bg(bg),
            ),
            Span::styled(
                format!("{:<5} ", item_time_label(item)),
                Style::new().fg(theme::muted()).bg(bg),
            ),
            Span::styled(
                format!("{:<source_width$}", item.source),
                Style::new().fg(row_source_color).bg(bg),
            ),
            Span::styled(
                format!("{:<title_width$}", truncate(&title, title_width)),
                Style::new().fg(row_text_color).bg(bg),
            ),
            Span::styled(
                format!(" {:>state_width$}", state_icon(&item.status)),
                Style::new().fg(state_color(&item.status)).bg(bg),
            ),
        ])
        .render(Rect::new(list.x, y, table_width, 1), frame.buffer_mut());
    }

    if rows.len() > visible_rows {
        render_scrollbar(frame, list, rows.len(), visible_rows, scroll);
    }

    draw_table_footer(frame, panel, list, rows.len(), visible_rows, scroll);
}

fn draw_search(frame: &mut ratatui::Frame<'_>, panel: Rect, app: &AppState) {
    let cursor_on = (app.tick / 6).is_multiple_of(2);
    let label = if app.search_query.is_empty() {
        let first_char_style = if cursor_on {
            Style::new().fg(theme::bg()).bg(theme::primary())
        } else {
            Style::new().fg(theme::muted())
        };
        Line::from(vec![
            Span::styled("t", first_char_style),
            Span::styled("ype to filter", Style::new().fg(theme::muted())),
        ])
    } else {
        let cursor = if cursor_on {
            "█"
        } else {
            " "
        };
        Line::from(vec![
            Span::styled(&app.search_query, Style::new().fg(theme::text())),
            Span::styled(cursor, Style::new().fg(theme::primary())),
        ])
    };
    label.render(
        Rect::new(panel.x, panel.y + 6, panel.width, 1),
        frame.buffer_mut(),
    );
}

fn draw_table_footer(
    frame: &mut ratatui::Frame<'_>,
    panel: Rect,
    list: Rect,
    total: usize,
    visible_rows: usize,
    scroll: usize,
) {
    Line::from(vec![
        Span::styled("state ", Style::new().fg(theme::muted())),
        Span::styled("● ", Style::new().fg(theme::primary())),
        Span::styled("open/active  ", Style::new().fg(theme::muted())),
        Span::styled("○ ", Style::new().fg(theme::primary())),
        Span::styled("todo  ", Style::new().fg(theme::muted())),
        Span::styled("◷ ", Style::new().fg(theme::primary())),
        Span::styled("waiting  ", Style::new().fg(theme::muted())),
        Span::styled("✓ ", Style::new().fg(theme::done())),
        Span::styled("done  ", Style::new().fg(theme::muted())),
        Span::styled("⊘ ", Style::new().fg(theme::muted())),
        Span::styled("cancel/ignored  ", Style::new().fg(theme::muted())),
        Span::styled("◌ ", Style::new().fg(theme::error())),
        Span::styled("draft", Style::new().fg(theme::muted())),
    ])
    .render(
        Rect::new(panel.x, list.y + list.height, panel.width, 1),
        frame.buffer_mut(),
    );

    Line::from(vec![
        Span::styled("front ", Style::new().fg(theme::muted())),
        Span::styled("● ", Style::new().fg(theme::primary())),
        Span::styled("workspace", Style::new().fg(theme::muted())),
    ])
    .render(
        Rect::new(panel.x, list.y + list.height + 1, panel.width, 1),
        frame.buffer_mut(),
    );

    let range_start = if total == 0 {
        0
    } else {
        scroll + 1
    };
    let range_end = (scroll + visible_rows).min(total);
    let range = format!("{range_start}-{range_end} of {total} • sorted by oldest");
    let range_width = range.chars().count() as u16;
    let range_x = panel.x + panel.width.saturating_sub(range_width);
    Line::styled(range, Style::new().fg(theme::muted())).render(
        Rect::new(range_x, list.y + list.height + 1, range_width, 1),
        frame.buffer_mut(),
    );

    let hint = Line::from(vec![
        Span::styled("↑/↓", Style::new().fg(theme::text())),
        Span::styled(":navigate  ", Style::new().fg(theme::muted())),
        Span::styled("typing", Style::new().fg(theme::text())),
        Span::styled(":search  ", Style::new().fg(theme::muted())),
        Span::styled("Enter", Style::new().fg(theme::text())),
        Span::styled(":open  ", Style::new().fg(theme::muted())),
        Span::styled("n", Style::new().fg(theme::text())),
        Span::styled(":new issue  ", Style::new().fg(theme::muted())),
        Span::styled("Ctrl+P", Style::new().fg(theme::text())),
        Span::styled(":commands", Style::new().fg(theme::muted())),
    ]);
    let hint_width = 76u16.min(panel.width);
    let hint_x = panel.x + panel.width.saturating_sub(hint_width);
    hint.render(
        Rect::new(hint_x, list.y + list.height + 2, hint_width, 1),
        frame.buffer_mut(),
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    if area.height == 0 {
        return;
    }
    let footer = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    )
    .inner(Margin::new(2, 0));
    Line::from(vec![
        Span::styled("tracker:", Style::new().fg(theme::muted())),
        Span::styled(tracker::label(snapshot), Style::new().fg(theme::primary())),
        Span::styled("  orchestrator ", Style::new().fg(theme::muted())),
        Span::styled(
            "•",
            Style::new().fg(dispatch_mode_color(snapshot.dispatch_mode)),
        ),
        Span::styled(
            snapshot.dispatch_mode.to_string(),
            Style::new().fg(theme::muted()),
        ),
    ])
    .render(footer, frame.buffer_mut());

    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let version_width = version.chars().count() as u16;
    Line::styled(version, Style::new().fg(theme::muted())).render(
        Rect::new(
            footer.x + footer.width.saturating_sub(version_width),
            footer.y,
            version_width,
            1,
        ),
        frame.buffer_mut(),
    );
}

fn dispatch_mode_color(mode: DispatchMode) -> Color {
    match mode {
        DispatchMode::Stop => theme::error(),
        DispatchMode::Idle | DispatchMode::Manual => theme::secondary(),
        DispatchMode::Automatic | DispatchMode::Nightshift => theme::done(),
    }
}

fn render_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    list: Rect,
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
    let scrollbar_area = Rect::new(
        list.x,
        list.y.saturating_add(1),
        list.width,
        list.height.saturating_sub(2),
    );
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

fn activity_indicator<'a>(
    item: &InboxItemRow,
    running_ids: &HashSet<&str>,
    tick: u32,
) -> (&'a str, Color) {
    if running_ids.contains(item.item_id.as_str())
        || item.status.eq_ignore_ascii_case("in progress")
    {
        (
            theme::BRAILLE_SPINNER[tick as usize % theme::BRAILLE_SPINNER.len()],
            theme::primary(),
        )
    } else if item.has_workspace {
        ("●", theme::primary())
    } else {
        (" ", theme::muted())
    }
}
