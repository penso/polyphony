mod agents;
mod deliverables;
mod footer;
mod header;
mod logs;
mod orchestrator;
pub(crate) mod popups;
mod tasks;
mod time;
mod triggers;

pub(crate) use time::*;

use polyphony_core::RuntimeSnapshot;

use crate::app::{ActiveTab, AppState};

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
    app.content_area = areas[1];

    match app.active_tab {
        ActiveTab::Triggers => triggers::draw_triggers_tab(frame, areas[1], snapshot, app),
        ActiveTab::Agents => agents::draw_agents_tab(frame, areas[1], snapshot, app),
        ActiveTab::Orchestrator => {
            orchestrator::draw_orchestrator_tab(frame, areas[1], snapshot, app);
        },
        ActiveTab::Tasks => tasks::draw_tasks_tab(frame, areas[1], snapshot, app),
        ActiveTab::Deliverables => {
            deliverables::draw_deliverables_tab(frame, areas[1], snapshot, app);
        },
        ActiveTab::Logs => logs::draw_logs_tab(frame, areas[1], snapshot, app),
    }

    footer::draw_footer(frame, areas[2], app);

    // Popups render on top
    if app.show_issue_detail
        && let Some(issue) = app.selected_trigger(snapshot).cloned()
    {
        popups::draw_issue_detail_modal(frame, &issue, snapshot, app);
    }

    if app.show_task_detail
        && let Some(task) = app.selected_task(snapshot).cloned()
    {
        popups::draw_task_detail_modal(frame, &task, app);
    }

    if app.show_movement_detail
        && let Some(movement) = app.selected_movement(snapshot).cloned()
    {
        popups::draw_movement_detail_modal(frame, &movement, snapshot, app);
    }

    if app.show_deliverable_detail
        && let Some(movement) = app.selected_deliverable(snapshot).cloned()
    {
        popups::draw_deliverable_detail_modal(frame, &movement, app);
    }

    if app.show_agent_detail {
        popups::draw_agent_detail_modal(frame, snapshot, app);
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
