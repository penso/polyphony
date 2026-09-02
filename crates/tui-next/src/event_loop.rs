use std::{io, io::Write, process::Command, time::Duration};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use polyphony_core::{DispatchMode, InboxItemKind, RunStatus, RuntimeSnapshot, TaskStatus};
use polyphony_orchestrator::RuntimeCommand;
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect, text::Line};
use ratatui_opentui_loader::KittLoader;
use tokio::sync::{mpsc, watch};

use crate::{
    Error,
    app::{AppState, Route, clamp_selection},
    command_palette, create_issue_modal, dispatch_mode_picker,
    render::draw,
    rows::display_rows_matching,
    session, theme,
};

const DETAIL_MOUSE_SCROLL_ROWS: u16 = 5;
const SELECTION_EDGE_SCROLL_ROWS: u16 = 2;
const SELECTION_EDGE_SCROLL_THRESHOLD: u16 = 2;

pub async fn run(
    mut snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
) -> Result<(), Error> {
    enable_raw_mode()?;
    let _cleanup = TerminalCleanup;

    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let _ = command_tx.send(RuntimeCommand::Refresh);

    let mut event_stream = event::EventStream::new();
    let mut app = AppState {
        loader: KittLoader::build(theme::primary(), 8, 6, 4, 4, 0.25, 0.55),
        settings: crate::settings::TuiSettings::load(),
        ..AppState::default()
    };
    let mut snapshot = snapshot_rx.borrow().clone();
    let mut needs_draw = true;

    loop {
        if needs_draw {
            terminal.draw(|frame| draw(frame, &snapshot, &mut app))?;
            needs_draw = false;
        }

        tokio::select! {
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        if handle_key(&mut app, key.code, key.modifiers, &snapshot, &command_tx) {
                            break;
                        }
                        needs_draw = true;
                    },
                    Some(Ok(Event::Mouse(mouse))) => {
                        handle_mouse(&mut app, &snapshot, mouse);
                        needs_draw = true;
                    },
                    Some(Ok(Event::Resize(_, _))) => needs_draw = true,
                    Some(Ok(_)) => {},
                    Some(Err(_)) => {},
                    None => break,
                }
            }
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                snapshot = snapshot_rx.borrow().clone();
                let row_count = display_rows_matching(&snapshot, &app.search_query).len();
                clamp_selection(&mut app, row_count);
                needs_draw = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                app.tick = app.tick.wrapping_add(1);
                if runtime_work_active(&snapshot) {
                    app.loader.tick();
                    app.loader.tick();
                }
                let selection_scrolled = auto_scroll_session_selection(&mut app);
                if selection_scrolled || app.route == Route::Inbox || app.toast_message.is_some() || runtime_work_active(&snapshot) {
                    needs_draw = true;
                }
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

fn runtime_work_active(snapshot: &RuntimeSnapshot) -> bool {
    !snapshot.running.is_empty()
        || snapshot.loading.any_active()
        || snapshot.runs.iter().any(|run| {
            matches!(
                run.status,
                RunStatus::Pending
                    | RunStatus::Planning
                    | RunStatus::InProgress
                    | RunStatus::Review
            )
        })
        || snapshot
            .inbox_items
            .iter()
            .any(|item| item.status.eq_ignore_ascii_case("in progress"))
}

fn auto_scroll_session_selection(app: &mut AppState) -> bool {
    if app.route != Route::Detail || !app.session_selecting || app.session_text_rect.is_empty() {
        return false;
    }
    let Some((column, row)) = app.mouse_pos else {
        return false;
    };
    let area = app.session_text_rect;
    if column < area.x || column >= area.x.saturating_add(area.width) {
        return false;
    }

    let top_edge = area.y.saturating_add(SELECTION_EDGE_SCROLL_THRESHOLD);
    let bottom_edge = area
        .y
        .saturating_add(area.height.saturating_sub(1))
        .saturating_sub(SELECTION_EDGE_SCROLL_THRESHOLD);
    let previous_scroll = app.detail_scroll;
    if row <= top_edge {
        app.detail_scroll = app.detail_scroll.saturating_sub(SELECTION_EDGE_SCROLL_ROWS);
    } else if row >= bottom_edge {
        app.detail_scroll = app
            .detail_scroll
            .saturating_add(SELECTION_EDGE_SCROLL_ROWS)
            .min(app.detail_scroll_max);
    }
    if app.detail_scroll == previous_scroll {
        return false;
    }

    app.detail_follow_bottom = false;
    app.session_selection_end = Some(session_position_at_mouse(app, column, row));
    true
}

fn handle_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    if app.command_palette_open {
        return handle_command_palette_key(app, code, modifiers, snapshot, command_tx);
    }

    if app.create_issue_modal.is_some() {
        return handle_create_issue_modal_key(app, code, modifiers, command_tx);
    }

    if app.dispatch_mode_picker_open {
        return handle_dispatch_mode_picker_key(app, code, modifiers, command_tx);
    }

    let rows = display_rows_matching(snapshot, &app.search_query);
    match code {
        KeyCode::Esc
            if app.route == Route::Detail
                && app.detail_input_mode != crate::app::DetailInputMode::None =>
        {
            app.detail_input_mode = crate::app::DetailInputMode::None;
            app.input.clear();
        },
        KeyCode::Esc if app.route == Route::Inbox && !app.search_query.is_empty() => {
            app.search_query.clear();
            app.selected = 0;
            app.scroll = 0;
        },
        KeyCode::Esc if app.route == Route::Detail => {
            app.route = Route::Inbox;
            app.detail_scroll = 0;
            app.detail_follow_bottom = false;
            app.children_expanded = false;
            app.detail_input_mode = crate::app::DetailInputMode::None;
            app.input.clear();
        },
        KeyCode::Esc => return true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.command_palette_open = true;
            command_palette::reset(app);
        },
        KeyCode::Char('m')
            if app.detail_input_mode == crate::app::DetailInputMode::None
                && app.route == Route::Detail
                && !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
        {
            dispatch_mode_picker::open(app, snapshot.dispatch_mode);
        },
        KeyCode::Char('t')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None
                && !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
        {
            app.settings.show_widget_timestamps = !app.settings.show_widget_timestamps;
            let _ = app.settings.save();
            app.toast_message = Some(format!(
                "timestamps {}",
                if app.settings.show_widget_timestamps {
                    "on"
                } else {
                    "off"
                }
            ));
            app.toast_until_tick = app.tick.saturating_add(24);
        },
        KeyCode::Char('n')
            if app.route == Route::Inbox
                && !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
        {
            create_issue_modal::open(app, snapshot);
        },
        KeyCode::Enter if app.route == Route::Inbox && !rows.is_empty() => {
            app.route = Route::Detail;
            app.detail_follow_bottom = true;
            app.children_expanded = false;
        },
        KeyCode::Enter
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::Hijack =>
        {
            submit_hijack(app, snapshot, command_tx);
        },
        KeyCode::Enter | KeyCode::Char('d')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            dispatch_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('s')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            stop_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('p')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            pause_or_resume_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('r')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            retry_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('x')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            clear_selected_session(app, snapshot, command_tx);
        },
        KeyCode::Char('y')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            copy_full_session(app, snapshot);
        },
        KeyCode::Char('h')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            app.detail_input_mode = crate::app::DetailInputMode::Hijack;
            app.input.clear();
        },
        KeyCode::Up if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_sub(1);
            app.detail_follow_bottom = false;
        },
        KeyCode::Down if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_add(1);
            app.detail_follow_bottom = false;
        },
        KeyCode::PageUp if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_sub(8);
            app.detail_follow_bottom = false;
        },
        KeyCode::PageDown if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_add(8);
            app.detail_follow_bottom = false;
        },
        KeyCode::Backspace
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::Hijack =>
        {
            app.input.pop();
        },
        KeyCode::Char(c)
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::Hijack
                && is_search_char(c, modifiers) =>
        {
            app.input.push(c);
        },
        KeyCode::Backspace if app.route == Route::Inbox => {
            app.search_query.pop();
            app.selected = app.selected.min(rows.len().saturating_sub(1));
        },
        KeyCode::Char(c) if app.route == Route::Inbox && is_search_char(c, modifiers) => {
            app.search_query.push(c);
            app.selected = 0;
            app.scroll = 0;
        },
        KeyCode::Up => select_previous_wrapping(app, rows.len()),
        KeyCode::Down => select_next_wrapping(app, rows.len()),
        KeyCode::PageUp => app.selected = app.selected.saturating_sub(app.visible_rows.max(1)),
        KeyCode::PageDown => {
            app.selected =
                (app.selected + app.visible_rows.max(1)).min(rows.len().saturating_sub(1));
        },
        KeyCode::Home => app.selected = 0,
        KeyCode::End => app.selected = rows.len().saturating_sub(1),
        _ => {},
    }
    false
}

fn handle_dispatch_mode_picker_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Esc | KeyCode::Char('m') => app.dispatch_mode_picker_open = false,
        KeyCode::Enter => {
            let mode = dispatch_mode_picker::selected_mode(app);
            let _ = command_tx.send(RuntimeCommand::SetMode(mode));
            app.dispatch_mode_picker_open = false;
        },
        KeyCode::Up | KeyCode::Char('k') => dispatch_mode_picker::selected_up(app),
        KeyCode::Down | KeyCode::Char('j') => dispatch_mode_picker::selected_down(app),
        _ => {},
    }
    false
}

fn select_previous_wrapping(app: &mut AppState, row_count: usize) {
    if row_count == 0 {
        app.selected = 0;
    } else if app.selected == 0 {
        app.selected = row_count - 1;
    } else {
        app.selected -= 1;
    }
}

fn select_next_wrapping(app: &mut AppState, row_count: usize) {
    if row_count == 0 {
        app.selected = 0;
    } else {
        app.selected = (app.selected + 1) % row_count;
    }
}

fn handle_create_issue_modal_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    let control_held = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('c') if control_held => return true,
        KeyCode::Esc => app.create_issue_modal = None,
        KeyCode::Char('d') | KeyCode::Char('D') if control_held => {
            submit_create_issue_modal(app, command_tx);
        },
        KeyCode::Tab => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.focus_next();
            }
        },
        KeyCode::Enter => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.insert_newline();
            }
        },
        KeyCode::Backspace => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.backspace();
            }
        },
        KeyCode::Left => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.move_left();
            }
        },
        KeyCode::Right => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.move_right();
            }
        },
        KeyCode::Up => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.select_backend_previous();
            }
        },
        KeyCode::Down => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.select_backend_next();
            }
        },
        KeyCode::Home => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.move_home();
            }
        },
        KeyCode::End => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.move_end();
            }
        },
        KeyCode::Char(c) if is_search_char(c, modifiers) => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.insert_char(c);
            }
        },
        _ => {},
    }
    false
}

fn submit_create_issue_modal(
    app: &mut AppState,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(modal) = app.create_issue_modal.take() else {
        return;
    };
    let title = modal.title.trim().to_string();
    if title.is_empty() {
        app.toast_message = Some("title is required".to_string());
        app.toast_until_tick = app.tick.saturating_add(24);
        app.create_issue_modal = Some(modal);
        return;
    }
    let Some(backend) = modal.selected_backend().cloned() else {
        app.toast_message = Some("no issue backend available".to_string());
        app.toast_until_tick = app.tick.saturating_add(24);
        app.create_issue_modal = Some(modal);
        return;
    };
    let _ = command_tx.send(RuntimeCommand::CreateIssue {
        title: title.clone(),
        description: modal.description.trim().to_string(),
        repo_id: backend.repo_id,
        tracker_source: backend.tracker_source,
    });
    let _ = command_tx.send(RuntimeCommand::Refresh);
    app.toast_message = Some(format!("creating issue: {title}"));
    app.toast_until_tick = app.tick.saturating_add(24);
}

fn is_search_char(c: char, modifiers: KeyModifiers) -> bool {
    if modifiers.contains(KeyModifiers::CONTROL)
        || modifiers.contains(KeyModifiers::ALT)
        || modifiers.contains(KeyModifiers::SUPER)
    {
        return false;
    }

    c.is_ascii() && !c.is_ascii_control()
}

fn selected_item(
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) -> Option<polyphony_core::InboxItemRow> {
    display_rows_matching(snapshot, &app.search_query)
        .get(app.selected)
        .and_then(|row| snapshot.inbox_items.get(row.item_idx))
        .cloned()
}

fn dispatch_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    dispatch_item(&item, None, command_tx);
    app.detail_follow_bottom = true;
    record_notice(
        app,
        &item,
        format!("dispatch queued for {}", item.identifier),
    );
}

fn dispatch_item(
    item: &polyphony_core::InboxItemRow,
    directives: Option<String>,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    match item.kind {
        InboxItemKind::Issue => {
            let _ = command_tx.send(RuntimeCommand::DispatchIssue {
                issue_id: item.item_id.clone(),
                agent_name: None,
                directives,
            });
        },
        InboxItemKind::PullRequestReview
        | InboxItemKind::PullRequestComment
        | InboxItemKind::PullRequestConflict => {
            let _ = command_tx.send(RuntimeCommand::DispatchPullRequestInboxItem {
                item_id: item.item_id.clone(),
                directives,
            });
        },
    }
}

fn stop_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    let _ = command_tx.send(RuntimeCommand::StopAgent {
        issue_id: item.item_id.clone(),
    });
    app.detail_follow_bottom = true;
    record_notice(
        app,
        &item,
        format!("stop requested for {}", item.identifier),
    );
}

fn pause_or_resume_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    if issue_has_running_agent(snapshot, &item) {
        let _ = command_tx.send(RuntimeCommand::StopAgent {
            issue_id: item.item_id.clone(),
        });
        record_notice(
            app,
            &item,
            format!("pause requested for {}", item.identifier),
        );
        app.detail_follow_bottom = true;
        return;
    }
    if retry_latest_stopped(snapshot, &item, command_tx) {
        record_notice(app, &item, format!("resume queued for {}", item.identifier));
        app.detail_follow_bottom = true;
        return;
    }
    dispatch_item(&item, None, command_tx);
    record_notice(
        app,
        &item,
        format!("dispatch queued for {}", item.identifier),
    );
    app.detail_follow_bottom = true;
}

fn retry_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    if retry_latest_stopped(snapshot, &item, command_tx) {
        record_notice(app, &item, format!("retry queued for {}", item.identifier));
        app.detail_follow_bottom = true;
    } else {
        record_notice(
            app,
            &item,
            "nothing failed or cancelled to retry".to_string(),
        );
    }
}

fn clear_selected_session(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    let _ = command_tx.send(RuntimeCommand::ClearIssueSession {
        issue_id: item.item_id.clone(),
        issue_identifier: item.identifier.clone(),
    });
    app.interventions
        .retain(|intervention| intervention.issue_id != item.item_id);
    app.notices.retain(|notice| notice.issue_id != item.item_id);
    app.detail_follow_bottom = true;
    app.toast_message = Some(format!("cleared local session for {}", item.identifier));
    app.toast_until_tick = app.tick.saturating_add(24);
}

fn submit_hijack(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let prompt = app.input.trim().to_string();
    if prompt.is_empty() {
        app.detail_input_mode = crate::app::DetailInputMode::None;
        app.input.clear();
        if let Some(item) = selected_item(snapshot, app) {
            record_notice(
                app,
                &item,
                "hijack cancelled: empty intervention".to_string(),
            );
        }
        return;
    }
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };

    if issue_has_running_agent(snapshot, &item) {
        let _ = command_tx.send(RuntimeCommand::StopAgent {
            issue_id: item.item_id.clone(),
        });
    }
    let run_id = latest_run(snapshot, &item).map(|run| run.id.clone());
    if let Some(run_id) = run_id.clone() {
        let _ = command_tx.send(RuntimeCommand::InjectRunFeedback {
            run_id,
            prompt: prompt.clone(),
            agent_name: None,
        });
    } else {
        dispatch_item(&item, Some(prompt.clone()), command_tx);
    }
    app.interventions.push(crate::app::IssueIntervention {
        issue_id: item.item_id.clone(),
        run_id,
        prompt,
    });
    app.detail_input_mode = crate::app::DetailInputMode::None;
    app.input.clear();
    app.detail_follow_bottom = true;
    record_notice(
        app,
        &item,
        format!("intervention queued for {}", item.identifier),
    );
}

fn record_notice(app: &mut AppState, item: &polyphony_core::InboxItemRow, message: String) {
    app.status_message = None;
    app.notices.push(crate::app::IssueNotice {
        issue_id: item.item_id.clone(),
        message,
    });
    if app.notices.len() > 64 {
        app.notices.remove(0);
    }
}

fn issue_has_running_agent(
    snapshot: &RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> bool {
    snapshot
        .running
        .iter()
        .any(|agent| agent.issue_id == item.item_id || agent.issue_identifier == item.identifier)
}

fn retry_latest_stopped(
    snapshot: &RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    if let Some(task) = latest_failed_task(snapshot, item) {
        let _ = command_tx.send(RuntimeCommand::RetryTask {
            run_id: task.run_id.clone(),
            task_id: task.id.clone(),
        });
        return true;
    }
    if let Some(run) = latest_run(snapshot, item)
        .filter(|run| matches!(run.status, RunStatus::Failed | RunStatus::Cancelled))
    {
        let _ = command_tx.send(RuntimeCommand::RetryRun {
            run_id: run.id.clone(),
        });
        return true;
    }
    false
}

fn latest_failed_task<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> Option<&'a polyphony_core::TaskRow> {
    let run_ids = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .map(|run| &run.id)
        .collect::<Vec<_>>();
    snapshot
        .tasks
        .iter()
        .filter(|task| {
            run_ids.contains(&&task.run_id)
                && matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled)
        })
        .max_by_key(|task| task.updated_at)
}

fn latest_run<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> Option<&'a polyphony_core::RunRow> {
    snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .max_by_key(|run| run.created_at)
}

fn handle_command_palette_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Esc if !app.command_query.is_empty() => {
            app.command_query.clear();
            app.command_selected = 0;
            app.command_scroll = 0;
        },
        KeyCode::Esc => app.command_palette_open = false,
        KeyCode::Enter => run_command_palette_selection(app, snapshot, command_tx),
        KeyCode::Up => command_palette::selected_up(app),
        KeyCode::Down => command_palette::selected_down(app),
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.command_palette_open = false;
        },
        KeyCode::Backspace => command_palette::pop_query_char(app),
        KeyCode::Char(c) if is_search_char(c, modifiers) => {
            command_palette::push_query_char(app, c);
        },
        _ => {},
    }
    false
}

fn run_command_palette_selection(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    match command_palette::selected_command(app).map(|command| command.name) {
        Some("Switch dispatch mode") => {
            app.command_palette_open = false;
            dispatch_mode_picker::open(app, snapshot.dispatch_mode);
        },
        Some("Create new issue") => {
            app.command_palette_open = false;
            create_issue_modal::open(app, snapshot);
        },
        Some("Toggle widget timestamps") => {
            app.settings.show_widget_timestamps = !app.settings.show_widget_timestamps;
            let _ = app.settings.save();
            app.toast_message = Some(format!(
                "timestamps {}",
                if app.settings.show_widget_timestamps {
                    "on"
                } else {
                    "off"
                }
            ));
            app.toast_until_tick = app.tick.saturating_add(24);
        },
        Some("Toggle dispatch start/stop") => {
            let mode = match snapshot.dispatch_mode {
                DispatchMode::Stop => DispatchMode::Manual,
                _ => DispatchMode::Stop,
            };
            let _ = command_tx.send(RuntimeCommand::SetMode(mode));
        },
        Some("Refresh trackers") => {
            let _ = command_tx.send(RuntimeCommand::Refresh);
            app.status_message = Some("refresh requested".to_string());
        },
        Some("Stop dispatching") => {
            let _ = command_tx.send(RuntimeCommand::SetMode(DispatchMode::Stop));
        },
        Some("Manual dispatch mode") => {
            let _ = command_tx.send(RuntimeCommand::SetMode(DispatchMode::Manual));
        },
        Some("Switch to nightshift mode") => {
            let _ = command_tx.send(RuntimeCommand::SetMode(DispatchMode::Nightshift));
        },
        _ => {},
    }
    if app.command_palette_open {
        app.command_palette_open = false;
    }
}

fn handle_mouse(app: &mut AppState, snapshot: &RuntimeSnapshot, mouse: event::MouseEvent) {
    app.mouse_pos = Some((mouse.column, mouse.row));

    if app.command_palette_open {
        match mouse.kind {
            MouseEventKind::ScrollDown => command_palette::selected_down(app),
            MouseEventKind::ScrollUp => command_palette::selected_up(app),
            _ => {},
        }
        return;
    }

    if app.create_issue_modal.is_some() {
        return;
    }

    let rows = display_rows_matching(snapshot, &app.search_query);
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            if app.route == Route::Detail {
                app.detail_scroll = app.detail_scroll.saturating_add(DETAIL_MOUSE_SCROLL_ROWS);
                app.detail_follow_bottom = false;
            } else {
                select_next_wrapping(app, rows.len());
            }
        },
        MouseEventKind::ScrollUp => {
            if app.route == Route::Detail {
                app.detail_scroll = app.detail_scroll.saturating_sub(DETAIL_MOUSE_SCROLL_ROWS);
                app.detail_follow_bottom = false;
            } else {
                select_previous_wrapping(app, rows.len());
            }
        },
        MouseEventKind::Down(event::MouseButton::Left)
        | MouseEventKind::Drag(event::MouseButton::Left)
            if app.route == Route::Detail
                && !app.session_selecting
                && !app.sidebar_selecting
                && (app.detail_scrollbar_active
                    || app
                        .detail_scrollbar_rect
                        .contains((mouse.column, mouse.row).into())) =>
        {
            app.detail_scrollbar_active = true;
            app.detail_scroll = scroll_from_scrollbar(app, mouse.row);
            app.detail_follow_bottom = false;
        },
        MouseEventKind::Down(event::MouseButton::Left)
            if app.route == Route::Detail
                && app
                    .session_text_rect
                    .contains((mouse.column, mouse.row).into()) =>
        {
            app.session_selecting = true;
            let position = session_position_at_mouse(app, mouse.column, mouse.row);
            app.session_selection_start = Some(position);
            app.session_selection_end = Some(position);
        },
        MouseEventKind::Down(event::MouseButton::Left)
            if app.route == Route::Detail
                && app
                    .sidebar_text_rect
                    .contains((mouse.column, mouse.row).into()) =>
        {
            app.sidebar_selecting = true;
            let position = sidebar_position_at_mouse(app, mouse.column, mouse.row);
            app.sidebar_selection_start = Some(position);
            app.sidebar_selection_end = Some(position);
        },
        MouseEventKind::Drag(event::MouseButton::Left)
            if app.route == Route::Detail && app.session_selecting =>
        {
            app.session_selection_end =
                Some(session_position_at_mouse(app, mouse.column, mouse.row));
        },
        MouseEventKind::Drag(event::MouseButton::Left)
            if app.route == Route::Detail && app.sidebar_selecting =>
        {
            app.sidebar_selection_end =
                Some(sidebar_position_at_mouse(app, mouse.column, mouse.row));
        },
        MouseEventKind::Up(event::MouseButton::Left) => {
            if app.route == Route::Detail && app.detail_scrollbar_active {
                app.detail_scroll = scroll_from_scrollbar(app, mouse.row);
                app.detail_scrollbar_active = false;
                app.detail_follow_bottom = false;
                return;
            }
            if app.route == Route::Detail && app.session_selecting {
                app.session_selection_end =
                    Some(session_position_at_mouse(app, mouse.column, mouse.row));
                copy_session_selection(app);
                app.session_selecting = false;
                app.session_selection_start = None;
                app.session_selection_end = None;
                return;
            }
            if app.route == Route::Detail && app.sidebar_selecting {
                app.sidebar_selection_end =
                    Some(sidebar_position_at_mouse(app, mouse.column, mouse.row));
                let copied = copy_sidebar_selection(app);
                app.sidebar_selecting = false;
                app.sidebar_selection_start = None;
                app.sidebar_selection_end = None;
                if copied {
                    return;
                }
            }
            if app.route == Route::Detail
                && app
                    .workspace_path_rect
                    .contains((mouse.column, mouse.row).into())
            {
                copy_workspace_path(app);
                return;
            }
            if app.route == Route::Detail
                && app
                    .children_expand_rect
                    .contains((mouse.column, mouse.row).into())
            {
                app.children_expanded = !app.children_expanded;
                return;
            }
            if app.route == Route::Inbox
                && let Some(row) = list_row_at_mouse(app, mouse.column, mouse.row)
            {
                app.selected = row.min(rows.len().saturating_sub(1));
                app.route = Route::Detail;
                app.detail_follow_bottom = true;
                app.children_expanded = false;
            }
        },
        _ => {},
    }
}

fn sidebar_position_at_mouse(app: &AppState, column: u16, row: u16) -> (u16, u16) {
    let area = app.sidebar_text_rect;
    clamp_to_rect((column, row), area)
}

fn session_position_at_mouse(app: &AppState, column: u16, row: u16) -> (u16, u16) {
    let area = app.session_text_rect;
    let (column, row) = clamp_to_rect((column, row), area);
    (
        column.saturating_sub(area.x),
        app.detail_scroll.saturating_add(row.saturating_sub(area.y)),
    )
}

fn copy_session_selection(app: &mut AppState) {
    let Some(text) = selected_session_text(app) else {
        app.session_selection_start = None;
        app.session_selection_end = None;
        return;
    };
    if text.trim().is_empty() {
        app.session_selection_start = None;
        app.session_selection_end = None;
        return;
    }
    match copy_to_clipboard(&text) {
        Ok(()) => {
            app.toast_message = Some("selection copied".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
        Err(_) => {
            app.toast_message = Some("copy failed".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
    }
}

fn copy_full_session(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    let Some(item) = selected_item(snapshot, app) else {
        app.toast_message = Some("no inbox item selected".to_string());
        app.toast_until_tick = app.tick.saturating_add(24);
        return;
    };
    let session = session::build_issue_session(
        snapshot,
        &item,
        true,
        &app.interventions,
        &app.notices,
        app.tick,
    );
    let text = session
        .blocks
        .iter()
        .map(|block| {
            block
                .lines
                .iter()
                .map(line_plain_text)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    match copy_to_clipboard(&text) {
        Ok(()) => {
            app.toast_message = Some("session copied".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
        Err(_) => {
            app.toast_message = Some("copy failed".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
    }
}

fn line_plain_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn copy_sidebar_selection(app: &mut AppState) -> bool {
    let Some(text) = selected_sidebar_text(app) else {
        return false;
    };
    if text.trim().is_empty() {
        return false;
    }
    match copy_to_clipboard(&text) {
        Ok(()) => {
            app.toast_message = Some("selection copied".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
        Err(_) => {
            app.toast_message = Some("copy failed".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
    }
    true
}

fn selected_sidebar_text(app: &AppState) -> Option<String> {
    let mut start = app.sidebar_selection_start?;
    let mut end = app.sidebar_selection_end?;
    let area = app.sidebar_text_rect;
    if area.is_empty() || start == end {
        return None;
    }
    if position_after(start, end) {
        std::mem::swap(&mut start, &mut end);
    }
    let viewport_bottom = area.y.saturating_add(area.height.saturating_sub(1));
    if end.1 < area.y || start.1 > viewport_bottom {
        return None;
    }

    let mut lines = Vec::new();
    let start_row = start.1.max(area.y);
    let end_row = end.1.min(viewport_bottom);
    for screen_row in start_row..=end_row {
        let visible_row = screen_row.saturating_sub(area.y);
        let Some(line) = app.sidebar_visible_lines.get(visible_row as usize) else {
            continue;
        };
        let start_col = if screen_row == start.1 {
            start.0.saturating_sub(area.x)
        } else {
            0
        } as usize;
        let end_col = if screen_row == end.1 {
            end.0.saturating_sub(area.x) as usize
        } else {
            line.chars().count().saturating_sub(1)
        };
        let width = end_col.saturating_sub(start_col).saturating_add(1);
        lines.push(
            line.chars()
                .skip(start_col)
                .take(width)
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
    }
    Some(lines.join("\n"))
}

fn selected_session_text(app: &AppState) -> Option<String> {
    let mut start = app.session_selection_start?;
    let mut end = app.session_selection_end?;
    if app.session_text_rect.is_empty() {
        return None;
    }
    if position_after(start, end) {
        std::mem::swap(&mut start, &mut end);
    }
    let scroll = app.detail_scroll;
    let viewport_bottom = scroll.saturating_add(app.session_text_rect.height.saturating_sub(1));
    if end.1 < scroll || start.1 > viewport_bottom {
        return None;
    }

    let mut lines = Vec::new();
    let start_row = start.1.max(scroll);
    let end_row = end.1.min(viewport_bottom);
    for content_row in start_row..=end_row {
        let visible_row = content_row.saturating_sub(scroll);
        let Some(line) = app.session_visible_lines.get(visible_row as usize) else {
            continue;
        };
        let start_col = if content_row == start.1 {
            start.0
        } else {
            0
        } as usize;
        let end_col = if content_row == end.1 {
            end.0 as usize
        } else {
            line.chars().count().saturating_sub(1)
        };
        let width = end_col.saturating_sub(start_col).saturating_add(1);
        lines.push(
            line.chars()
                .skip(start_col)
                .take(width)
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
    }
    strip_decorative_left_borders(&mut lines);
    Some(lines.join("\n"))
}

fn position_after(a: (u16, u16), b: (u16, u16)) -> bool {
    (a.1, a.0) > (b.1, b.0)
}

fn strip_decorative_left_borders(lines: &mut [String]) {
    if lines.len() <= 1 {
        return;
    }
    for line in lines {
        let trimmed = line.trim_start();
        let Some(first) = trimmed.chars().next() else {
            continue;
        };
        if is_left_border_glyph(first) {
            *line = trimmed[first.len_utf8()..].trim_start().to_string();
        }
    }
}

fn is_left_border_glyph(c: char) -> bool {
    matches!(c, '┃' | '│' | '║' | '▕' | '▏' | '▌' | '▐' | '█' | '|')
}

fn clamp_to_rect((x, y): (u16, u16), area: Rect) -> (u16, u16) {
    let max_x = area.x.saturating_add(area.width.saturating_sub(1));
    let max_y = area.y.saturating_add(area.height.saturating_sub(1));
    (x.clamp(area.x, max_x), y.clamp(area.y, max_y))
}

fn copy_workspace_path(app: &mut AppState) {
    let Some(path) = app.workspace_path_to_copy.as_deref() else {
        return;
    };
    match copy_to_clipboard(path) {
        Ok(()) => {
            app.toast_message = Some("workspace copied".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
        Err(_) => {
            app.toast_message = Some("copy failed".to_string());
            app.toast_until_tick = app.tick.saturating_add(24);
        },
    }
}

fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let commands: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    let mut last_error = None;
    for (program, args) in commands {
        match copy_to_clipboard_with(program, args, text) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("no clipboard command available")))
}

fn copy_to_clipboard_with(program: &str, args: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} failed")))
    }
}

fn scroll_from_scrollbar(app: &AppState, row: u16) -> u16 {
    if app.detail_scrollbar_rect.is_empty() || app.detail_scroll_max == 0 {
        return app.detail_scroll;
    }

    let top = app.detail_scrollbar_rect.y;
    let track_rows = app.detail_scrollbar_rect.height.saturating_sub(1).max(1);
    let row_offset = row.saturating_sub(top).min(track_rows);
    ((u32::from(row_offset) * u32::from(app.detail_scroll_max)) / u32::from(track_rows)) as u16
}

fn list_row_at_mouse(app: &AppState, column: u16, row: u16) -> Option<usize> {
    if !app.list_rect.contains((column, row).into()) || row <= app.list_rect.y {
        return None;
    }
    let visible_row = row.saturating_sub(app.list_rect.y + 1) as usize;
    if visible_row >= app.visible_rows {
        return None;
    }
    Some(app.scroll + visible_row)
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}
