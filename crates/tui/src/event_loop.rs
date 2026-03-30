use futures_util::StreamExt;

use crate::{
    bootstrap::{drain_pending_input, mouse_in_rect},
    prelude::*,
    *,
};

pub async fn run(
    mut snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    log_buffer: LogBuffer,
) -> Result<(), Error> {
    let theme = detect_terminal_theme().unwrap_or_else(default_theme);
    enable_raw_mode()?;
    drain_pending_input();
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = AppState::new(theme, log_buffer);
    let mut snapshot = snapshot_rx.borrow().clone();
    app.on_snapshot(&snapshot);
    refresh_agent_detail_artifact(&mut app, &snapshot).await;

    // Always trigger a fresh fetch on startup so issues appear immediately.
    let _ = command_tx.send(RuntimeCommand::Refresh);

    let mut event_stream = crossterm::event::EventStream::new();
    let mut needs_draw = true;

    let result = loop {
        if needs_draw {
            terminal.draw(|frame| {
                render::render(frame, &snapshot, &mut app);
            })?;
            needs_draw = false;
        }

        if let Some(since) = app.leaving_since
            && since.elapsed() > Duration::from_secs(3)
        {
            break Ok(());
        }

        // Wait for terminal input, snapshot update, or a 1-second tick so
        // live countdowns (e.g. "next poll") stay fresh.
        let terminal_event = tokio::select! {
            biased;
            event = event_stream.next() => {
                match event {
                    Some(Ok(ev)) => Some(ev),
                    Some(Err(_)) => None,
                    None => break Ok(()),
                }
            }
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
                snapshot = snapshot_rx.borrow().clone();
                app.on_snapshot(&snapshot);
                refresh_agent_detail_artifact(&mut app, &snapshot).await;
                needs_draw = true;
                continue;
            }
            _ = tokio::time::sleep(if !snapshot.running.is_empty() || snapshot.loading.any_active() {
                Duration::from_millis(80)
            } else {
                Duration::from_secs(1)
            }) => {
                refresh_live_log_content(&mut app);
                needs_draw = true;
                continue;
            }
        };

        let Some(ev) = terminal_event else {
            continue;
        };

        let mut key_handled = false;
        match ev {
            Event::Mouse(mouse) => {
                if !app.leaving {
                    if app.dispatch_modal.is_some() {
                        if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                            app.dispatch_modal = None;
                        }
                    } else if app.create_issue_modal.is_some() {
                        if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                            app.create_issue_modal = None;
                        }
                    } else if app.feedback_modal.is_some() {
                        if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                            app.feedback_modal = None;
                        }
                    } else if app.has_detail() {
                        // Click outside modal closes it
                        if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                            app.pop_detail();
                        }
                    } else {
                        match mouse.kind {
                            MouseEventKind::Down(event::MouseButton::Left) => {
                                if let Some(tab) = app.tab_at_position(mouse.column, mouse.row) {
                                    app.active_tab = tab;
                                } else if app.active_tab == app::ActiveTab::Inbox {
                                    // Single click selects inbox item row
                                    if let Some(idx) = app.issue_row_at_position(mouse.row) {
                                        app.issues_state.select(Some(idx));
                                    }
                                    // Double-click opens detail modal
                                    let now = Instant::now();
                                    let is_double = app.last_click_at.is_some_and(|prev| {
                                        now.duration_since(prev) < Duration::from_millis(400)
                                            && app.last_click_pos.1 == mouse.row
                                    });
                                    if is_double
                                        && let Some(item) = app.selected_inbox_item(&snapshot)
                                    {
                                        app.push_detail(crate::app::DetailView::InboxItem {
                                            item_id: item.item_id.clone(),
                                            scroll: 0,
                                            focus: Default::default(),
                                            runs_selected: 0,
                                            agents_selected: 0,
                                        });
                                        app.last_click_at = None;
                                    } else {
                                        app.last_click_at = Some(now);
                                        app.last_click_pos = (mouse.column, mouse.row);
                                    }
                                } else if app.active_tab == app::ActiveTab::Tasks {
                                    if let Some(idx) = app.table_row_at_position(mouse.row) {
                                        app.tasks_state.select(Some(idx));
                                    }
                                    let now = Instant::now();
                                    let is_double = app.last_click_at.is_some_and(|prev| {
                                        now.duration_since(prev) < Duration::from_millis(400)
                                            && app.last_click_pos.1 == mouse.row
                                    });
                                    if is_double && let Some(task) = app.selected_task(&snapshot) {
                                        app.push_detail(crate::app::DetailView::Task {
                                            task_id: task.id.clone(),
                                            scroll: 0,
                                        });
                                        app.last_click_at = None;
                                    } else {
                                        app.last_click_at = Some(now);
                                        app.last_click_pos = (mouse.column, mouse.row);
                                    }
                                }
                            },
                            MouseEventKind::ScrollDown => {
                                handle_mouse_scroll(&mut app, &mouse, &snapshot);
                            },
                            MouseEventKind::ScrollUp => {
                                handle_mouse_scroll(&mut app, &mouse, &snapshot);
                            },
                            _ => {},
                        }
                    }
                }
                key_handled = true;
            },
            Event::Key(key) => {
                handle_key_event(&mut app, key, &snapshot, &command_tx);
                key_handled = true;
            },
            _ => {},
        }

        if key_handled {
            // Check if a cast playback was requested before redrawing
            if let Some(playback) = app.pending_cast_playback.take() {
                run_cast_playback(&playback);
            }
            refresh_live_log_content(&mut app);
            needs_draw = true;
            refresh_agent_detail_artifact(&mut app, &snapshot).await;
            // Pick up any snapshot that arrived while handling input.
            if snapshot_rx.has_changed().unwrap_or(false) {
                snapshot = snapshot_rx.borrow().clone();
                app.on_snapshot(&snapshot);
            }
        }
    };

    drain_pending_input();
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

#[derive(Clone)]
enum AgentArtifactRequest {
    Running {
        key: String,
        workspace_path: std::path::PathBuf,
    },
    History {
        key: String,
        workspace_path: std::path::PathBuf,
        issue_id: String,
        started_at: chrono::DateTime<chrono::Utc>,
        agent_name: String,
        attempt: Option<u32>,
    },
}

async fn refresh_agent_detail_artifact(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    let Some(request) = selected_agent_artifact_request(app, snapshot) else {
        // Clear artifact cache on the current agent detail if present
        if let Some(crate::app::DetailView::Agent { artifact_cache, .. }) = app.current_detail_mut()
        {
            **artifact_cache = None;
        }
        return;
    };
    // Check if we already have a matching cache
    let existing_key =
        if let Some(crate::app::DetailView::Agent { artifact_cache, .. }) = app.current_detail() {
            artifact_cache.as_ref().as_ref().map(|a| a.key.clone())
        } else {
            None
        };
    let request_key = agent_artifact_request_key(&request);
    if existing_key.as_deref() == Some(&request_key) {
        return;
    }
    let loaded = tokio::task::spawn_blocking(move || load_agent_artifact(request))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    if let Some(crate::app::DetailView::Agent { artifact_cache, .. }) = app.current_detail_mut() {
        **artifact_cache = Some(crate::app::AgentDetailArtifactCache {
            key: request_key,
            saved_context: loaded,
        });
    }
}

fn handle_key_event(
    app: &mut AppState,
    key: event::KeyEvent,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    if app.leaving {
    } else if app.show_agent_picker {
        match key.code {
            KeyCode::Esc => {
                app.show_agent_picker = false;
                app.agent_picker_issue_id = None;
            },
            KeyCode::Char('j') | KeyCode::Down => {
                let count = snapshot.agent_profiles.len();
                if count > 0 {
                    app.agent_picker_selected = (app.agent_picker_selected + 1) % count;
                }
            },
            KeyCode::Char('k') | KeyCode::Up => {
                let count = snapshot.agent_profiles.len();
                if count > 0 {
                    app.agent_picker_selected = (app.agent_picker_selected + count - 1) % count;
                }
            },
            KeyCode::Enter => {
                if let Some(issue_id) = app.agent_picker_issue_id.take() {
                    let agent_name = snapshot
                        .agent_profiles
                        .get(app.agent_picker_selected)
                        .map(|p| p.name.clone());
                    app.show_agent_picker = false;
                    if let Some(item) = find_inbox_item_by_id(snapshot, &issue_id) {
                        let _ = start_dispatch_for_inbox_item(app, item, agent_name);
                    }
                }
            },
            _ => {},
        }
    } else if app.dispatch_modal.is_some() {
        if let Some(command) = handle_dispatch_modal_key(app, key) {
            tracing::info!(?command, "TUI sending command");
            let _ = command_tx.send(command);
        }
    } else if app.create_issue_modal.is_some() {
        if let Some(command) = handle_create_issue_modal_key(app, key) {
            tracing::info!(?command, "TUI sending command");
            let _ = command_tx.send(command);
        }
    } else if app.feedback_modal.is_some() {
        if let Some(command) = handle_feedback_modal_key(app, key, snapshot) {
            tracing::info!(?command, "TUI sending command");
            let _ = command_tx.send(command);
        }
    } else if app.confirm_quit {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                app.confirm_quit = false;
                let _ = command_tx.send(RuntimeCommand::Shutdown);
                app.leaving = true;
                app.leaving_since = Some(Instant::now());
            },
            _ => {
                app.confirm_quit = false;
            },
        }
    } else if app.show_help_modal {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                app.show_help_modal = false;
            },
            _ => {},
        }
    } else if app.show_mode_modal {
        match key.code {
            KeyCode::Esc => {
                app.show_mode_modal = false;
            },
            KeyCode::Char('j') | KeyCode::Down => {
                app.mode_modal_selected = (app.mode_modal_selected + 1) % 5;
            },
            KeyCode::Char('k') | KeyCode::Up => {
                app.mode_modal_selected = (app.mode_modal_selected + 4) % 5;
            },
            KeyCode::Enter => {
                let modes = [
                    DispatchMode::Manual,
                    DispatchMode::Automatic,
                    DispatchMode::Nightshift,
                    DispatchMode::Idle,
                    DispatchMode::Stop,
                ];
                let selected = modes[app.mode_modal_selected];
                app.show_mode_modal = false;
                let _ = command_tx.send(RuntimeCommand::SetMode(selected));
            },
            _ => {},
        }
    } else if app.has_detail() && app.split_focus == crate::app::SplitFocus::Detail {
        if let Some(cmd) = handle_detail_key(app, key.code, snapshot, command_tx) {
            let _ = command_tx.send(cmd);
        }
    } else if app.has_detail() && app.split_focus == crate::app::SplitFocus::List {
        match key.code {
            KeyCode::Tab => {
                app.split_focus = crate::app::SplitFocus::Detail;
            },
            KeyCode::Esc => {
                app.pop_detail();
                app.split_focus = crate::app::SplitFocus::default();
            },
            KeyCode::Enter => {},
            KeyCode::Char('e')
            | KeyCode::Char('E')
            | KeyCode::Char('c')
            | KeyCode::Char('f')
            | KeyCode::Char('w')
            | KeyCode::Char('O') => {
                if let Some(cmd) = handle_detail_key(app, key.code, snapshot, command_tx) {
                    let _ = command_tx.send(cmd);
                }
            },
            _ => {
                if let Some(command) = handle_key(app, key.code, snapshot) {
                    let shutdown = matches!(command, RuntimeCommand::Shutdown);
                    if matches!(command, RuntimeCommand::Refresh) {
                        app.refresh_requested = true;
                    }
                    tracing::info!(?command, "TUI sending command");
                    let _ = command_tx.send(command);
                    if shutdown {
                        app.leaving = true;
                        app.leaving_since = Some(Instant::now());
                    }
                }
                update_split_detail_from_selection(app, snapshot);
            },
        }
    } else if app.search_active {
        match key.code {
            KeyCode::Esc => {
                app.search_active = false;
                app.search_query.clear();
                app.rebuild_sorted_indices(snapshot);
                sync_selection_after_search(app, snapshot);
            },
            KeyCode::Enter => {
                app.search_active = false;
            },
            KeyCode::Backspace => {
                app.search_query.pop();
                app.rebuild_sorted_indices(snapshot);
                sync_selection_after_search(app, snapshot);
            },
            KeyCode::Char(c) => {
                app.search_query.push(c);
                app.rebuild_sorted_indices(snapshot);
                sync_selection_after_search(app, snapshot);
            },
            _ => {},
        }
    } else if app.runs_search_active {
        match key.code {
            KeyCode::Esc => {
                app.runs_search_active = false;
                app.runs_search_query.clear();
            },
            KeyCode::Enter => {
                app.runs_search_active = false;
            },
            KeyCode::Backspace => {
                app.runs_search_query.pop();
            },
            KeyCode::Char(c) => {
                app.runs_search_query.push(c);
            },
            _ => {},
        }
    } else if app.logs_search_active {
        match key.code {
            KeyCode::Esc => {
                app.logs_search_active = false;
                app.logs_search_query.clear();
            },
            KeyCode::Enter => {
                app.logs_search_active = false;
            },
            KeyCode::Backspace => {
                app.logs_search_query.pop();
            },
            KeyCode::Char(c) => {
                app.logs_search_query.push(c);
            },
            _ => {},
        }
    } else if let Some(command) = handle_key(app, key.code, snapshot) {
        let shutdown = matches!(command, RuntimeCommand::Shutdown);
        if matches!(command, RuntimeCommand::Refresh) {
            app.refresh_requested = true;
        }
        tracing::info!(?command, "TUI sending command");
        let _ = command_tx.send(command);
        if shutdown {
            app.leaving = true;
            app.leaving_since = Some(Instant::now());
        }
    }
}

fn selected_agent_artifact_request(
    app: &AppState,
    snapshot: &RuntimeSnapshot,
) -> Option<AgentArtifactRequest> {
    // Only load artifacts when viewing an Agent detail
    let agent_index = match app.current_detail() {
        Some(crate::app::DetailView::Agent { agent_index, .. }) => *agent_index,
        _ => return None,
    };
    let agent = if let Some(running) = snapshot.running.get(agent_index) {
        crate::app::SelectedAgentRow::Running(running)
    } else {
        snapshot
            .agent_run_history
            .get(agent_index.saturating_sub(snapshot.running.len()))
            .map(crate::app::SelectedAgentRow::History)?
    };
    match agent {
        crate::app::SelectedAgentRow::Running(agent) => Some(AgentArtifactRequest::Running {
            key: format!(
                "running:{}:{}:{}",
                agent.issue_id,
                agent.started_at.to_rfc3339(),
                agent
                    .last_event_at
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_default()
            ),
            workspace_path: agent.workspace_path.clone(),
        }),
        crate::app::SelectedAgentRow::History(agent) => {
            let workspace_path = agent.workspace_path.clone()?;
            Some(AgentArtifactRequest::History {
                key: format!(
                    "history:{}:{}:{}:{}",
                    agent.issue_id,
                    agent.agent_name,
                    agent.attempt.unwrap_or_default(),
                    agent.started_at.to_rfc3339()
                ),
                workspace_path,
                issue_id: agent.issue_id.clone(),
                started_at: agent.started_at,
                agent_name: agent.agent_name.clone(),
                attempt: agent.attempt,
            })
        },
    }
}

fn agent_artifact_request_key(request: &AgentArtifactRequest) -> String {
    match request {
        AgentArtifactRequest::Running { key, .. } | AgentArtifactRequest::History { key, .. } => {
            key.clone()
        },
    }
}

fn load_agent_artifact(
    request: AgentArtifactRequest,
) -> Result<Option<polyphony_core::AgentContextSnapshot>, polyphony_core::Error> {
    match request {
        AgentArtifactRequest::Running { workspace_path, .. } => {
            polyphony_core::load_workspace_saved_context_artifact(&workspace_path)
        },
        AgentArtifactRequest::History {
            workspace_path,
            issue_id,
            started_at,
            agent_name,
            attempt,
            ..
        } => Ok(polyphony_core::load_workspace_agent_run_history_record(
            &workspace_path,
            &issue_id,
            started_at,
            &agent_name,
            attempt,
        )?
        .and_then(|run| run.saved_context)),
    }
}

fn handle_key(
    app: &mut AppState,
    key: KeyCode,
    snapshot: &RuntimeSnapshot,
) -> Option<RuntimeCommand> {
    match key {
        KeyCode::Char('q') => {
            app.confirm_quit = true;
            return None;
        },
        KeyCode::Char('r') => {
            app.show_toast("Refreshing".to_string(), None);
            return Some(RuntimeCommand::Refresh);
        },

        // Tab switching
        KeyCode::Tab | KeyCode::Right => {
            app.clear_detail_stack();
            app.active_tab = app.active_tab.next();
        },
        KeyCode::BackTab | KeyCode::Left => {
            app.clear_detail_stack();
            app.active_tab = app.active_tab.previous();
        },
        KeyCode::Char('1') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Inbox;
        },
        KeyCode::Char('2') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Orchestrator;
        },
        KeyCode::Char('3') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Tasks;
        },
        KeyCode::Char('4') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Deliverables;
        },
        KeyCode::Char('5') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Agents;
        },
        KeyCode::Char('6') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Repos;
        },
        KeyCode::Char('7') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Logs;
        },
        KeyCode::Char('\\') => {
            // Cycle through repo filter: All → repo1 → repo2 → ... → All
            let repo_ids = &snapshot.repo_ids;
            if repo_ids.is_empty() {
                app.selected_repo = None;
            } else {
                match &app.selected_repo {
                    None => {
                        app.selected_repo = Some(repo_ids[0].clone());
                        app.show_toast(format!("Filter: {}", repo_ids[0]), None);
                    },
                    Some(current) => {
                        let idx = repo_ids.iter().position(|id| id == current);
                        let next = idx.map(|i| i + 1).unwrap_or(0);
                        if next < repo_ids.len() {
                            app.selected_repo = Some(repo_ids[next].clone());
                            app.show_toast(format!("Filter: {}", repo_ids[next]), None);
                        } else {
                            app.selected_repo = None;
                            app.show_toast("Filter: All repos".to_string(), None);
                        }
                    },
                }
            }
        },
        KeyCode::Char('J') => {},
        KeyCode::Char('K') => {},

        // Navigation (works on active tab's table)
        KeyCode::Char('j') | KeyCode::Down => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = false;
            }
            let len = app.active_table_len(snapshot);
            app.move_down(len, 1);
        },
        KeyCode::Char('k') | KeyCode::Up => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = false;
            }
            let len = app.active_table_len(snapshot);
            app.move_up(len, 1);
        },
        KeyCode::PageDown => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = false;
            }
            let len = app.active_table_len(snapshot);
            app.move_down(len, 8);
        },
        KeyCode::PageUp => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = false;
            }
            let len = app.active_table_len(snapshot);
            app.move_up(len, 8);
        },

        // Jump to bottom
        KeyCode::Char('G') | KeyCode::End => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = true;
            }
            let len = app.active_table_len(snapshot);
            if len > 0 {
                app.move_down(len, len);
            }
        },

        // Jump to top
        KeyCode::Char('g') | KeyCode::Home => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = false;
            }
            let len = app.active_table_len(snapshot);
            if len > 0 {
                app.move_up(len, len);
            }
        },

        // Toggle collapse on run rows (Orchestrator tab)
        KeyCode::Char(' ') => {
            if app.active_tab == app::ActiveTab::Orchestrator
                && let Some(app::OrchestratorTreeRow::Run { snapshot_index }) =
                    app.selected_orchestrator_row().cloned()
            {
                let run = &snapshot.runs[snapshot_index];
                app.toggle_run_collapse(&run.id.clone());
                app.rebuild_orchestrator_tree(snapshot);
            }
        },

        // Sort cycling
        KeyCode::Char('s') => {
            if app.active_tab == app::ActiveTab::Inbox {
                app.issue_sort = app.issue_sort.cycle();
                app.rebuild_sorted_indices(snapshot);
            } else if app.active_tab == app::ActiveTab::Orchestrator {
                app.run_sort = app.run_sort.cycle();
                app.on_snapshot(snapshot);
            }
        },

        // Detail view (Enter opens for active tab)
        KeyCode::Enter => {
            if app.active_tab == app::ActiveTab::Inbox
                && let Some(item) = app.selected_inbox_item(snapshot)
            {
                app.push_detail(crate::app::DetailView::InboxItem {
                    item_id: item.item_id.clone(),
                    scroll: 0,
                    focus: Default::default(),
                    runs_selected: 0,
                    agents_selected: 0,
                });
            } else if app.active_tab == app::ActiveTab::Tasks
                && let Some(task) = app.selected_task(snapshot)
            {
                app.push_detail(crate::app::DetailView::Task {
                    task_id: task.id.clone(),
                    scroll: 0,
                });
            } else if app.active_tab == app::ActiveTab::Orchestrator {
                match app.selected_orchestrator_row().cloned() {
                    Some(app::OrchestratorTreeRow::Run { snapshot_index }) => {
                        let run = &snapshot.runs[snapshot_index];
                        app.push_detail(crate::app::DetailView::Run {
                            run_id: run.id.clone(),
                            scroll: 0,
                        });
                    },
                    Some(app::OrchestratorTreeRow::InboxItem { item_index, .. }) => {
                        let item = &snapshot.inbox_items[item_index];
                        app.push_detail(crate::app::DetailView::InboxItem {
                            item_id: item.item_id.clone(),
                            scroll: 0,
                            focus: Default::default(),
                            runs_selected: 0,
                            agents_selected: 0,
                        });
                    },
                    Some(app::OrchestratorTreeRow::Task { snapshot_index, .. }) => {
                        let task = &snapshot.tasks[snapshot_index];
                        app.push_detail(crate::app::DetailView::Task {
                            task_id: task.id.clone(),
                            scroll: 0,
                        });
                    },
                    Some(app::OrchestratorTreeRow::AgentSession { history_index, .. }) => {
                        // Map history_index to a sorted agent display index
                        let display_index = app
                            .sorted_agent_indices
                            .iter()
                            .position(|&(is_running, idx)| !is_running && idx == history_index);
                        if let Some(display_idx) = display_index {
                            app.push_detail(crate::app::DetailView::Agent {
                                agent_index: display_idx,
                                scroll: u16::MAX,
                                artifact_cache: Box::new(None),
                            });
                        }
                    },
                    Some(app::OrchestratorTreeRow::RunningAgent { running_index, .. }) => {
                        // Map running_index to a sorted agent display index
                        let display_index = app
                            .sorted_agent_indices
                            .iter()
                            .position(|&(is_running, idx)| is_running && idx == running_index);
                        if let Some(display_idx) = display_index {
                            app.push_detail(crate::app::DetailView::Agent {
                                agent_index: display_idx,
                                scroll: u16::MAX,
                                artifact_cache: Box::new(None),
                            });
                        }
                    },
                    Some(app::OrchestratorTreeRow::Outcome { run_snapshot_index }) => {
                        let run = &snapshot.runs[run_snapshot_index];
                        app.push_detail(crate::app::DetailView::Deliverable {
                            run_id: run.id.clone(),
                            scroll: 0,
                        });
                    },
                    Some(app::OrchestratorTreeRow::LogEntry {
                        run_snapshot_index, ..
                    }) => {
                        let run = &snapshot.runs[run_snapshot_index];
                        app.push_detail(crate::app::DetailView::Run {
                            run_id: run.id.clone(),
                            scroll: 0,
                        });
                    },
                    Some(app::OrchestratorTreeRow::AgentLogLine { running_index, .. }) => {
                        // Open the running agent detail
                        let display_index = app
                            .sorted_agent_indices
                            .iter()
                            .position(|&(is_running, idx)| is_running && idx == running_index);
                        if let Some(display_idx) = display_index {
                            app.push_detail(crate::app::DetailView::Agent {
                                agent_index: display_idx,
                                scroll: u16::MAX,
                                artifact_cache: Box::new(None),
                            });
                        }
                    },
                    Some(app::OrchestratorTreeRow::Progress {
                        run_snapshot_index, ..
                    }) => {
                        let run = &snapshot.runs[run_snapshot_index];
                        app.push_detail(crate::app::DetailView::Run {
                            run_id: run.id.clone(),
                            scroll: 0,
                        });
                    },
                    None => {},
                }
            } else if app.active_tab == app::ActiveTab::Deliverables
                && let Some(run) = app.selected_deliverable(snapshot)
            {
                app.push_detail(crate::app::DetailView::Deliverable {
                    run_id: run.id.clone(),
                    scroll: 0,
                });
            } else if app.active_tab == app::ActiveTab::Agents
                && let Some(index) = app.agents_state.selected()
            {
                app.push_detail(crate::app::DetailView::Agent {
                    agent_index: index,
                    scroll: u16::MAX,
                    artifact_cache: Box::new(None),
                });
            }
        },

        // Open item in browser
        KeyCode::Char('o') => {
            if app.active_tab == app::ActiveTab::Inbox
                && let Some(item) = app.selected_inbox_item(snapshot)
                && let Some(url) = &item.url
            {
                open_url(url);
            }
        },
        KeyCode::Char('O') => {
            let url = match app.active_tab {
                app::ActiveTab::Deliverables => app
                    .selected_deliverable(snapshot)
                    .and_then(|run| run.deliverable.as_ref())
                    .and_then(|deliverable| deliverable.url.as_deref()),
                app::ActiveTab::Orchestrator => app.selected_run(snapshot).and_then(|run| {
                    run.deliverable
                        .as_ref()
                        .and_then(|deliverable| deliverable.url.as_deref())
                        .or_else(|| {
                            run.review_target
                                .as_ref()
                                .and_then(|target| target.url.as_deref())
                        })
                }),
                _ => None,
            };
            if let Some(url) = url {
                open_url(url);
            }
        },

        // Search
        KeyCode::Char('/') => {
            if app.active_tab == app::ActiveTab::Inbox {
                app.search_active = true;
                app.search_query.clear();
            } else if app.active_tab == app::ActiveTab::Orchestrator {
                app.runs_search_active = true;
                app.runs_search_query.clear();
            } else if app.active_tab == app::ActiveTab::Logs {
                app.logs_search_active = true;
                app.logs_search_query.clear();
            }
        },

        // Dispatch selected item
        KeyCode::Char('n') => {
            if app.active_tab == app::ActiveTab::Inbox {
                app.create_issue_modal = Some(crate::app::CreateIssueModalState::new());
                return None;
            }
        },
        KeyCode::Char('d') => {
            if app.active_tab == app::ActiveTab::Inbox
                && let Some(item) = app.selected_inbox_item(snapshot)
            {
                return start_dispatch_for_inbox_item(app, item, None);
            }
        },
        KeyCode::Char('a') => {
            if app.active_tab == app::ActiveTab::Inbox
                && let Some(item) = app.selected_inbox_item(snapshot)
                && item.approval_state == polyphony_core::DispatchApprovalState::Waiting
            {
                app.show_toast(format!("Approving {}", item.identifier), None);
                return Some(RuntimeCommand::ApproveInboxItem {
                    item_id: item.item_id.clone(),
                    source: item.source.clone(),
                });
            }
            if let Some(run) = selected_resolvable_deliverable_run(app, snapshot) {
                let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                app.show_toast(format!("Accepting & merging {label}"), None);
                return Some(RuntimeCommand::ResolveRunDeliverable {
                    run_id: run.id.clone(),
                    decision: polyphony_core::DeliverableDecision::Accepted,
                });
            }
        },

        // Merge deliverable (local branch or PR)
        KeyCode::Char('M') => {
            if let Some(run) = selected_deliverable_run(app, snapshot)
                && run.deliverable.is_some()
            {
                let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                app.show_toast(format!("Merging {label}"), None);
                return Some(RuntimeCommand::MergeDeliverable {
                    run_id: run.id.clone(),
                });
            }
            if let Some(app::DetailView::Run { run_id, .. }) = app.current_detail()
                && let Some(run) = find_run_by_id(snapshot, run_id)
                && run.deliverable.is_some()
            {
                let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                app.show_toast(format!("Merging {label}"), None);
                return Some(RuntimeCommand::MergeDeliverable {
                    run_id: run.id.clone(),
                });
            }
        },

        // Dispatch issue (pick agent)
        KeyCode::Char('D') => {
            if app.active_tab == app::ActiveTab::Inbox
                && let Some(issue) = app.selected_inbox_item(snapshot)
                && issue.kind == InboxItemKind::Issue
                && !snapshot.agent_profiles.is_empty()
            {
                app.show_agent_picker = true;
                app.agent_picker_selected = 0;
                app.agent_picker_issue_id = Some(issue.item_id.clone());
            }
        },

        // Mode modal
        KeyCode::Char('m') => {
            app.show_mode_modal = true;
            // Pre-select current mode
            app.mode_modal_selected = match snapshot.dispatch_mode {
                DispatchMode::Manual => 0,
                DispatchMode::Automatic => 1,
                DispatchMode::Nightshift => 2,
                DispatchMode::Idle => 3,
                DispatchMode::Stop => 4,
            };
        },
        KeyCode::Char('x') => {
            if app.active_tab == app::ActiveTab::Inbox
                && let Some(item) = app.selected_inbox_item(snapshot)
                && inbox_item_can_close_issue(snapshot, item)
            {
                app.show_toast(format!("Closing {}", item.identifier), None);
                return Some(RuntimeCommand::CloseTrackerIssue {
                    issue_id: item.item_id.clone(),
                });
            }
            if let Some(run) = selected_resolvable_deliverable_run(app, snapshot) {
                let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                app.show_toast(format!("Rejecting {label}"), None);
                return Some(RuntimeCommand::ResolveRunDeliverable {
                    run_id: run.id.clone(),
                    decision: polyphony_core::DeliverableDecision::Rejected,
                });
            }
        },

        // Open terminal at workspace
        KeyCode::Char('w') => {
            let workspace = match app.active_tab {
                app::ActiveTab::Orchestrator => app
                    .selected_run(snapshot)
                    .and_then(|m| m.workspace_path.clone()),
                app::ActiveTab::Agents => match app.selected_agent(snapshot) {
                    Some(app::SelectedAgentRow::Running(r)) => Some(r.workspace_path.clone()),
                    Some(app::SelectedAgentRow::History(h)) => h.workspace_path.clone(),
                    None => None,
                },
                _ => None,
            };
            if let Some(ws) = workspace {
                let ws = if ws.is_relative() {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(&ws))
                        .unwrap_or(ws)
                } else {
                    ws
                };
                open_terminal_at(&ws);
            }
        },

        // Play asciicast recording for selected agent
        KeyCode::Char('c') => {
            if app.active_tab == app::ActiveTab::Agents
                || app.active_tab == app::ActiveTab::Orchestrator
            {
                request_cast_playback(app, snapshot);
            }
        },

        // Stop a running agent
        KeyCode::Char('S') => {
            if let Some(issue_id) = selected_running_agent_issue_id(app, snapshot) {
                let identifier = snapshot
                    .running
                    .iter()
                    .find(|r| r.issue_id == issue_id)
                    .map(|r| r.issue_identifier.as_str())
                    .unwrap_or(&issue_id);
                app.show_toast(format!("Stopping agent on {identifier}"), None);
                return Some(RuntimeCommand::StopAgent { issue_id });
            }
        },

        // Inject feedback into a run from the Orchestration tab
        KeyCode::Char('f') => {
            if app.active_tab == app::ActiveTab::Orchestrator
                && let Some(row) = app.selected_orchestrator_row().cloned()
            {
                let run = match row {
                    app::OrchestratorTreeRow::Run { snapshot_index, .. }
                    | app::OrchestratorTreeRow::InboxItem {
                        run_snapshot_index: snapshot_index,
                        ..
                    }
                    | app::OrchestratorTreeRow::Progress {
                        run_snapshot_index: snapshot_index,
                        ..
                    }
                    | app::OrchestratorTreeRow::Outcome {
                        run_snapshot_index: snapshot_index,
                        ..
                    }
                    | app::OrchestratorTreeRow::LogEntry {
                        run_snapshot_index: snapshot_index,
                        ..
                    } => snapshot.runs.get(snapshot_index),
                    app::OrchestratorTreeRow::Task { snapshot_index, .. } => {
                        let task = &snapshot.tasks[snapshot_index];
                        snapshot.runs.iter().find(|m| m.id == task.run_id)
                    },
                    app::OrchestratorTreeRow::AgentSession { history_index, .. } => {
                        let session = &snapshot.agent_run_history[history_index];
                        snapshot.runs.iter().find(|m| {
                            m.issue_identifier.as_deref() == Some(&session.issue_identifier)
                        })
                    },
                    app::OrchestratorTreeRow::RunningAgent { running_index, .. }
                    | app::OrchestratorTreeRow::AgentLogLine { running_index, .. } => {
                        let running = &snapshot.running[running_index];
                        snapshot.runs.iter().find(|m| {
                            m.issue_identifier.as_deref() == Some(running.issue_identifier.as_str())
                        })
                    },
                };
                if let Some(run) = run
                    && run.workspace_path.is_some()
                {
                    app.feedback_modal = Some(crate::app::FeedbackModalState::new(
                        run.id.clone(),
                        run.title.clone(),
                    ));
                    return None;
                }
            }
        },

        // Retry failed run from its first failed task
        KeyCode::Char('t') => {
            if app.active_tab == app::ActiveTab::Orchestrator
                && let Some(row) = app.selected_orchestrator_row().cloned()
            {
                match row {
                    app::OrchestratorTreeRow::Run { snapshot_index, .. } => {
                        let run = &snapshot.runs[snapshot_index];
                        if run_can_retry(snapshot, &run.id) {
                            let label = run.issue_identifier.as_deref().unwrap_or(&run.title);
                            app.show_toast(format!("Retrying {label}"), None);
                            return Some(RuntimeCommand::RetryRun {
                                run_id: run.id.clone(),
                            });
                        }
                    },
                    app::OrchestratorTreeRow::Task { snapshot_index, .. } => {
                        let task = &snapshot.tasks[snapshot_index];
                        if task.status != polyphony_core::TaskStatus::Completed
                            && run_can_retry(snapshot, &task.run_id)
                        {
                            app.show_toast(format!("Retrying: {}", task.title), None);
                            return Some(RuntimeCommand::RetryRun {
                                run_id: task.run_id.clone(),
                            });
                        }
                    },
                    _ => {},
                }
            }
        },

        // Resolve task (mark as completed, resume pipeline)
        KeyCode::Char('R') => {
            if app.active_tab == app::ActiveTab::Orchestrator
                && let Some(app::OrchestratorTreeRow::Task { snapshot_index, .. }) =
                    app.selected_orchestrator_row().cloned()
            {
                let task = &snapshot.tasks[snapshot_index];
                if matches!(
                    task.status,
                    polyphony_core::TaskStatus::Failed | polyphony_core::TaskStatus::InProgress
                ) {
                    app.show_toast(format!("Resolved: {}", task.title), None);
                    return Some(RuntimeCommand::ResolveTask {
                        run_id: task.run_id.clone(),
                        task_id: task.id.clone(),
                    });
                }
            }
        },

        // Help modal
        KeyCode::Char('?') => {
            app.show_help_modal = true;
        },

        // Clear search filter
        KeyCode::Esc => {
            if !app.search_query.is_empty() {
                app.search_query.clear();
                app.rebuild_sorted_indices(snapshot);
                sync_selection_after_search(app, snapshot);
            } else if !app.logs_search_query.is_empty() {
                app.logs_search_query.clear();
            }
        },

        _ => {},
    }
    None
}

/// Handle key events when a detail view is on the stack.
/// Returns an optional command to send.
fn handle_detail_key(
    app: &mut AppState,
    key: KeyCode,
    snapshot: &RuntimeSnapshot,
    _command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> Option<RuntimeCommand> {
    // In split mode, Esc switches focus to list; a second Esc closes the detail
    let in_split = app.is_split_eligible() && app.split_focus == crate::app::SplitFocus::Detail;
    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            if in_split {
                app.split_focus = crate::app::SplitFocus::List;
            } else {
                app.pop_detail();
            }
            return None;
        },
        KeyCode::Home | KeyCode::Char('g') => {
            *app.current_detail_mut()?.scroll_mut() = 0;
            return None;
        },
        KeyCode::End | KeyCode::Char('G') => {
            *app.current_detail_mut()?.scroll_mut() = u16::MAX;
            return None;
        },
        _ => {},
    }

    // Dispatch based on current detail variant
    let detail = app.current_detail().cloned()?;

    match detail {
        crate::app::DetailView::InboxItem { ref item_id, .. } => match key {
            KeyCode::Tab if in_split => {
                app.split_focus = crate::app::SplitFocus::List;
            },
            KeyCode::Enter => {
                // Open the single run detail if one exists
                if let Some(item) = find_inbox_item_by_id(snapshot, item_id)
                    && let Some(run) = snapshot
                        .runs
                        .iter()
                        .find(|m| m.issue_identifier.as_deref() == Some(&*item.identifier))
                {
                    app.push_detail(crate::app::DetailView::Run {
                        run_id: run.id.clone(),
                        scroll: 0,
                    });
                }
            },
            KeyCode::Char('j') | KeyCode::Down => {
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                scroll_detail_back(app, 8);
            },
            KeyCode::Char('o') => {
                if let Some(item) = find_inbox_item_by_id(snapshot, item_id)
                    && let Some(url) = &item.url
                {
                    open_url(url);
                }
            },
            KeyCode::Char('a') => {
                if let Some(item) = find_inbox_item_by_id(snapshot, item_id)
                    && item.approval_state == polyphony_core::DispatchApprovalState::Waiting
                {
                    app.show_toast(format!("Approving {}", item.identifier), None);
                    return Some(RuntimeCommand::ApproveInboxItem {
                        item_id: item.item_id.clone(),
                        source: item.source.clone(),
                    });
                }
            },
            KeyCode::Char('x') => {
                if let Some(item) = find_inbox_item_by_id(snapshot, item_id)
                    && inbox_item_can_close_issue(snapshot, item)
                {
                    app.show_toast(format!("Closing {}", item.identifier), None);
                    return Some(RuntimeCommand::CloseTrackerIssue {
                        issue_id: item.item_id.clone(),
                    });
                }
            },
            KeyCode::Char('d') => {
                if let Some(item) = find_inbox_item_by_id(snapshot, item_id) {
                    return start_dispatch_for_inbox_item(app, item, None);
                }
            },
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if let Some(item) = find_inbox_item_by_id(snapshot, item_id) {
                    app.push_detail(crate::app::DetailView::Events {
                        filter: item.identifier.clone(),
                        scroll: u16::MAX,
                    });
                }
            },
            _ => {},
        },
        crate::app::DetailView::Run { ref run_id, .. } => match key {
            KeyCode::Tab if in_split => {
                app.split_focus = crate::app::SplitFocus::List;
            },
            KeyCode::Enter => {},
            KeyCode::Char('j') | KeyCode::Down => {
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                scroll_detail_back(app, 8);
            },
            KeyCode::Char('O') => {
                if let Some(run) = find_run_by_id(snapshot, run_id) {
                    let url = run
                        .deliverable
                        .as_ref()
                        .and_then(|d| d.url.as_deref())
                        .or_else(|| run.review_target.as_ref().and_then(|t| t.url.as_deref()));
                    if let Some(url) = url {
                        open_url(url);
                    }
                }
            },
            KeyCode::Char('t') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && run_can_retry(snapshot, &run.id)
                {
                    let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                    app.show_toast(format!("Retrying {label}"), None);
                    return Some(RuntimeCommand::RetryRun {
                        run_id: run.id.clone(),
                    });
                }
            },
            KeyCode::Char('a') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && run_can_resolve_deliverable(run)
                {
                    let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                    app.show_toast(format!("Accepting & merging {label}"), None);
                    return Some(RuntimeCommand::ResolveRunDeliverable {
                        run_id: run.id.clone(),
                        decision: polyphony_core::DeliverableDecision::Accepted,
                    });
                }
            },
            KeyCode::Char('x') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && run_can_resolve_deliverable(run)
                {
                    let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                    app.show_toast(format!("Rejecting {label}"), None);
                    return Some(RuntimeCommand::ResolveRunDeliverable {
                        run_id: run.id.clone(),
                        decision: polyphony_core::DeliverableDecision::Rejected,
                    });
                }
            },
            KeyCode::Char('f') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && run.workspace_path.is_some()
                {
                    app.feedback_modal = Some(crate::app::FeedbackModalState::new(
                        run.id.clone(),
                        run.title.clone(),
                    ));
                }
            },
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if let Some(run) = find_run_by_id(snapshot, run_id) {
                    let filter = run
                        .issue_identifier
                        .clone()
                        .unwrap_or_else(|| run.id.clone());
                    app.push_detail(crate::app::DetailView::Events {
                        filter,
                        scroll: u16::MAX,
                    });
                }
            },
            KeyCode::Char('w') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && let Some(ws) = &run.workspace_path
                {
                    let ws = if ws.is_relative() {
                        std::env::current_dir()
                            .map(|cwd| cwd.join(ws))
                            .unwrap_or_else(|_| ws.clone())
                    } else {
                        ws.clone()
                    };
                    open_terminal_at(&ws);
                }
            },
            _ => {},
        },
        crate::app::DetailView::Task { ref task_id, .. } => match key {
            KeyCode::Tab if in_split => {
                app.split_focus = crate::app::SplitFocus::List;
            },
            KeyCode::Enter => {},
            KeyCode::Char('c') => {
                request_cast_playback_from_detail(app, snapshot);
            },
            KeyCode::Char('j') | KeyCode::Down => {
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                scroll_detail_back(app, 8);
            },
            KeyCode::Char('t') => {
                if let Some(task) = snapshot.tasks.iter().find(|task| task.id == *task_id)
                    && task.status != polyphony_core::TaskStatus::Completed
                    && run_can_retry(snapshot, &task.run_id)
                {
                    app.show_toast(format!("Retrying: {}", task.title), None);
                    return Some(RuntimeCommand::RetryRun {
                        run_id: task.run_id.clone(),
                    });
                }
            },
            KeyCode::Char('R') => {
                if let Some(task) = snapshot.tasks.iter().find(|task| task.id == *task_id)
                    && matches!(
                        task.status,
                        polyphony_core::TaskStatus::Failed | polyphony_core::TaskStatus::InProgress
                    )
                {
                    app.show_toast(format!("Resolved: {}", task.title), None);
                    return Some(RuntimeCommand::ResolveTask {
                        run_id: task.run_id.clone(),
                        task_id: task.id.clone(),
                    });
                }
            },
            _ => {},
        },
        crate::app::DetailView::Agent { .. } => match key {
            KeyCode::Tab if in_split => {
                app.split_focus = crate::app::SplitFocus::List;
            },
            KeyCode::Enter => {},
            KeyCode::Char('j') | KeyCode::Down => {
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                scroll_detail_back(app, 8);
            },
            KeyCode::Char('c') => {
                request_cast_playback_from_detail(app, snapshot);
            },
            KeyCode::Char('S') => {
                if let Some(issue_id) = selected_running_agent_issue_id(app, snapshot) {
                    let identifier = snapshot
                        .running
                        .iter()
                        .find(|r| r.issue_id == issue_id)
                        .map(|r| r.issue_identifier.as_str())
                        .unwrap_or(&issue_id);
                    app.show_toast(format!("Stopping agent on {identifier}"), None);
                    return Some(RuntimeCommand::StopAgent { issue_id });
                }
            },
            KeyCode::Char('w') => {
                let agent_index = match app.current_detail() {
                    Some(crate::app::DetailView::Agent { agent_index, .. }) => Some(*agent_index),
                    _ => None,
                };
                if let Some(idx) = agent_index {
                    let ws = match app.resolve_agent(snapshot, idx) {
                        Some(app::SelectedAgentRow::Running(r)) => Some(r.workspace_path.clone()),
                        Some(app::SelectedAgentRow::History(h)) => h.workspace_path.clone(),
                        None => None,
                    };
                    if let Some(ws) = ws {
                        let ws = if ws.is_relative() {
                            std::env::current_dir()
                                .map(|cwd| cwd.join(&ws))
                                .unwrap_or(ws)
                        } else {
                            ws
                        };
                        open_terminal_at(&ws);
                    }
                }
            },
            _ => {},
        },
        crate::app::DetailView::Deliverable { ref run_id, .. } => match key {
            KeyCode::Tab if in_split => {
                app.split_focus = crate::app::SplitFocus::List;
            },
            KeyCode::Enter => {},
            KeyCode::Char('j') | KeyCode::Down => {
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                scroll_detail_back(app, 8);
            },
            KeyCode::Char('O') => {
                if let Some(run) = find_run_by_id(snapshot, run_id) {
                    let url = run
                        .deliverable
                        .as_ref()
                        .and_then(|d| d.url.as_deref())
                        .or_else(|| run.review_target.as_ref().and_then(|t| t.url.as_deref()));
                    if let Some(url) = url {
                        open_url(url);
                    }
                }
            },
            KeyCode::Char('a') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && run_can_resolve_deliverable(run)
                {
                    let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                    app.show_toast(format!("Accepting & merging {label}"), None);
                    return Some(RuntimeCommand::ResolveRunDeliverable {
                        run_id: run.id.clone(),
                        decision: polyphony_core::DeliverableDecision::Accepted,
                    });
                }
            },
            KeyCode::Char('x') => {
                if let Some(run) = find_run_by_id(snapshot, run_id)
                    && run_can_resolve_deliverable(run)
                {
                    let label = run.issue_identifier.as_deref().unwrap_or(&run.id);
                    app.show_toast(format!("Rejecting {label}"), None);
                    return Some(RuntimeCommand::ResolveRunDeliverable {
                        run_id: run.id.clone(),
                        decision: polyphony_core::DeliverableDecision::Rejected,
                    });
                }
            },
            _ => {},
        },
        crate::app::DetailView::Events { .. } => match key {
            KeyCode::Char('j') | KeyCode::Down => {
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                scroll_detail_back(app, 8);
            },
            _ => {},
        },
        crate::app::DetailView::LiveLog { .. } => match key {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(crate::app::DetailView::LiveLog { auto_scroll, .. }) =
                    app.current_detail_mut()
                {
                    *auto_scroll = false;
                }
                scroll_detail(app, 1);
            },
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(crate::app::DetailView::LiveLog { auto_scroll, .. }) =
                    app.current_detail_mut()
                {
                    *auto_scroll = false;
                }
                scroll_detail_back(app, 1);
            },
            KeyCode::PageDown => {
                if let Some(crate::app::DetailView::LiveLog { auto_scroll, .. }) =
                    app.current_detail_mut()
                {
                    *auto_scroll = false;
                }
                scroll_detail(app, 8);
            },
            KeyCode::PageUp => {
                if let Some(crate::app::DetailView::LiveLog { auto_scroll, .. }) =
                    app.current_detail_mut()
                {
                    *auto_scroll = false;
                }
                scroll_detail_back(app, 8);
            },
            KeyCode::Char('G') | KeyCode::End => {
                if let Some(crate::app::DetailView::LiveLog {
                    auto_scroll,
                    scroll,
                    ..
                }) = app.current_detail_mut()
                {
                    *auto_scroll = true;
                    *scroll = u16::MAX;
                }
            },
            _ => {},
        },
    }
    None
}

/// Refresh the cached content of a LiveLog detail view by re-reading the log file.
fn refresh_live_log_content(app: &mut AppState) {
    let Some(crate::app::DetailView::LiveLog {
        log_path,
        cached_content,
        auto_scroll,
        scroll,
        ..
    }) = app.current_detail_mut()
    else {
        return;
    };
    // Read the raw log file. For PTY/tmux logs that contain ANSI escapes, use vt100
    // to parse the terminal screen. For appserver logs (plain text), read directly.
    let is_plain_text = log_path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.contains("appserver"));
    if let Ok(raw) = std::fs::read(log_path) {
        if is_plain_text {
            *cached_content = String::from_utf8_lossy(&raw).into_owned();
        } else {
            let mut parser = vt100::Parser::new(500, 120, 0);
            parser.process(&raw);
            *cached_content = parser.screen().contents();
        }
        if *auto_scroll {
            *scroll = u16::MAX;
        }
    }
}

fn scroll_detail(app: &mut AppState, amount: u16) {
    if let Some(detail) = app.current_detail_mut() {
        let scroll = detail.scroll_mut();
        *scroll = scroll.saturating_add(amount);
    }
}

fn scroll_detail_back(app: &mut AppState, amount: u16) {
    if let Some(detail) = app.current_detail_mut() {
        let scroll = detail.scroll_mut();
        *scroll = scroll.saturating_sub(amount);
    }
}

/// Navigate within an inbox item detail view: when a section is focused, j/k moves
/// the mini-list selection; when Body is focused, j/k scrolls the page.
fn find_inbox_item_by_id<'a>(
    snapshot: &'a RuntimeSnapshot,
    item_id: &str,
) -> Option<&'a polyphony_core::InboxItemRow> {
    snapshot.inbox_items.iter().find(|t| t.item_id == item_id)
}

fn find_run_by_id<'a>(
    snapshot: &'a RuntimeSnapshot,
    run_id: &str,
) -> Option<&'a polyphony_core::RunRow> {
    snapshot.runs.iter().find(|m| m.id == run_id)
}

/// Return the issue_id of the currently selected running agent, if any.
/// Works from the Agents tab and the Orchestrator tab (when a RunningAgent row is selected).
fn selected_running_agent_issue_id(app: &AppState, snapshot: &RuntimeSnapshot) -> Option<String> {
    match app.active_tab {
        app::ActiveTab::Agents => {
            if let Some(app::SelectedAgentRow::Running(row)) = app.selected_agent(snapshot) {
                Some(row.issue_id.clone())
            } else {
                None
            }
        },
        app::ActiveTab::Orchestrator => {
            if let Some(app::OrchestratorTreeRow::RunningAgent { running_index, .. }) =
                app.selected_orchestrator_row().cloned()
            {
                snapshot
                    .running
                    .get(running_index)
                    .map(|r| r.issue_id.clone())
            } else {
                None
            }
        },
        _ => None,
    }
}

fn selected_deliverable_run<'a>(
    app: &AppState,
    snapshot: &'a RuntimeSnapshot,
) -> Option<&'a polyphony_core::RunRow> {
    match app.active_tab {
        app::ActiveTab::Deliverables => app.selected_deliverable(snapshot),
        app::ActiveTab::Orchestrator => {
            // When a Run row is selected directly.
            if let Some(m) = app
                .selected_run(snapshot)
                .filter(|m| m.deliverable.is_some())
            {
                return Some(m);
            }
            // When a child row (Outcome) is selected, find parent run.
            if let Some(app::OrchestratorTreeRow::Outcome { run_snapshot_index }) =
                app.selected_orchestrator_row()
            {
                return snapshot
                    .runs
                    .get(*run_snapshot_index)
                    .filter(|m| m.deliverable.is_some());
            }
            None
        },
        _ => None,
    }
}

fn selected_resolvable_deliverable_run<'a>(
    app: &AppState,
    snapshot: &'a RuntimeSnapshot,
) -> Option<&'a polyphony_core::RunRow> {
    selected_deliverable_run(app, snapshot).filter(|run| run_can_resolve_deliverable(run))
}

fn run_can_resolve_deliverable(run: &polyphony_core::RunRow) -> bool {
    run.deliverable.as_ref().is_some_and(|deliverable| {
        deliverable.decision == polyphony_core::DeliverableDecision::Waiting
    })
}

fn run_can_retry(snapshot: &RuntimeSnapshot, run_id: &str) -> bool {
    let Some(run) = find_run_by_id(snapshot, run_id) else {
        return false;
    };
    if run.status == polyphony_core::RunStatus::Failed {
        return true;
    }
    if run.status != polyphony_core::RunStatus::InProgress {
        return false;
    }

    let mut has_retryable_task = false;
    for task in snapshot.tasks.iter().filter(|task| task.run_id == run_id) {
        match task.status {
            polyphony_core::TaskStatus::Failed => return true,
            polyphony_core::TaskStatus::Pending | polyphony_core::TaskStatus::Cancelled => {
                has_retryable_task = true;
            },
            polyphony_core::TaskStatus::InProgress => return false,
            polyphony_core::TaskStatus::Completed => {},
        }
    }
    has_retryable_task
}

fn inbox_item_can_close_issue(
    snapshot: &RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> bool {
    if item.kind != InboxItemKind::Issue {
        return false;
    }
    if snapshot
        .running
        .iter()
        .any(|row| row.issue_id == item.item_id)
    {
        return false;
    }
    !matches!(
        item.status.to_ascii_lowercase().as_str(),
        "closed" | "done" | "completed" | "cancelled" | "canceled" | "reviewed" | "already_fixed"
    )
}

/// In split mode, after navigating the list, replace the single detail stack
/// entry with the newly selected entity so the right pane updates live.
fn update_split_detail_from_selection(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    if app.detail_stack.len() != 1 {
        return;
    }
    let new_detail = match app.active_tab {
        app::ActiveTab::Inbox => {
            app.selected_inbox_item(snapshot)
                .map(|t| crate::app::DetailView::InboxItem {
                    item_id: t.item_id.clone(),
                    scroll: 0,
                    focus: Default::default(),
                    runs_selected: 0,
                    agents_selected: 0,
                })
        },
        app::ActiveTab::Orchestrator => match app.selected_orchestrator_row().cloned() {
            Some(app::OrchestratorTreeRow::Run { snapshot_index }) => snapshot
                .runs
                .get(snapshot_index)
                .map(|m| crate::app::DetailView::Run {
                    run_id: m.id.clone(),
                    scroll: 0,
                }),
            Some(app::OrchestratorTreeRow::InboxItem { item_index, .. }) => snapshot
                .inbox_items
                .get(item_index)
                .map(|t| crate::app::DetailView::InboxItem {
                    item_id: t.item_id.clone(),
                    scroll: 0,
                    focus: Default::default(),
                    runs_selected: 0,
                    agents_selected: 0,
                }),
            Some(app::OrchestratorTreeRow::Task { snapshot_index, .. }) => snapshot
                .tasks
                .get(snapshot_index)
                .map(|t| crate::app::DetailView::Task {
                    task_id: t.id.clone(),
                    scroll: 0,
                }),
            Some(app::OrchestratorTreeRow::AgentSession { history_index, .. }) => {
                let display_index = app
                    .sorted_agent_indices
                    .iter()
                    .position(|&(is_running, idx)| !is_running && idx == history_index);
                display_index.map(|display_idx| crate::app::DetailView::Agent {
                    agent_index: display_idx,
                    scroll: u16::MAX,
                    artifact_cache: Box::new(None),
                })
            },
            Some(app::OrchestratorTreeRow::RunningAgent { running_index, .. }) => {
                let display_index = app
                    .sorted_agent_indices
                    .iter()
                    .position(|&(is_running, idx)| is_running && idx == running_index);
                display_index.map(|display_idx| crate::app::DetailView::Agent {
                    agent_index: display_idx,
                    scroll: u16::MAX,
                    artifact_cache: Box::new(None),
                })
            },
            Some(app::OrchestratorTreeRow::Outcome { run_snapshot_index }) => snapshot
                .runs
                .get(run_snapshot_index)
                .map(|m| crate::app::DetailView::Deliverable {
                    run_id: m.id.clone(),
                    scroll: 0,
                }),
            Some(app::OrchestratorTreeRow::LogEntry {
                run_snapshot_index, ..
            }) => snapshot
                .runs
                .get(run_snapshot_index)
                .map(|m| crate::app::DetailView::Run {
                    run_id: m.id.clone(),
                    scroll: 0,
                }),
            Some(app::OrchestratorTreeRow::AgentLogLine { running_index, .. }) => {
                let display_index = app
                    .sorted_agent_indices
                    .iter()
                    .position(|&(is_running, idx)| is_running && idx == running_index);
                display_index.map(|display_idx| crate::app::DetailView::Agent {
                    agent_index: display_idx,
                    scroll: u16::MAX,
                    artifact_cache: Box::new(None),
                })
            },
            Some(app::OrchestratorTreeRow::Progress {
                run_snapshot_index, ..
            }) => snapshot
                .runs
                .get(run_snapshot_index)
                .map(|m| crate::app::DetailView::Run {
                    run_id: m.id.clone(),
                    scroll: 0,
                }),
            None => None,
        },
        app::ActiveTab::Tasks => {
            app.selected_task(snapshot)
                .map(|t| crate::app::DetailView::Task {
                    task_id: t.id.clone(),
                    scroll: 0,
                })
        },
        app::ActiveTab::Deliverables => {
            app.selected_deliverable(snapshot)
                .map(|m| crate::app::DetailView::Deliverable {
                    run_id: m.id.clone(),
                    scroll: 0,
                })
        },
        app::ActiveTab::Agents => {
            app.agents_state
                .selected()
                .map(|idx| crate::app::DetailView::Agent {
                    agent_index: idx,
                    scroll: u16::MAX,
                    artifact_cache: Box::new(None),
                })
        },
        _ => None,
    };
    if let Some(detail) = new_detail
        && let Some(current) = app.detail_stack.last_mut()
    {
        *current = detail;
    }
}

fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// Open a new terminal window at the given directory.
fn open_terminal_at(path: &std::path::Path) {
    // macOS: open a new Terminal.app window at the path
    let _ = std::process::Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(path)
        .spawn();
}

/// Handle `c` key press: for running agents, open a live log detail view;
/// for finished agents, open the `.cast` replay in the browser.
fn request_cast_playback_for_agent(app: &mut AppState, agent: crate::app::SelectedAgentRow<'_>) {
    let target = match agent {
        crate::app::SelectedAgentRow::Running(r) => CastPlaybackTarget {
            workspace_path: Some(r.workspace_path.clone()),
            agent_name: r.agent_name.clone(),
            issue_identifier: r.issue_identifier.clone(),
            is_running: true,
            task_id: None,
        },
        crate::app::SelectedAgentRow::History(h) => CastPlaybackTarget {
            workspace_path: h.workspace_path.clone(),
            agent_name: h.agent_name.clone(),
            issue_identifier: h.issue_identifier.clone(),
            is_running: false,
            task_id: None,
        },
    };
    request_cast_playback_for_target(app, target);
}

#[derive(Debug, Clone)]
struct CastPlaybackTarget {
    workspace_path: Option<std::path::PathBuf>,
    agent_name: String,
    issue_identifier: String,
    is_running: bool,
    task_id: Option<String>,
}

fn request_cast_playback_for_target(app: &mut AppState, target: CastPlaybackTarget) {
    let CastPlaybackTarget {
        workspace_path,
        agent_name,
        issue_identifier,
        is_running,
        task_id,
    } = target;
    let Some(ws) = workspace_path else {
        tracing::debug!(agent_name, "cast playback: no workspace path");
        app.show_toast(
            format!("No recording for {agent_name}"),
            Some("No workspace path is available for this task or agent yet.".into()),
        );
        return;
    };
    // Ensure absolute path — workspace paths may be stored relative to CWD.
    let ws = if ws.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(&ws))
            .unwrap_or(ws)
    } else {
        ws
    };
    let run_dir = ws.join(".polyphony");

    if is_running {
        // Open a live log viewer inside the TUI
        for suffix in &["pty.log", "tmux.log", "appserver.log"] {
            let path = run_dir.join(format!("{agent_name}-{suffix}"));
            tracing::debug!(path = %path.display(), exists = path.exists(), "cast playback: checking live log");
            if path.exists() {
                app.push_detail(crate::app::DetailView::LiveLog {
                    log_path: path,
                    agent_name,
                    issue_identifier,
                    task_id,
                    scroll: u16::MAX,
                    cached_content: String::new(),
                    auto_scroll: true,
                });
                return;
            }
        }
    }

    // Finished agent (or running agent without log): open cast replay in browser
    for transport in &["pty", "tmux", "appserver"] {
        let path = run_dir.join(format!("{agent_name}-{transport}.cast"));
        tracing::debug!(path = %path.display(), exists = path.exists(), "cast playback: checking cast file");
        if path.exists() {
            app.pending_cast_playback = Some(crate::app::CastPlayback::Replay(path));
            return;
        }
    }
    app.show_toast(
        format!("No recording for {agent_name}"),
        Some("No log or cast file found. Use Enter for details.".into()),
    );
}

fn cast_playback_target_for_task(
    snapshot: &RuntimeSnapshot,
    task: &polyphony_core::TaskRow,
) -> Option<CastPlaybackTarget> {
    let agent_name = task.agent_name.clone()?;
    let run = find_run_by_id(snapshot, &task.run_id)?;
    let issue_identifier = run
        .issue_identifier
        .clone()
        .unwrap_or_else(|| run.id.clone());

    if let Some(running) = snapshot.running.iter().find(|running| {
        running.agent_name == agent_name && running.issue_identifier == issue_identifier
    }) {
        return Some(CastPlaybackTarget {
            workspace_path: Some(running.workspace_path.clone()),
            agent_name: running.agent_name.clone(),
            issue_identifier: running.issue_identifier.clone(),
            is_running: true,
            task_id: Some(task.id.clone()),
        });
    }

    if task.status == polyphony_core::TaskStatus::InProgress {
        return Some(CastPlaybackTarget {
            workspace_path: run.workspace_path.clone(),
            agent_name,
            issue_identifier,
            is_running: true,
            task_id: Some(task.id.clone()),
        });
    }

    let latest_history = snapshot
        .agent_run_history
        .iter()
        .filter(|history| {
            history.agent_name == agent_name && history.issue_identifier == issue_identifier
        })
        .max_by_key(|history| history.started_at);
    if let Some(history) = latest_history {
        return Some(CastPlaybackTarget {
            workspace_path: history.workspace_path.clone(),
            agent_name: history.agent_name.clone(),
            issue_identifier: history.issue_identifier.clone(),
            is_running: false,
            task_id: Some(task.id.clone()),
        });
    }

    Some(CastPlaybackTarget {
        workspace_path: run.workspace_path.clone(),
        agent_name,
        issue_identifier,
        is_running: false,
        task_id: Some(task.id.clone()),
    })
}

fn request_cast_playback_for_task(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    task: &polyphony_core::TaskRow,
) {
    if let Some(target) = cast_playback_target_for_task(snapshot, task) {
        request_cast_playback_for_target(app, target);
    }
}

/// Set `pending_cast_playback` on the app for the currently selected agent.
fn request_cast_playback(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    let agent = match app.active_tab {
        crate::app::ActiveTab::Agents => app.selected_agent(snapshot),
        crate::app::ActiveTab::Orchestrator => match app.selected_orchestrator_row().cloned() {
            Some(crate::app::OrchestratorTreeRow::AgentSession { history_index, .. }) => snapshot
                .agent_run_history
                .get(history_index)
                .map(crate::app::SelectedAgentRow::History),
            Some(crate::app::OrchestratorTreeRow::RunningAgent { running_index, .. }) => snapshot
                .running
                .get(running_index)
                .map(crate::app::SelectedAgentRow::Running),
            Some(crate::app::OrchestratorTreeRow::Task { snapshot_index, .. }) => {
                if let Some(task) = snapshot.tasks.get(snapshot_index) {
                    request_cast_playback_for_task(app, snapshot, task);
                }
                return;
            },
            _ => None,
        },
        _ => None,
    };
    if let Some(agent) = agent {
        request_cast_playback_for_agent(app, agent);
    }
}

/// Set `pending_cast_playback` for the agent shown in the current detail view.
fn request_cast_playback_from_detail(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    match app.current_detail() {
        Some(crate::app::DetailView::Agent { agent_index, .. }) => {
            if let Some(agent) = app.resolve_agent(snapshot, *agent_index) {
                request_cast_playback_for_agent(app, agent);
            }
        },
        Some(crate::app::DetailView::Task { task_id, .. }) => {
            if let Some(task) = snapshot.tasks.iter().find(|task| task.id == *task_id) {
                request_cast_playback_for_task(app, snapshot, task);
            }
        },
        _ => {},
    }
}

/// Open the cast replay in the browser (non-blocking).
fn run_cast_playback(playback: &crate::app::CastPlayback) {
    match playback {
        crate::app::CastPlayback::Replay(cast_path) => {
            open_cast_in_browser(cast_path);
        },
    }
}

/// Build a styled HTML transcript from log content for instant at-a-glance review.
fn build_transcript_html(content: &str) -> String {
    let mut html = String::from(
        r#"<div class="transcript"><h2>Session Transcript</h2><div class="transcript-lines">"#,
    );

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let escaped = html_escape::encode_text(line);
        // Classify lines by content for styling
        let class = if escaped.contains("→") {
            "line-sent"
        } else if escaped.contains("✓ turn completed") || escaped.contains("✓ Tool:") {
            "line-success"
        } else if escaped.contains("✕") || escaped.contains("turn failed") {
            "line-error"
        } else if escaped.contains("Agent:") {
            "line-agent"
        } else if escaped.contains("Prompt:") || escaped.contains("Plan:") {
            "line-prompt"
        } else if escaped.contains("Diff:") {
            "line-diff"
        } else if escaped.contains("Output:") || escaped.starts_with("              ") {
            "line-output"
        } else if escaped.contains("Tool:") || escaped.contains("Exec:") {
            "line-tool"
        } else if escaped.contains("←") {
            "line-received"
        } else {
            "line-default"
        };
        html.push_str(&format!(r#"<div class="tline {class}">{escaped}</div>"#));
    }

    html.push_str("</div></div>");
    html
}

/// Generate a self-contained HTML page with the asciinema-player and open it in the browser.
fn open_cast_in_browser(cast_path: &std::path::Path) {
    // Ensure absolute path for file:// URL
    let cast_path = if cast_path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(cast_path))
            .unwrap_or_else(|_| cast_path.to_path_buf())
    } else {
        cast_path.to_path_buf()
    };
    let cast_path = cast_path.as_path();
    let cast_data = match std::fs::read_to_string(cast_path) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(%error, path = %cast_path.display(), "failed to read cast file");
            return;
        },
    };

    let title = cast_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");

    // Derive agent name from cast filename (e.g. "router-pty" → "router")
    // and read the prompt file if it exists.
    let agent_name = title
        .strip_suffix("-pty")
        .or_else(|| title.strip_suffix("-tmux"))
        .or_else(|| title.strip_suffix("-appserver"))
        .unwrap_or(title);
    let prompt_html = cast_path
        .parent()
        .map(|dir| dir.join(format!("{agent_name}-prompt.md")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|prompt| {
            let escaped = html_escape::encode_text(&prompt);
            format!(
                r#"<details class="prompt"><summary>Prompt sent to agent</summary><pre>{escaped}</pre></details>"#
            )
        })
        .unwrap_or_default();

    // Build a styled transcript from the .log file for instant viewing.
    let transcript_html = cast_path
        .parent()
        .and_then(|dir| {
            // Try appserver.log, pty.log, tmux.log
            ["appserver.log", "pty.log", "tmux.log"]
                .iter()
                .map(|suffix| dir.join(format!("{agent_name}-{suffix}")))
                .find(|p| p.exists())
        })
        .and_then(|log_path| {
            let raw = std::fs::read(&log_path).ok()?;
            let is_plain = log_path
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.contains("appserver"));
            let content = if is_plain {
                String::from_utf8_lossy(&raw).into_owned()
            } else {
                let mut parser = vt100::Parser::new(500, 120, 0);
                parser.process(&raw);
                parser.screen().contents()
            };
            if content.trim().is_empty() {
                return None;
            }
            Some(build_transcript_html(&content))
        })
        .unwrap_or_default();

    // Escape the cast JSONL for embedding in a JS template literal:
    // backticks and backslashes need escaping, and ${} interpolation must be neutralised.
    let cast_escaped = cast_data
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("${", "\\${");

    let html = format!(
        r##"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>{title} — polyphony cast</title>
<link rel="stylesheet" href="https://unpkg.com/asciinema-player@3.9.0/dist/bundle/asciinema-player.css">
<style>
  body {{
    margin: 0;
    padding: 1.5em 2em;
    background: #1a1b26;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace;
  }}
  h1 {{
    color: #7aa2f7;
    font-size: 1.1em;
    font-weight: 400;
    margin: 0 0 0.8em;
  }}
  #player {{
    width: 100%;
    border: 1px solid #3b4261;
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.5);
    background: #282a36;
  }}
  /* Allow horizontal scroll if terminal is wider than viewport */
  #player {{
    overflow-x: auto;
  }}
  /* Ensure distinct terminal background */
  .ap-player {{
    background: #282a36 !important;
  }}
  /* Timeline scrubber bar below the player */
  #scrubber-container {{
    margin-top: 12px;
    display: flex;
    align-items: center;
    gap: 12px;
    color: #c0caf5;
    font-size: 0.85em;
    font-variant-numeric: tabular-nums;
  }}
  #scrubber {{
    flex: 1;
    height: 6px;
    -webkit-appearance: none;
    appearance: none;
    background: #3b4261;
    border-radius: 3px;
    outline: none;
    cursor: pointer;
  }}
  #scrubber::-webkit-slider-thumb {{
    -webkit-appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #7aa2f7;
    cursor: pointer;
  }}
  #scrubber::-moz-range-thumb {{
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #7aa2f7;
    border: none;
    cursor: pointer;
  }}
  #scrubber::-webkit-slider-runnable-track {{
    height: 6px;
    border-radius: 3px;
  }}
  #play-btn {{
    background: none;
    border: 1px solid #3b4261;
    border-radius: 4px;
    color: #c0caf5;
    font-size: 1.1em;
    width: 32px;
    height: 28px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
  }}
  #play-btn:hover {{
    background: #3b4261;
  }}
  .hint {{
    color: #565f89;
    font-size: 0.8em;
    margin-top: 1em;
  }}
  .transcript {{
    margin-top: 2em;
    width: 100%;
  }}
  .transcript h2 {{
    color: #7aa2f7;
    font-size: 1em;
    font-weight: 500;
    margin: 0 0 0.8em;
  }}
  .transcript-lines {{
    background: #282a36;
    border: 1px solid #3b4261;
    border-radius: 6px;
    padding: 1em;
    font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', 'Menlo', monospace;
    font-size: 13px;
    line-height: 1.6;
    max-height: 70vh;
    overflow-y: auto;
  }}
  .tline {{ white-space: pre-wrap; word-wrap: break-word; }}
  .line-sent {{ color: #9ece6a; }}
  .line-received {{ color: #7dcfff; }}
  .line-agent {{ color: #e0af68; }}
  .line-success {{ color: #9ece6a; font-weight: 500; }}
  .line-error {{ color: #f7768e; font-weight: 500; }}
  .line-prompt {{ color: #bb9af7; }}
  .line-diff {{ color: #7aa2f7; }}
  .line-output {{ color: #565f89; }}
  .line-tool {{ color: #7dcfff; font-weight: 500; }}
  .line-default {{ color: #a9b1d6; }}
  .prompt {{
    margin-top: 1.5em;
    width: 100%;
    color: #a9b1d6;
    font-size: 0.85em;
  }}
  .prompt summary {{
    cursor: pointer;
    color: #7aa2f7;
    font-weight: 500;
    margin-bottom: 0.5em;
  }}
  .prompt pre {{
    background: #282a36;
    border: 1px solid #3b4261;
    border-radius: 6px;
    padding: 1em;
    overflow-x: auto;
    white-space: pre-wrap;
    word-wrap: break-word;
    max-height: 60vh;
    overflow-y: auto;
    line-height: 1.5;
  }}
</style>
</head>
<body>
<h1>{title}</h1>
<div id="player"></div>
<div id="scrubber-container">
  <button id="play-btn" title="Play/Pause">▶</button>
  <span id="time-current">0:00</span>
  <input type="range" id="scrubber" min="0" max="1000" value="1000">
  <span id="time-total">0:00</span>
</div>
<p class="hint">space = play/pause &middot; &larr;&rarr; = seek 5s &middot; 0-9 = jump to %</p>
{transcript_html}
{prompt_html}
<script src="https://unpkg.com/asciinema-player@3.9.0/dist/bundle/asciinema-player.min.js"></script>
<script>
const castData = `{cast_escaped}`;
const blob = new Blob([castData], {{ type: "text/plain" }});
const url = URL.createObjectURL(blob);
const player = AsciinemaPlayer.create(url, document.getElementById("player"), {{
  fit: false,
  autoPlay: false,
  controls: false,
  poster: "npt:99:59:59",
  terminalFontSize: "14px",
  terminalFontFamily: "'JetBrains Mono', 'Fira Code', 'SF Mono', 'Menlo', monospace",
  theme: "dracula",
  idleTimeLimit: 2
}});

// Custom scrubber and controls.
// asciinema-player v3 exposes getDuration() and getCurrentTime() but they
// may return undefined until the recording is loaded. We derive duration
// from the cast data as a reliable fallback.
const scrubber = document.getElementById("scrubber");
const timeCurrent = document.getElementById("time-current");
const timeTotal = document.getElementById("time-total");
const playBtn = document.getElementById("play-btn");
let isPlaying = false;
let seeking = false;
let duration = 0;

// Parse duration from cast data (last event timestamp) so scrubber works before playback.
(function() {{
  const lines = castData.trim().split("\n");
  for (let i = lines.length - 1; i >= 1; i--) {{
    try {{
      const ev = JSON.parse(lines[i]);
      if (Array.isArray(ev) && typeof ev[0] === "number") {{ duration = ev[0]; break; }}
    }} catch(e) {{}}
  }}
  timeTotal.textContent = fmt(duration);
  timeCurrent.textContent = fmt(duration);
  scrubber.style.background = "linear-gradient(to right, #7aa2f7 100%, #3b4261 100%)";
}})();

function fmt(s) {{
  if (!s || isNaN(s)) return "0:00";
  s = Math.max(0, s);
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return m + ":" + (sec < 10 ? "0" : "") + sec;
}}

function updateScrubberUI(ct) {{
  if (!duration) return;
  ct = Math.max(0, Math.min(ct, duration));
  scrubber.value = Math.round((ct / duration) * 1000);
  timeCurrent.textContent = fmt(ct);
  const pct = (scrubber.value / 1000) * 100;
  scrubber.style.background = "linear-gradient(to right, #7aa2f7 " + pct + "%, #3b4261 " + pct + "%)";
}}

// Use the "playing" event (fires on first frame) to read duration,
// since getDuration() returns null before the recording is loaded.
player.addEventListener("playing", function() {{
  const d = player.getDuration();
  if (d != null && !isNaN(d)) {{
    duration = d;
    timeTotal.textContent = fmt(duration);
  }}
}});

player.addEventListener("play", function() {{
  isPlaying = true;
  playBtn.textContent = "⏸";
}});
player.addEventListener("pause", function() {{
  isPlaying = false;
  playBtn.textContent = "▶";
}});
player.addEventListener("ended", function() {{
  isPlaying = false;
  playBtn.textContent = "▶";
  updateScrubberUI(duration);
}});

// Poll getCurrentTime() to update the scrubber during playback.
setInterval(function() {{
  if (seeking) return;
  const ct = player.getCurrentTime();
  if (ct != null && !isNaN(ct)) updateScrubberUI(ct);
}}, 250);

// Scrubber drag — seek live while dragging
scrubber.addEventListener("input", function() {{
  seeking = true;
  if (!duration) return;
  const t = (scrubber.value / 1000) * duration;
  timeCurrent.textContent = fmt(t);
  const pct = (scrubber.value / 1000) * 100;
  scrubber.style.background = "linear-gradient(to right, #7aa2f7 " + pct + "%, #3b4261 " + pct + "%)";
  player.seek(t);
}});
scrubber.addEventListener("change", function() {{
  seeking = false;
}});

playBtn.addEventListener("click", function() {{
  if (isPlaying) {{ player.pause(); }} else {{ player.play(); }}
}});

document.addEventListener("keydown", function(e) {{
  if (e.key === " ") {{
    e.preventDefault();
    if (isPlaying) {{ player.pause(); }} else {{ player.play(); }}
  }} else if (e.key === "ArrowRight" && duration) {{
    player.seek(Math.min(player.getCurrentTime() + 5, duration));
  }} else if (e.key === "ArrowLeft" && duration) {{
    player.seek(Math.max(player.getCurrentTime() - 5, 0));
  }} else if (e.key >= "0" && e.key <= "9" && duration) {{
    player.seek(parseInt(e.key) / 10 * duration);
  }}
}});
</script>
</body>
</html>"##
    );

    let html_path = cast_path.with_extension("html");
    if let Err(error) = std::fs::write(&html_path, &html) {
        tracing::warn!(%error, "failed to write cast HTML player");
        return;
    }

    open_url(&format!("file://{}", html_path.display()));
}

fn handle_mouse_scroll(
    app: &mut AppState,
    mouse: &crossterm::event::MouseEvent,
    snapshot: &RuntimeSnapshot,
) {
    let now = Instant::now();
    let skip = app
        .last_scroll_at
        .is_some_and(|prev| now.duration_since(prev) < Duration::from_millis(50));
    if skip {
        return;
    }
    app.last_scroll_at = Some(now);

    let scrolling_down = matches!(mouse.kind, MouseEventKind::ScrollDown);
    if app.active_tab == app::ActiveTab::Logs {
        app.logs_auto_scroll = false;
    }

    if app.has_detail() {
        if let Some(detail) = app.current_detail_mut() {
            let scroll = detail.scroll_mut();
            if scrolling_down {
                *scroll = scroll.saturating_add(1);
            } else {
                *scroll = scroll.saturating_sub(1);
            }
        }
        return;
    }

    if app.active_tab == app::ActiveTab::Orchestrator
        && mouse_in_rect(mouse.column, mouse.row, app.events_area)
    {
        if scrolling_down {
            app.events_scroll = app.events_scroll.saturating_add(1);
        } else {
            app.events_scroll = app.events_scroll.saturating_sub(1);
        }
        return;
    }

    let len = app.active_table_len(snapshot);
    if scrolling_down {
        app.move_down(len, 1);
    } else {
        app.move_up(len, 1);
    }
}

fn start_dispatch_for_inbox_item(
    app: &mut AppState,
    item: &polyphony_core::InboxItemRow,
    agent_name: Option<String>,
) -> Option<RuntimeCommand> {
    app.dispatch_modal = Some(crate::app::DispatchModalState::new(
        item.item_id.clone(),
        item.identifier.clone(),
        item.title.clone(),
        item.kind,
        agent_name,
    ));
    None
}

fn handle_dispatch_modal_key(app: &mut AppState, key: event::KeyEvent) -> Option<RuntimeCommand> {
    let control_held = key.modifiers.contains(event::KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.dispatch_modal = None;
            None
        },
        KeyCode::Char('d') | KeyCode::Char('D') if control_held => submit_dispatch_modal(app),
        _ => {
            let modal = app.dispatch_modal.as_mut()?;
            match key.code {
                KeyCode::Enter => modal.insert_newline(),
                KeyCode::Backspace => modal.backspace(),
                KeyCode::Left => modal.move_left(),
                KeyCode::Right => modal.move_right(),
                KeyCode::Up => modal.move_up(),
                KeyCode::Down => modal.move_down(),
                KeyCode::Home => modal.move_home(),
                KeyCode::End => modal.move_end(),
                KeyCode::Tab => {
                    for _ in 0..4 {
                        modal.insert_char(' ');
                    }
                },
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == event::KeyModifiers::SHIFT =>
                {
                    modal.insert_char(c);
                },
                _ => {},
            }
            None
        },
    }
}

fn submit_dispatch_modal(app: &mut AppState) -> Option<RuntimeCommand> {
    let modal = app.dispatch_modal.take()?;
    let agent_label = modal.agent_name.as_deref().unwrap_or("default");
    let directives = modal.normalized_directives();
    app.show_toast(
        format!("Dispatching {} to {agent_label}", modal.item_identifier),
        directives.clone(),
    );
    app.dispatching_inbox_items.insert(modal.item_id.clone());
    match modal.item_kind {
        InboxItemKind::Issue => Some(RuntimeCommand::DispatchIssue {
            issue_id: modal.item_id,
            agent_name: modal.agent_name,
            directives,
        }),
        InboxItemKind::PullRequestReview
        | InboxItemKind::PullRequestComment
        | InboxItemKind::PullRequestConflict => {
            Some(RuntimeCommand::DispatchPullRequestInboxItem {
                item_id: modal.item_id,
                directives,
            })
        },
    }
}

pub(crate) fn handle_create_issue_modal_key(
    app: &mut AppState,
    key: event::KeyEvent,
) -> Option<RuntimeCommand> {
    let control_held = key.modifiers.contains(event::KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.create_issue_modal = None;
            None
        },
        KeyCode::Char('d') | KeyCode::Char('D') if control_held => submit_create_issue_modal(app),
        KeyCode::Tab => {
            if let Some(modal) = app.create_issue_modal.as_mut() {
                modal.toggle_field();
            }
            None
        },
        _ => {
            let modal = app.create_issue_modal.as_mut()?;
            match key.code {
                KeyCode::Enter => modal.insert_newline(),
                KeyCode::Backspace => modal.backspace(),
                KeyCode::Left => modal.move_left(),
                KeyCode::Right => modal.move_right(),
                KeyCode::Up => modal.move_up(),
                KeyCode::Down => modal.move_down(),
                KeyCode::Home => modal.move_home(),
                KeyCode::End => modal.move_end(),
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == event::KeyModifiers::SHIFT =>
                {
                    modal.insert_char(c);
                },
                _ => {},
            }
            None
        },
    }
}

fn submit_create_issue_modal(app: &mut AppState) -> Option<RuntimeCommand> {
    let modal = app.create_issue_modal.take()?;
    if !modal.is_valid() {
        app.create_issue_modal = Some(modal);
        app.show_toast("Title is required", None);
        return None;
    }
    let title = modal.title.trim().to_string();
    app.show_toast(format!("Creating issue: {title}"), None);
    Some(RuntimeCommand::CreateIssue {
        title,
        description: modal.description.trim().to_string(),
    })
}

pub(crate) fn handle_feedback_modal_key(
    app: &mut AppState,
    key: event::KeyEvent,
    snapshot: &RuntimeSnapshot,
) -> Option<RuntimeCommand> {
    let control_held = key.modifiers.contains(event::KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.feedback_modal = None;
            None
        },
        KeyCode::Char('d') | KeyCode::Char('D') if control_held => submit_feedback_modal(app),
        KeyCode::Tab => {
            // Cycle through agent profiles
            if !snapshot.agent_profiles.is_empty() {
                let modal = app.feedback_modal.as_mut()?;
                let current_idx = modal
                    .agent_name
                    .as_ref()
                    .and_then(|name| snapshot.agent_profiles.iter().position(|p| p.name == *name));
                let next_idx = match current_idx {
                    Some(idx) if idx + 1 < snapshot.agent_profiles.len() => idx + 1,
                    _ => 0,
                };
                modal.agent_name = Some(snapshot.agent_profiles[next_idx].name.clone());
            }
            None
        },
        KeyCode::BackTab => {
            // Clear agent selection back to default
            if let Some(modal) = app.feedback_modal.as_mut() {
                modal.agent_name = None;
            }
            None
        },
        _ => {
            let modal = app.feedback_modal.as_mut()?;
            match key.code {
                KeyCode::Enter => modal.insert_newline(),
                KeyCode::Backspace => modal.backspace(),
                KeyCode::Left => modal.move_left(),
                KeyCode::Right => modal.move_right(),
                KeyCode::Up => modal.move_up(),
                KeyCode::Down => modal.move_down(),
                KeyCode::Home => modal.move_home(),
                KeyCode::End => modal.move_end(),
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == event::KeyModifiers::SHIFT =>
                {
                    modal.insert_char(c);
                },
                _ => {},
            }
            None
        },
    }
}

fn submit_feedback_modal(app: &mut AppState) -> Option<RuntimeCommand> {
    let modal = app.feedback_modal.take()?;
    let prompt = modal.normalized_prompt();
    let Some(prompt) = prompt else {
        app.feedback_modal = Some(modal);
        app.show_toast("Feedback prompt is required", None);
        return None;
    };
    app.show_toast(
        "Injecting feedback",
        Some(prompt.lines().next().unwrap_or("").to_string()),
    );
    Some(RuntimeCommand::InjectRunFeedback {
        run_id: modal.run_id,
        prompt,
        agent_name: modal.agent_name,
    })
}

fn sync_selection_after_search(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    let len = app.sorted_issue_indices.len();
    if len == 0 {
        app.issues_state.select(None);
    } else {
        match app.issues_state.selected() {
            Some(i) if i >= len => app.issues_state.select(Some(len - 1)),
            None => app.issues_state.select(Some(0)),
            _ => {},
        }
    }
    let _ = snapshot; // used only for consistent API
}

// --- Helper functions ---

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::Utc;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use polyphony_core::{
        AgentContextEntry, AgentContextSnapshot, AgentEventKind, AttemptStatus, Deliverable,
        DeliverableDecision, DeliverableKind, DeliverableStatus, DispatchApprovalState,
        InboxItemKind, InboxItemRow, PersistedAgentRunRecord, RuntimeSnapshot, SnapshotCounts,
        TokenUsage, workspace_agent_run_history_artifact_path,
        workspace_saved_context_artifact_path,
    };
    use tokio::sync::mpsc;

    use crate::{LogBuffer, event_loop::AgentArtifactRequest};

    #[test]
    fn outputs_tab_accepts_selected_deliverable() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Deliverables;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('a'), &snapshot);

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::ResolveRunDeliverable {
                run_id,
                decision: polyphony_core::DeliverableDecision::Accepted,
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn outputs_tab_ignores_already_accepted_deliverable() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.runs[0]
            .deliverable
            .as_mut()
            .expect("deliverable exists")
            .decision = DeliverableDecision::Accepted;
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Deliverables;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('a'), &snapshot);

        assert!(command.is_none());
    }

    #[test]
    fn orchestrator_tab_rejects_selected_deliverable() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Orchestrator;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('x'), &snapshot);

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::ResolveRunDeliverable {
                run_id,
                decision: polyphony_core::DeliverableDecision::Rejected,
            }) if run_id == "run-1"
        ));
    }

    #[test]
    fn deliverable_detail_ignores_already_resolved_deliverable() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.runs[0]
            .deliverable
            .as_mut()
            .expect("deliverable exists")
            .decision = DeliverableDecision::Accepted;
        app.on_snapshot(&snapshot);
        app.push_detail(crate::app::DetailView::Deliverable {
            run_id: "run-1".into(),
            scroll: 0,
        });

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('a'), &snapshot);

        assert!(command.is_none());
    }

    #[test]
    fn inbox_tab_approves_waiting_issue() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.inbox_items = vec![InboxItemRow {
            repo_id: String::new(),
            item_id: "7".into(),
            kind: InboxItemKind::Issue,
            source: "github".into(),
            identifier: "#7".into(),
            title: "Untrusted issue".into(),
            status: "Todo".into(),
            approval_state: DispatchApprovalState::Waiting,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: Some("outsider".into()),
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        }];
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Inbox;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('a'), &snapshot);

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::ApproveInboxItem {
                item_id,
                source,
            }) if item_id == "7" && source == "github"
        ));
    }

    #[test]
    fn inbox_tab_approves_waiting_pull_request_event() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.inbox_items = vec![InboxItemRow {
            repo_id: String::new(),
            item_id: "pr_review:github:penso/polyphony:7:abc123".into(),
            kind: InboxItemKind::PullRequestReview,
            source: "github".into(),
            identifier: "penso/polyphony#7".into(),
            title: "Review me".into(),
            status: "waiting_approval".into(),
            approval_state: DispatchApprovalState::Waiting,
            priority: None,
            labels: vec![],
            description: None,
            url: None,
            author: Some("outsider".into()),
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        }];
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Inbox;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('a'), &snapshot);

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::ApproveInboxItem {
                item_id,
                source,
            }) if item_id == "pr_review:github:penso/polyphony:7:abc123" && source == "github"
        ));
    }

    #[test]
    fn inbox_tab_closes_selected_issue() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.inbox_items = vec![InboxItemRow {
            repo_id: String::new(),
            item_id: "7".into(),
            kind: InboxItemKind::Issue,
            source: "github".into(),
            identifier: "#7".into(),
            title: "Already done".into(),
            status: "Todo".into(),
            approval_state: DispatchApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: Some("penso".into()),
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        }];
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Inbox;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('x'), &snapshot);

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::CloseTrackerIssue { issue_id })
                if issue_id == "7"
        ));
    }

    #[test]
    fn inbox_tab_opens_dispatch_modal_for_issue() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.inbox_items = vec![InboxItemRow {
            repo_id: String::new(),
            item_id: "7".into(),
            kind: InboxItemKind::Issue,
            source: "github".into(),
            identifier: "#7".into(),
            title: "Needs operator guidance".into(),
            status: "Todo".into(),
            approval_state: DispatchApprovalState::Approved,
            priority: Some(2),
            labels: vec![],
            description: None,
            url: None,
            author: Some("penso".into()),
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        }];
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Inbox;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('d'), &snapshot);

        assert!(command.is_none());
        let modal = app
            .dispatch_modal
            .as_ref()
            .expect("dispatch modal should open");
        assert_eq!(modal.item_id, "7");
        assert_eq!(modal.item_identifier, "#7");
        assert_eq!(modal.item_title, "Needs operator guidance");
        assert_eq!(modal.item_kind, InboxItemKind::Issue);
        assert!(modal.agent_name.is_none());
    }

    #[test]
    fn inbox_tab_opens_dispatch_modal_for_pull_request_review() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.inbox_items = vec![InboxItemRow {
            repo_id: String::new(),
            item_id: "pr-review-7".into(),
            kind: InboxItemKind::PullRequestReview,
            source: "github".into(),
            identifier: "penso/polyphony#7".into(),
            title: "Review me".into(),
            status: "ready".into(),
            approval_state: DispatchApprovalState::Approved,
            priority: None,
            labels: vec![],
            description: None,
            url: None,
            author: Some("penso".into()),
            parent_id: None,
            updated_at: None,
            created_at: None,
            has_workspace: false,
        }];
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Inbox;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('d'), &snapshot);

        assert!(command.is_none());
        let modal = app
            .dispatch_modal
            .as_ref()
            .expect("dispatch modal should open");
        assert_eq!(modal.item_id, "pr-review-7");
        assert_eq!(modal.item_kind, InboxItemKind::PullRequestReview);
    }

    #[test]
    fn dispatch_modal_submits_issue_with_directives() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.dispatch_modal = Some(crate::app::DispatchModalState::new(
            "7".into(),
            "#7".into(),
            "Needs operator guidance".into(),
            InboxItemKind::Issue,
            Some("router".into()),
        ));

        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::empty()),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let command = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::DispatchIssue {
                issue_id,
                agent_name,
                directives,
            }) if issue_id == "7"
                && agent_name.as_deref() == Some("router")
                && directives.as_deref() == Some("Plan")
        ));
        assert!(app.dispatch_modal.is_none());
        assert!(app.dispatching_inbox_items.contains("7"));
    }

    #[test]
    fn dispatch_modal_submits_pull_request_event_with_directives() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.dispatch_modal = Some(crate::app::DispatchModalState::new(
            "pr-review-7".into(),
            "penso/polyphony#7".into(),
            "Review me".into(),
            InboxItemKind::PullRequestReview,
            None,
        ));

        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::empty()),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()),
        );
        let _ = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()),
        );
        let command = crate::event_loop::handle_dispatch_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::DispatchPullRequestInboxItem {
                item_id,
                directives,
            }) if item_id == "pr-review-7" && directives.as_deref() == Some("Check")
        ));
        assert!(app.dispatch_modal.is_none());
        assert!(app.dispatching_inbox_items.contains("pr-review-7"));
    }

    #[test]
    fn dispatch_modal_consumes_typed_keys_before_global_keybinds() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.dispatch_modal = Some(crate::app::DispatchModalState::new(
            "7".into(),
            "#7".into(),
            "Needs operator guidance".into(),
            InboxItemKind::Issue,
            None,
        ));
        let snapshot = test_snapshot_with_deliverable();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );
        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        let modal = app
            .dispatch_modal
            .as_ref()
            .expect("dispatch modal should remain open");
        assert_eq!(modal.directives, "oq");
        assert!(!app.confirm_quit);
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn inbox_search_consumes_typed_keys_before_global_keybinds() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        app.search_active = true;

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );
        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert_eq!(app.search_query, "oq");
        assert!(!app.confirm_quit);
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn run_search_consumes_typed_keys_before_global_keybinds() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        app.runs_search_active = true;

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );
        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert_eq!(app.runs_search_query, "oq");
        assert!(!app.confirm_quit);
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn logs_search_consumes_typed_keys_before_global_keybinds() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        app.logs_search_active = true;

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );
        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert_eq!(app.logs_search_query, "oq");
        assert!(!app.confirm_quit);
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn mouse_scroll_in_logs_disables_auto_scroll_and_moves_selection() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.active_tab = crate::app::ActiveTab::Logs;
        app.logs_auto_scroll = true;
        app.logs_state.select(Some(4));
        for index in 0..8 {
            app.log_buffer.push_line(format!("line {index}"));
        }
        let snapshot = test_snapshot_with_deliverable();

        crate::event_loop::handle_mouse_scroll(
            &mut app,
            &MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::empty(),
            },
            &snapshot,
        );

        assert!(!app.logs_auto_scroll);
        assert_eq!(app.logs_state.selected(), Some(3));
    }

    #[test]
    fn load_agent_artifact_reads_running_saved_context_from_workspace_file() {
        let workspace = std::env::temp_dir().join(format!(
            "polyphony-tui-running-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace.join(".polyphony/runtime")).unwrap();
        let context = AgentContextSnapshot {
            repo_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "DOG-1".into(),
            updated_at: Utc::now(),
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Succeeded),
            error: None,
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: Utc::now(),
                kind: AgentEventKind::Notification,
                message: "from artifact".into(),
            }],
        };
        std::fs::write(
            workspace_saved_context_artifact_path(&workspace),
            serde_json::to_vec_pretty(&context).unwrap(),
        )
        .unwrap();

        let loaded = crate::event_loop::load_agent_artifact(AgentArtifactRequest::Running {
            key: "running".into(),
            workspace_path: workspace.clone(),
        })
        .unwrap()
        .unwrap();

        assert_eq!(loaded.issue_identifier, "DOG-1");
        assert_eq!(loaded.transcript[0].message, "from artifact");
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn load_agent_artifact_reads_history_saved_context_from_run_history_file() {
        let workspace = std::env::temp_dir().join(format!(
            "polyphony-tui-history-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace.join(".polyphony/runtime")).unwrap();
        let now = Utc::now();
        let context = AgentContextSnapshot {
            repo_id: String::new(),
            issue_id: "issue-2".into(),
            issue_identifier: "DOG-2".into(),
            updated_at: now,
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Succeeded),
            error: None,
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: now,
                kind: AgentEventKind::Notification,
                message: "history artifact".into(),
            }],
        };
        let record = PersistedAgentRunRecord {
            repo_id: String::new(),
            issue_id: "issue-2".into(),
            issue_identifier: "DOG-2".into(),
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(2),
            max_turns: 3,
            turn_count: 1,
            last_event: None,
            last_message: None,
            started_at: now,
            finished_at: Some(now),
            last_event_at: Some(now),
            tokens: TokenUsage::default(),
            workspace_path: Some(workspace.clone()),
            error: None,
            saved_context: Some(context),
        };
        std::fs::write(
            workspace_agent_run_history_artifact_path(&workspace),
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();

        let loaded = crate::event_loop::load_agent_artifact(AgentArtifactRequest::History {
            key: "history".into(),
            workspace_path: workspace.clone(),
            issue_id: "issue-2".into(),
            started_at: now,
            agent_name: "implementer".into(),
            attempt: Some(2),
        })
        .unwrap()
        .unwrap();

        assert_eq!(loaded.issue_identifier, "DOG-2");
        assert_eq!(loaded.transcript[0].message, "history artifact");
        std::fs::remove_dir_all(workspace).unwrap();
    }

    fn test_snapshot_with_deliverable() -> RuntimeSnapshot {
        RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: Utc::now(),
            counts: SnapshotCounts::default(),
            cadence: Default::default(),
            tracker_issues: vec![],
            inbox_items: vec![],
            approved_inbox_keys: vec![],
            running: vec![],
            agent_run_history: vec![],
            retrying: vec![],
            codex_totals: Default::default(),
            rate_limits: None,
            throttles: vec![],
            budgets: vec![],
            agent_catalogs: vec![],
            saved_contexts: vec![],
            recent_events: vec![],
            pending_user_interactions: vec![],
            runs: vec![polyphony_core::RunRow {
                repo_id: String::new(),
                id: "run-1".into(),
                kind: polyphony_core::RunKind::IssueDelivery,
                issue_identifier: Some("#7".into()),
                title: "Ship PR".into(),
                status: polyphony_core::RunStatus::Delivered,
                task_count: 1,
                tasks_completed: 1,
                deliverable: Some(Deliverable {
                    kind: DeliverableKind::GithubPullRequest,
                    status: DeliverableStatus::Open,
                    url: Some("https://github.com/penso/polyphony/pull/8".into()),
                    decision: DeliverableDecision::Waiting,
                    title: None,
                    description: None,
                    metadata: Default::default(),
                }),
                has_deliverable: true,
                review_target: None,
                workspace_key: None,
                workspace_path: None,
                created_at: Utc::now(),
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
            }],
            tasks: vec![],
            loading: Default::default(),
            dispatch_mode: Default::default(),
            tracker_kind: Default::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: vec![],
            agent_profiles: vec![],
        }
    }

    fn test_snapshot_with_task(status: polyphony_core::TaskStatus) -> RuntimeSnapshot {
        let now = Utc::now();
        RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: Default::default(),
            tracker_issues: vec![],
            inbox_items: vec![],
            approved_inbox_keys: vec![],
            running: vec![],
            agent_run_history: vec![],
            retrying: vec![],
            codex_totals: Default::default(),
            rate_limits: None,
            throttles: vec![],
            budgets: vec![],
            agent_catalogs: vec![],
            saved_contexts: vec![],
            recent_events: vec![],
            pending_user_interactions: vec![],
            runs: vec![polyphony_core::RunRow {
                repo_id: String::new(),
                id: "run-task-1".into(),
                kind: polyphony_core::RunKind::PullRequestReview,
                issue_identifier: Some("penso/arbor#89".into()),
                title: "Retry me".into(),
                status: polyphony_core::RunStatus::Failed,
                task_count: 1,
                tasks_completed: 0,
                deliverable: None,
                has_deliverable: false,
                review_target: None,
                workspace_key: Some("penso_arbor_89".into()),
                workspace_path: None,
                created_at: now,
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
            }],
            tasks: vec![polyphony_core::TaskRow {
                repo_id: String::new(),
                id: "task-1".into(),
                run_id: "run-task-1".into(),
                title: "Creating worktree".into(),
                description: None,
                activity_log: vec![],
                category: polyphony_core::TaskCategory::Research,
                status,
                ordinal: 0,
                agent_name: Some("orchestrator".into()),
                turns_completed: 0,
                total_tokens: 0,
                started_at: Some(now),
                finished_at: None,
                error: None,
                created_at: now,
                updated_at: now,
            }],
            loading: Default::default(),
            dispatch_mode: Default::default(),
            tracker_kind: Default::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: vec![],
            agent_profiles: vec![],
        }
    }

    fn test_snapshot_with_review_task_log(
        workspace_path: std::path::PathBuf,
        status: polyphony_core::TaskStatus,
    ) -> RuntimeSnapshot {
        let now = Utc::now();
        RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: Default::default(),
            tracker_issues: vec![],
            inbox_items: vec![],
            approved_inbox_keys: vec![],
            running: vec![],
            agent_run_history: vec![],
            retrying: vec![],
            codex_totals: Default::default(),
            rate_limits: None,
            throttles: vec![],
            budgets: vec![],
            agent_catalogs: vec![],
            saved_contexts: vec![],
            recent_events: vec![],
            pending_user_interactions: vec![],
            runs: vec![polyphony_core::RunRow {
                repo_id: String::new(),
                id: "run-review-1".into(),
                kind: polyphony_core::RunKind::PullRequestReview,
                issue_identifier: Some("penso/arbor#89".into()),
                title: "Review me".into(),
                status: polyphony_core::RunStatus::InProgress,
                task_count: 2,
                tasks_completed: 1,
                deliverable: None,
                has_deliverable: false,
                review_target: None,
                workspace_key: Some("penso_arbor_89".into()),
                workspace_path: Some(workspace_path),
                created_at: now,
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
            }],
            tasks: vec![polyphony_core::TaskRow {
                repo_id: String::new(),
                id: "task-review-1".into(),
                run_id: "run-review-1".into(),
                title: "Run PR review".into(),
                description: None,
                activity_log: vec![],
                category: polyphony_core::TaskCategory::Review,
                status,
                ordinal: 1,
                agent_name: Some("reviewer".into()),
                turns_completed: 0,
                total_tokens: 0,
                started_at: Some(now),
                finished_at: None,
                error: None,
                created_at: now,
                updated_at: now,
            }],
            loading: Default::default(),
            dispatch_mode: Default::default(),
            tracker_kind: Default::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: vec![],
            agent_profiles: vec![],
        }
    }

    fn unique_temp_workspace(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn orchestrator_retry_key_retries_failed_run() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.active_tab = crate::app::ActiveTab::Orchestrator;
        let failed_snapshot = test_snapshot_with_task(polyphony_core::TaskStatus::Failed);
        app.on_snapshot(&failed_snapshot);
        app.rebuild_orchestrator_tree(&failed_snapshot);
        let run_index = app
            .orchestrator_tree_rows
            .iter()
            .position(|row| matches!(row, crate::app::OrchestratorTreeRow::Run { .. }))
            .expect("run row");
        app.runs_state.select(Some(run_index));
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()),
            &failed_snapshot,
            &command_tx,
        );

        assert!(matches!(
            command_rx.try_recv().ok(),
            Some(polyphony_orchestrator::RuntimeCommand::RetryRun { run_id })
                if run_id == "run-task-1"
        ));

        app.toggle_run_collapse("run-task-1");
        app.rebuild_orchestrator_tree(&failed_snapshot);
        let task_index = app
            .orchestrator_tree_rows
            .iter()
            .position(|row| matches!(row, crate::app::OrchestratorTreeRow::Task { .. }))
            .expect("task row");
        app.runs_state.select(Some(task_index));

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()),
            &failed_snapshot,
            &command_tx,
        );

        assert!(matches!(
            command_rx.try_recv().ok(),
            Some(polyphony_orchestrator::RuntimeCommand::RetryRun { run_id })
                if run_id == "run-task-1"
        ));

        let completed_snapshot = test_snapshot_with_task(polyphony_core::TaskStatus::Completed);
        app.on_snapshot(&completed_snapshot);
        let completed_index = app
            .orchestrator_tree_rows
            .iter()
            .position(|row| matches!(row, crate::app::OrchestratorTreeRow::Task { .. }))
            .expect("task row");
        app.runs_state.select(Some(completed_index));

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()),
            &completed_snapshot,
            &command_tx,
        );

        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn task_detail_retry_key_retries_parent_run() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let failed_snapshot = test_snapshot_with_task(polyphony_core::TaskStatus::Failed);
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        app.push_detail(crate::app::DetailView::Task {
            task_id: "task-1".into(),
            scroll: 0,
        });
        app.split_focus = crate::app::SplitFocus::Detail;

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()),
            &failed_snapshot,
            &command_tx,
        );

        assert!(matches!(
            command_rx.try_recv().ok(),
            Some(polyphony_orchestrator::RuntimeCommand::RetryRun { run_id })
                if run_id == "run-task-1"
        ));

        let completed_snapshot = test_snapshot_with_task(polyphony_core::TaskStatus::Completed);
        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()),
            &completed_snapshot,
            &command_tx,
        );

        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn orchestrator_retry_key_retries_stalled_run_with_pending_task() {
        let now = Utc::now();
        let stale_snapshot = RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: Default::default(),
            tracker_issues: vec![],
            inbox_items: vec![],
            approved_inbox_keys: vec![],
            running: vec![],
            agent_run_history: vec![],
            retrying: vec![],
            codex_totals: Default::default(),
            rate_limits: None,
            throttles: vec![],
            budgets: vec![],
            agent_catalogs: vec![],
            saved_contexts: vec![],
            recent_events: vec![],
            pending_user_interactions: vec![],
            runs: vec![polyphony_core::RunRow {
                repo_id: String::new(),
                id: "run-stalled-1".into(),
                kind: polyphony_core::RunKind::PullRequestReview,
                issue_identifier: Some("penso/arbor#89".into()),
                title: "Retry stale run".into(),
                status: polyphony_core::RunStatus::InProgress,
                task_count: 2,
                tasks_completed: 0,
                deliverable: None,
                has_deliverable: false,
                review_target: None,
                workspace_key: Some("penso_arbor_89".into()),
                workspace_path: None,
                created_at: now,
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
            }],
            tasks: vec![
                polyphony_core::TaskRow {
                    repo_id: String::new(),
                    id: "task-stalled-1".into(),
                    run_id: "run-stalled-1".into(),
                    title: "Creating worktree".into(),
                    description: None,
                    activity_log: vec![],
                    category: polyphony_core::TaskCategory::Research,
                    status: polyphony_core::TaskStatus::Pending,
                    ordinal: 0,
                    agent_name: Some("orchestrator".into()),
                    turns_completed: 0,
                    total_tokens: 0,
                    started_at: None,
                    finished_at: None,
                    error: None,
                    created_at: now,
                    updated_at: now,
                },
                polyphony_core::TaskRow {
                    repo_id: String::new(),
                    id: "task-stalled-2".into(),
                    run_id: "run-stalled-1".into(),
                    title: "Run PR review".into(),
                    description: None,
                    activity_log: vec![],
                    category: polyphony_core::TaskCategory::Review,
                    status: polyphony_core::TaskStatus::Cancelled,
                    ordinal: 1,
                    agent_name: Some("reviewer".into()),
                    turns_completed: 0,
                    total_tokens: 0,
                    started_at: None,
                    finished_at: Some(now),
                    error: Some("workspace setup failed".into()),
                    created_at: now,
                    updated_at: now,
                },
            ],
            loading: Default::default(),
            dispatch_mode: Default::default(),
            tracker_kind: Default::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: vec![],
            agent_profiles: vec![],
        };
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.active_tab = crate::app::ActiveTab::Orchestrator;
        app.on_snapshot(&stale_snapshot);
        app.rebuild_orchestrator_tree(&stale_snapshot);
        let run_index = app
            .orchestrator_tree_rows
            .iter()
            .position(|row| matches!(row, crate::app::OrchestratorTreeRow::Run { .. }))
            .expect("run row");
        app.runs_state.select(Some(run_index));
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()),
            &stale_snapshot,
            &command_tx,
        );

        assert!(matches!(
            command_rx.try_recv().ok(),
            Some(polyphony_orchestrator::RuntimeCommand::RetryRun { run_id })
                if run_id == "run-stalled-1"
        ));
    }

    #[test]
    fn orchestrator_task_cast_key_opens_live_log_from_task_workspace() {
        let workspace = unique_temp_workspace("polyphony-tui-task-cast-orchestrator");
        fs::create_dir_all(workspace.join(".polyphony")).unwrap();
        let log_path = workspace.join(".polyphony/reviewer-pty.log");
        fs::write(&log_path, "review output\n").unwrap();

        let snapshot = test_snapshot_with_review_task_log(
            workspace.clone(),
            polyphony_core::TaskStatus::InProgress,
        );
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.active_tab = crate::app::ActiveTab::Orchestrator;
        app.on_snapshot(&snapshot);
        app.rebuild_orchestrator_tree(&snapshot);
        let task_index = app
            .orchestrator_tree_rows
            .iter()
            .position(|row| matches!(row, crate::app::OrchestratorTreeRow::Task { .. }))
            .expect("task row");
        app.runs_state.select(Some(task_index));
        let (command_tx, _command_rx) = mpsc::unbounded_channel();

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert!(matches!(
            app.current_detail(),
            Some(crate::app::DetailView::LiveLog { log_path: path, agent_name, issue_identifier, task_id, .. })
                if *path == log_path
                    && agent_name == "reviewer"
                    && issue_identifier == "penso/arbor#89"
                    && task_id.as_deref() == Some("task-review-1")
        ));
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn task_detail_cast_key_opens_live_log_from_task_workspace() {
        let workspace = unique_temp_workspace("polyphony-tui-task-cast-detail");
        fs::create_dir_all(workspace.join(".polyphony")).unwrap();
        let log_path = workspace.join(".polyphony/reviewer-pty.log");
        fs::write(&log_path, "review output\n").unwrap();

        let snapshot = test_snapshot_with_review_task_log(
            workspace.clone(),
            polyphony_core::TaskStatus::InProgress,
        );
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.push_detail(crate::app::DetailView::Task {
            task_id: "task-review-1".into(),
            scroll: 0,
        });
        app.split_focus = crate::app::SplitFocus::Detail;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert!(matches!(
            app.current_detail(),
            Some(crate::app::DetailView::LiveLog { log_path: path, agent_name, issue_identifier, task_id, .. })
                if *path == log_path
                    && agent_name == "reviewer"
                    && issue_identifier == "penso/arbor#89"
                    && task_id.as_deref() == Some("task-review-1")
        ));
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn task_cast_prefers_live_log_over_previous_history_cast() {
        let workspace = unique_temp_workspace("polyphony-tui-task-cast-prefer-live");
        fs::create_dir_all(workspace.join(".polyphony")).unwrap();
        let log_path = workspace.join(".polyphony/reviewer-pty.log");
        fs::write(&log_path, "review output\n").unwrap();
        let old_workspace = unique_temp_workspace("polyphony-tui-task-cast-old");
        fs::create_dir_all(old_workspace.join(".polyphony")).unwrap();
        fs::write(
            old_workspace.join(".polyphony/reviewer-pty.cast"),
            "cast data\n",
        )
        .unwrap();

        let mut snapshot = test_snapshot_with_review_task_log(
            workspace.clone(),
            polyphony_core::TaskStatus::InProgress,
        );
        snapshot
            .agent_run_history
            .push(polyphony_core::AgentRunHistoryRow {
                repo_id: String::new(),
                issue_id: "issue-89".into(),
                issue_identifier: "penso/arbor#89".into(),
                agent_name: "reviewer".into(),
                model: Some("gpt-5.4".into()),
                status: AttemptStatus::CancelledByUser,
                attempt: Some(1),
                max_turns: 10,
                turn_count: 1,
                session_id: None,
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                last_event: Some("cancelled".into()),
                last_message: Some("previous attempt".into()),
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                last_event_at: Some(Utc::now()),
                tokens: TokenUsage::default(),
                workspace_path: Some(old_workspace.clone()),
                error: Some("cancelled".into()),
                saved_context: None,
            });
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.push_detail(crate::app::DetailView::Task {
            task_id: "task-review-1".into(),
            scroll: 0,
        });
        app.split_focus = crate::app::SplitFocus::Detail;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();

        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert!(matches!(
            app.current_detail(),
            Some(crate::app::DetailView::LiveLog { log_path: path, task_id, .. })
                if *path == log_path && task_id.as_deref() == Some("task-review-1")
        ));
        assert!(app.pending_cast_playback.is_none());
        fs::remove_dir_all(workspace).unwrap();
        fs::remove_dir_all(old_workspace).unwrap();
    }

    #[test]
    fn inbox_tab_opens_create_issue_modal() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        app.on_snapshot(&snapshot);
        app.active_tab = crate::app::ActiveTab::Inbox;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('n'), &snapshot);

        assert!(command.is_none());
        assert!(app.create_issue_modal.is_some());
    }

    #[test]
    fn create_issue_modal_submits_with_title() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.create_issue_modal = Some(crate::app::CreateIssueModalState::new());

        // Type title
        for ch in "Fix the bug".chars() {
            let _ = crate::event_loop::handle_create_issue_modal_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()),
            );
        }

        // Switch to description
        let _ = crate::event_loop::handle_create_issue_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );

        // Type description
        for ch in "details here".chars() {
            let _ = crate::event_loop::handle_create_issue_modal_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()),
            );
        }

        // Submit with Ctrl+D
        let command = crate::event_loop::handle_create_issue_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::CreateIssue { title, description })
                if title == "Fix the bug" && description == "details here"
        ));
        assert!(app.create_issue_modal.is_none());
    }

    #[test]
    fn create_issue_modal_rejects_empty_title() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.create_issue_modal = Some(crate::app::CreateIssueModalState::new());

        let command = crate::event_loop::handle_create_issue_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );

        assert!(command.is_none());
        assert!(app.create_issue_modal.is_some());
    }

    #[test]
    fn create_issue_modal_escape_cancels() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.create_issue_modal = Some(crate::app::CreateIssueModalState::new());

        let command = crate::event_loop::handle_create_issue_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert!(command.is_none());
        assert!(app.create_issue_modal.is_none());
    }

    #[test]
    fn run_detail_opens_feedback_modal() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.runs[0].workspace_path = Some(std::path::PathBuf::from("/tmp/ws"));
        app.on_snapshot(&snapshot);
        app.push_detail(crate::app::DetailView::Run {
            run_id: "run-1".into(),
            scroll: 0,
        });

        let (command_tx, _) = mpsc::unbounded_channel();
        crate::event_loop::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
            &snapshot,
            &command_tx,
        );

        assert!(app.feedback_modal.is_some());
        let modal = app.feedback_modal.as_ref().unwrap();
        assert_eq!(modal.run_id, "run-1");
    }

    #[test]
    fn feedback_modal_submits_prompt() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        app.feedback_modal = Some(crate::app::FeedbackModalState::new(
            "run-1".into(),
            "Ship PR".into(),
        ));

        for ch in "add tests".chars() {
            let _ = crate::event_loop::handle_feedback_modal_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()),
                &snapshot,
            );
        }

        let command = crate::event_loop::handle_feedback_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &snapshot,
        );

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::InjectRunFeedback {
                run_id,
                prompt,
                agent_name,
            }) if run_id == "run-1" && prompt == "add tests" && agent_name.is_none()
        ));
        assert!(app.feedback_modal.is_none());
    }

    #[test]
    fn feedback_modal_rejects_empty_prompt() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        app.feedback_modal = Some(crate::app::FeedbackModalState::new(
            "run-1".into(),
            "Ship PR".into(),
        ));

        let command = crate::event_loop::handle_feedback_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &snapshot,
        );

        assert!(command.is_none());
        assert!(app.feedback_modal.is_some());
    }

    #[test]
    fn feedback_modal_escape_cancels() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let snapshot = test_snapshot_with_deliverable();
        app.feedback_modal = Some(crate::app::FeedbackModalState::new(
            "run-1".into(),
            "Ship PR".into(),
        ));

        let command = crate::event_loop::handle_feedback_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
            &snapshot,
        );

        assert!(command.is_none());
        assert!(app.feedback_modal.is_none());
    }

    #[test]
    fn feedback_modal_tab_cycles_agent() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.agent_profiles = vec![
            polyphony_core::AgentProfileSummary {
                name: "coder".into(),
                kind: String::new(),
                description: None,
                source: Default::default(),
            },
            polyphony_core::AgentProfileSummary {
                name: "reviewer".into(),
                kind: String::new(),
                description: None,
                source: Default::default(),
            },
        ];
        app.feedback_modal = Some(crate::app::FeedbackModalState::new(
            "run-1".into(),
            "Ship PR".into(),
        ));

        // First Tab selects the first agent
        let _ = crate::event_loop::handle_feedback_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            &snapshot,
        );
        assert_eq!(
            app.feedback_modal.as_ref().unwrap().agent_name.as_deref(),
            Some("coder")
        );

        // Second Tab selects next agent
        let _ = crate::event_loop::handle_feedback_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            &snapshot,
        );
        assert_eq!(
            app.feedback_modal.as_ref().unwrap().agent_name.as_deref(),
            Some("reviewer")
        );

        // Shift+Tab resets to default
        let _ = crate::event_loop::handle_feedback_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &snapshot,
        );
        assert!(app.feedback_modal.as_ref().unwrap().agent_name.is_none());
    }

    #[test]
    fn create_issue_modal_consumes_typed_keys() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        app.create_issue_modal = Some(crate::app::CreateIssueModalState::new());
        app.active_tab = crate::app::ActiveTab::Inbox;

        // Type 'q' which would normally quit — should be consumed by the modal
        let _ = crate::event_loop::handle_create_issue_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
        );

        assert!(app.create_issue_modal.is_some());
        let modal = app.create_issue_modal.as_ref().unwrap();
        assert_eq!(modal.title, "q");
    }
}
