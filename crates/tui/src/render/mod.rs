mod agents;
mod deliverables;
pub(crate) mod detail_agent;
pub(crate) mod detail_common;
pub(crate) mod detail_deliverable;
pub(crate) mod detail_movement;
pub(crate) mod detail_task;
pub(crate) mod detail_trigger;
mod footer;
mod header;
mod logs;
mod orchestrator;
pub(crate) mod popups;
mod tasks;
mod time;
mod triggers;

use polyphony_core::RuntimeSnapshot;
pub(crate) use time::*;

use crate::app::{ActiveTab, AppState, SplitFocus};

/// Minimum terminal width to engage the side-by-side master-detail layout.
const SPLIT_MIN_WIDTH: u16 = 140;

pub fn render(frame: &mut ratatui::Frame<'_>, snapshot: &RuntimeSnapshot, app: &mut AppState) {
    app.frame_count = app.frame_count.wrapping_add(1);

    let areas = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(3), // Header tabs
            ratatui::layout::Constraint::Min(6),    // Main content
            ratatui::layout::Constraint::Length(1), // Footer version bar
        ])
        .split(frame.area());

    header::draw_header(frame, areas[0], snapshot, app);

    // Decide layout mode: split (master-detail) or full-page
    let use_split = frame.area().width >= SPLIT_MIN_WIDTH
        && app.detail_stack.len() == 1
        && !matches!(app.active_tab, ActiveTab::Logs);

    if let Some(detail) = app.current_detail().cloned() {
        if use_split {
            // Master-detail: list on left (35%), detail on right (65%)
            let split = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Horizontal)
                .constraints([
                    ratatui::layout::Constraint::Percentage(35),
                    ratatui::layout::Constraint::Percentage(65),
                ])
                .split(areas[1]);
            // The content_area for click detection maps to the list pane
            app.content_area = split[0];
            let detail_focused = app.split_focus == SplitFocus::Detail;
            render_tab_table(frame, split[0], snapshot, app, !detail_focused);
            app.detail_border_focused = detail_focused;
            render_detail_view(frame, split[1], &detail, snapshot, app);
        } else {
            // Full-page detail (deep stack or narrow terminal)
            app.content_area = areas[1];
            app.detail_border_focused = true;
            render_detail_view(frame, areas[1], &detail, snapshot, app);
        }
    } else {
        app.content_area = areas[1];
        render_tab_table(frame, areas[1], snapshot, app, true);
    }

    footer::draw_footer(frame, areas[2], snapshot, app);

    if app.confirm_quit {
        popups::draw_confirm_quit(frame, app);
    }

    if app.show_mode_modal {
        popups::draw_mode_modal(frame, snapshot, app);
    }

    if app.show_agent_picker {
        popups::draw_agent_picker_modal(frame, snapshot, app);
    }

    if app.leaving {
        popups::draw_leaving_modal(frame, app.theme);
    }
}

fn render_tab_table(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
    _focused: bool,
) {
    match app.active_tab {
        ActiveTab::Triggers => triggers::draw_triggers_tab(frame, area, snapshot, app),
        ActiveTab::Agents => agents::draw_agents_tab(frame, area, snapshot, app),
        ActiveTab::Orchestrator => orchestrator::draw_orchestrator_tab(frame, area, snapshot, app),
        ActiveTab::Tasks => tasks::draw_tasks_tab(frame, area, snapshot, app),
        ActiveTab::Deliverables => deliverables::draw_deliverables_tab(frame, area, snapshot, app),
        ActiveTab::Logs => logs::draw_logs_tab(frame, area, snapshot, app),
    }
}

fn render_detail_view(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    detail: &crate::app::DetailView,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    match detail {
        crate::app::DetailView::Trigger { trigger_id, .. } => {
            detail_trigger::draw_trigger_detail(frame, area, trigger_id, snapshot, app);
        },
        crate::app::DetailView::Movement { movement_id, .. } => {
            detail_movement::draw_movement_detail(frame, area, movement_id, snapshot, app);
        },
        crate::app::DetailView::Task { task_id, .. } => {
            detail_task::draw_task_detail(frame, area, task_id, snapshot, app);
        },
        crate::app::DetailView::Agent { agent_index, .. } => {
            detail_agent::draw_agent_detail(frame, area, *agent_index, snapshot, app);
        },
        crate::app::DetailView::Deliverable { movement_id, .. } => {
            detail_deliverable::draw_deliverable_detail(frame, area, movement_id, snapshot, app);
        },
        crate::app::DetailView::Events { filter, .. } => {
            orchestrator::draw_filtered_events(frame, area, filter, snapshot, app);
        },
    }
}
