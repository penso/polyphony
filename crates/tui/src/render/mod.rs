mod deliverables;
mod footer;
mod header;
pub(crate) mod issues;
mod logs;
mod orchestrator;
pub(crate) mod popups;
mod tasks;

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
        ActiveTab::Issues => issues::draw_issues_tab(frame, areas[1], snapshot, app),
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
        && let Some(issue) = app.selected_issue(snapshot).cloned()
    {
        popups::draw_issue_detail_modal(frame, &issue, snapshot.tracker_kind, app);
    }

    if app.show_mode_modal {
        popups::draw_mode_modal(frame, snapshot, app);
    }

    if app.leaving {
        popups::draw_leaving_modal(frame, app.theme);
    }
}
