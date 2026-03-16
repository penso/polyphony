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

    let result = loop {
        terminal.draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })?;

        if let Some(since) = app.leaving_since
            && since.elapsed() > Duration::from_secs(3)
        {
            break Ok(());
        }

        let mut key_handled = false;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Mouse(mouse) => {
                    if !app.leaving {
                        if app.show_issue_detail {
                            // Click outside modal closes it
                            if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                                app.show_issue_detail = false;
                                app.detail_scroll = 0;
                            }
                        } else if app.show_task_detail {
                            if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                                app.show_task_detail = false;
                                app.task_detail_scroll = 0;
                            }
                        } else {
                            match mouse.kind {
                                MouseEventKind::Down(event::MouseButton::Left) => {
                                    if let Some(tab) = app.tab_at_position(mouse.column, mouse.row)
                                    {
                                        app.active_tab = tab;
                                    } else if app.active_tab == app::ActiveTab::Triggers {
                                        // Single click selects trigger row
                                        if let Some(idx) = app.issue_row_at_position(mouse.row) {
                                            app.issues_state.select(Some(idx));
                                        }
                                        // Double-click opens detail modal
                                        let now = Instant::now();
                                        let is_double = app.last_click_at.is_some_and(|prev| {
                                            now.duration_since(prev) < Duration::from_millis(400)
                                                && app.last_click_pos.1 == mouse.row
                                        });
                                        if is_double && app.selected_trigger(&snapshot).is_some() {
                                            app.show_issue_detail = true;
                                            app.detail_scroll = 0;
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
                                        if is_double && app.selected_task(&snapshot).is_some() {
                                            app.show_task_detail = true;
                                            app.task_detail_scroll = 0;
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
                    if app.leaving {
                        // Ignore keys while leaving
                    } else if app.show_agent_picker {
                        match key.code {
                            KeyCode::Esc => {
                                app.show_agent_picker = false;
                                app.agent_picker_issue_id = None;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                let count = snapshot.agent_profile_names.len();
                                if count > 0 {
                                    app.agent_picker_selected =
                                        (app.agent_picker_selected + 1) % count;
                                }
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                let count = snapshot.agent_profile_names.len();
                                if count > 0 {
                                    app.agent_picker_selected =
                                        (app.agent_picker_selected + count - 1) % count;
                                }
                            },
                            KeyCode::Enter => {
                                if let Some(issue_id) = app.agent_picker_issue_id.take() {
                                    let agent_name = snapshot
                                        .agent_profile_names
                                        .get(app.agent_picker_selected)
                                        .cloned();
                                    app.show_agent_picker = false;
                                    let _ = command_tx.send(RuntimeCommand::DispatchIssue {
                                        issue_id,
                                        agent_name,
                                    });
                                }
                            },
                            _ => {},
                        }
                    } else if app.show_mode_modal {
                        match key.code {
                            KeyCode::Esc => {
                                app.show_mode_modal = false;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.mode_modal_selected = (app.mode_modal_selected + 1) % 4;
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.mode_modal_selected = (app.mode_modal_selected + 3) % 4;
                            },
                            KeyCode::Enter => {
                                let modes = [
                                    DispatchMode::Manual,
                                    DispatchMode::Automatic,
                                    DispatchMode::Nightshift,
                                    DispatchMode::Idle,
                                ];
                                let selected = modes[app.mode_modal_selected];
                                app.show_mode_modal = false;
                                let _ = command_tx.send(RuntimeCommand::SetMode(selected));
                            },
                            _ => {},
                        }
                    } else if app.show_issue_detail {
                        // Modal is open — handle modal keys
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                                app.show_issue_detail = false;
                                app.detail_scroll = 0;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.detail_scroll = app.detail_scroll.saturating_add(1);
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.detail_scroll = app.detail_scroll.saturating_sub(1);
                            },
                            KeyCode::PageDown => {
                                app.detail_scroll = app.detail_scroll.saturating_add(8);
                            },
                            KeyCode::PageUp => {
                                app.detail_scroll = app.detail_scroll.saturating_sub(8);
                            },
                            KeyCode::Char('o') => {
                                if let Some(trigger) = app.selected_trigger(&snapshot)
                                    && let Some(url) = &trigger.url
                                {
                                    let _ = std::process::Command::new("open").arg(url).spawn();
                                }
                            },
                            KeyCode::Char('a') => {
                                if let Some(trigger) = app.selected_trigger(&snapshot)
                                    && trigger.kind == VisibleTriggerKind::Issue
                                    && trigger.approval_state
                                        == polyphony_core::IssueApprovalState::Waiting
                                {
                                    let _ = command_tx.send(RuntimeCommand::ApproveIssueTrigger {
                                        issue_id: trigger.trigger_id.clone(),
                                        source: trigger.source.clone(),
                                    });
                                }
                            },
                            KeyCode::Char('d') => {
                                if let Some(trigger) = app.selected_trigger(&snapshot) {
                                    let command = match trigger.kind {
                                        VisibleTriggerKind::Issue => {
                                            RuntimeCommand::DispatchIssue {
                                                issue_id: trigger.trigger_id.clone(),
                                                agent_name: None,
                                            }
                                        },
                                        VisibleTriggerKind::PullRequestReview
                                        | VisibleTriggerKind::PullRequestComment
                                        | VisibleTriggerKind::PullRequestConflict => {
                                            RuntimeCommand::DispatchPullRequestTrigger {
                                                trigger_id: trigger.trigger_id.clone(),
                                            }
                                        },
                                    };
                                    let _ = command_tx.send(command);
                                }
                            },
                            _ => {},
                        }
                    } else if app.show_task_detail {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                                app.show_task_detail = false;
                                app.task_detail_scroll = 0;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.task_detail_scroll = app.task_detail_scroll.saturating_add(1);
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.task_detail_scroll = app.task_detail_scroll.saturating_sub(1);
                            },
                            KeyCode::PageDown => {
                                app.task_detail_scroll = app.task_detail_scroll.saturating_add(8);
                            },
                            KeyCode::PageUp => {
                                app.task_detail_scroll = app.task_detail_scroll.saturating_sub(8);
                            },
                            _ => {},
                        }
                    } else if app.show_movement_detail {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                                app.show_movement_detail = false;
                                app.movement_detail_scroll = 0;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.movement_detail_scroll =
                                    app.movement_detail_scroll.saturating_add(1);
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.movement_detail_scroll =
                                    app.movement_detail_scroll.saturating_sub(1);
                            },
                            KeyCode::PageDown => {
                                app.movement_detail_scroll =
                                    app.movement_detail_scroll.saturating_add(8);
                            },
                            KeyCode::PageUp => {
                                app.movement_detail_scroll =
                                    app.movement_detail_scroll.saturating_sub(8);
                            },
                            KeyCode::Char('O') => {
                                if let Some(movement) = app.selected_movement(&snapshot) {
                                    let url = movement
                                        .deliverable
                                        .as_ref()
                                        .and_then(|d| d.url.as_deref())
                                        .or_else(|| {
                                            movement
                                                .review_target
                                                .as_ref()
                                                .and_then(|t| t.url.as_deref())
                                        });
                                    if let Some(url) = url {
                                        open_url(url);
                                    }
                                }
                            },
                            KeyCode::Char('a') => {
                                if let Some(movement) = app.selected_movement(&snapshot)
                                    && movement.deliverable.is_some()
                                {
                                    let _ =
                                        command_tx.send(RuntimeCommand::ResolveMovementDeliverable {
                                            movement_id: movement.id.clone(),
                                            decision: polyphony_core::DeliverableDecision::Accepted,
                                        });
                                }
                            },
                            KeyCode::Char('x') => {
                                if let Some(movement) = app.selected_movement(&snapshot)
                                    && movement.deliverable.is_some()
                                {
                                    let _ =
                                        command_tx.send(RuntimeCommand::ResolveMovementDeliverable {
                                            movement_id: movement.id.clone(),
                                            decision: polyphony_core::DeliverableDecision::Rejected,
                                        });
                                }
                            },
                            _ => {},
                        }
                    } else if app.show_deliverable_detail {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                                app.show_deliverable_detail = false;
                                app.deliverable_detail_scroll = 0;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.deliverable_detail_scroll =
                                    app.deliverable_detail_scroll.saturating_add(1);
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.deliverable_detail_scroll =
                                    app.deliverable_detail_scroll.saturating_sub(1);
                            },
                            KeyCode::PageDown => {
                                app.deliverable_detail_scroll =
                                    app.deliverable_detail_scroll.saturating_add(8);
                            },
                            KeyCode::PageUp => {
                                app.deliverable_detail_scroll =
                                    app.deliverable_detail_scroll.saturating_sub(8);
                            },
                            KeyCode::Char('O') => {
                                if let Some(movement) = app.selected_deliverable(&snapshot)
                                    && let Some(deliverable) = &movement.deliverable
                                    && let Some(url) = &deliverable.url
                                {
                                    open_url(url);
                                }
                            },
                            KeyCode::Char('a') => {
                                if let Some(movement) = app.selected_deliverable(&snapshot) {
                                    let _ =
                                        command_tx.send(RuntimeCommand::ResolveMovementDeliverable {
                                            movement_id: movement.id.clone(),
                                            decision: polyphony_core::DeliverableDecision::Accepted,
                                        });
                                }
                            },
                            KeyCode::Char('x') => {
                                if let Some(movement) = app.selected_deliverable(&snapshot) {
                                    let _ =
                                        command_tx.send(RuntimeCommand::ResolveMovementDeliverable {
                                            movement_id: movement.id.clone(),
                                            decision: polyphony_core::DeliverableDecision::Rejected,
                                        });
                                }
                            },
                            _ => {},
                        }
                    } else if app.show_agent_detail {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                                app.show_agent_detail = false;
                                app.agents_detail_scroll = 0;
                            },
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.agents_detail_scroll =
                                    app.agents_detail_scroll.saturating_add(1);
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.agents_detail_scroll =
                                    app.agents_detail_scroll.saturating_sub(1);
                            },
                            KeyCode::PageDown => {
                                app.agents_detail_scroll =
                                    app.agents_detail_scroll.saturating_add(8);
                            },
                            KeyCode::PageUp => {
                                app.agents_detail_scroll =
                                    app.agents_detail_scroll.saturating_sub(8);
                            },
                            _ => {},
                        }
                    } else if app.search_active {
                        match key.code {
                            KeyCode::Esc => {
                                app.search_active = false;
                                app.search_query.clear();
                                app.rebuild_sorted_indices(&snapshot);
                                sync_selection_after_search(&mut app, &snapshot);
                            },
                            KeyCode::Enter => {
                                app.search_active = false;
                                // Keep filter active, just exit input mode
                            },
                            KeyCode::Backspace => {
                                app.search_query.pop();
                                app.rebuild_sorted_indices(&snapshot);
                                sync_selection_after_search(&mut app, &snapshot);
                            },
                            KeyCode::Char(c) => {
                                app.search_query.push(c);
                                app.rebuild_sorted_indices(&snapshot);
                                sync_selection_after_search(&mut app, &snapshot);
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
                    } else if let Some(command) = handle_key(&mut app, key.code, &snapshot) {
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
                    key_handled = true;
                },
                _ => {},
            }
        }

        refresh_agent_detail_artifact(&mut app, &snapshot).await;

        // Always check for snapshot updates, whether or not a key was handled.
        // Use a short timeout so the draw loop stays responsive.
        tokio::select! {
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
                snapshot = snapshot_rx.borrow().clone();
                app.on_snapshot(&snapshot);
                refresh_agent_detail_artifact(&mut app, &snapshot).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(if key_handled { 1 } else { 100 })) => {}
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
        app.agent_detail_artifact = None;
        return;
    };
    if app
        .agent_detail_artifact
        .as_ref()
        .is_some_and(|artifact| artifact.key == agent_artifact_request_key(&request))
    {
        return;
    }
    let key = agent_artifact_request_key(&request);
    let loaded = tokio::task::spawn_blocking(move || load_agent_artifact(request))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    app.agent_detail_artifact = Some(crate::app::AgentDetailArtifactCache {
        key,
        saved_context: loaded,
    });
}

fn selected_agent_artifact_request(
    app: &AppState,
    snapshot: &RuntimeSnapshot,
) -> Option<AgentArtifactRequest> {
    match app.selected_agent(snapshot)? {
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
        } => Ok(polyphony_core::load_workspace_run_history_record(
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
        KeyCode::Char('q') => return Some(RuntimeCommand::Shutdown),
        KeyCode::Char('r') => return Some(RuntimeCommand::Refresh),

        // Tab switching
        KeyCode::Tab | KeyCode::Right => {
            app.active_tab = app.active_tab.next();
        },
        KeyCode::BackTab | KeyCode::Left => {
            app.active_tab = app.active_tab.previous();
        },
        KeyCode::Char('1') => app.active_tab = app::ActiveTab::Triggers,
        KeyCode::Char('2') => app.active_tab = app::ActiveTab::Orchestrator,
        KeyCode::Char('3') => app.active_tab = app::ActiveTab::Tasks,
        KeyCode::Char('4') => app.active_tab = app::ActiveTab::Deliverables,
        KeyCode::Char('5') => app.active_tab = app::ActiveTab::Agents,
        KeyCode::Char('6') => app.active_tab = app::ActiveTab::Logs,
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

        // Jump to bottom (Logs: re-enable auto-scroll)
        KeyCode::Char('G') | KeyCode::End => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = true;
                let len = app.active_table_len(snapshot);
                if len > 0 {
                    app.logs_state.select(Some(len - 1));
                }
            }
        },

        // Jump to top (Logs: disable auto-scroll)
        KeyCode::Char('g') | KeyCode::Home => {
            if app.active_tab == app::ActiveTab::Logs {
                app.logs_auto_scroll = false;
                let len = app.active_table_len(snapshot);
                if len > 0 {
                    app.logs_state.select(Some(0));
                }
            }
        },

        // Sort cycling (Triggers tab)
        KeyCode::Char('s') => {
            if app.active_tab == app::ActiveTab::Triggers {
                app.issue_sort = app.issue_sort.cycle();
                app.rebuild_sorted_indices(snapshot);
            }
        },

        // Detail modal (Enter opens for active tab)
        KeyCode::Enter => {
            if app.active_tab == app::ActiveTab::Triggers
                && app.selected_trigger(snapshot).is_some()
            {
                app.show_issue_detail = true;
            } else if app.active_tab == app::ActiveTab::Tasks
                && app.selected_task(snapshot).is_some()
            {
                app.show_task_detail = true;
            } else if app.active_tab == app::ActiveTab::Orchestrator
                && app.selected_movement(snapshot).is_some()
            {
                app.show_movement_detail = true;
                app.movement_detail_scroll = 0;
            } else if app.active_tab == app::ActiveTab::Deliverables
                && app.selected_deliverable(snapshot).is_some()
            {
                app.show_deliverable_detail = true;
                app.deliverable_detail_scroll = 0;
            } else if app.active_tab == app::ActiveTab::Agents
                && app.selected_agent(snapshot).is_some()
            {
                app.show_agent_detail = true;
                app.agents_detail_scroll = 0;
            }
        },

        // Open trigger in browser
        KeyCode::Char('o') => {
            if app.active_tab == app::ActiveTab::Triggers
                && let Some(trigger) = app.selected_trigger(snapshot)
                && let Some(url) = &trigger.url
            {
                open_url(url);
            }
        },
        KeyCode::Char('O') => {
            let url = match app.active_tab {
                app::ActiveTab::Deliverables => app
                    .selected_deliverable(snapshot)
                    .and_then(|movement| movement.deliverable.as_ref())
                    .and_then(|deliverable| deliverable.url.as_deref()),
                app::ActiveTab::Orchestrator => {
                    app.selected_movement(snapshot).and_then(|movement| {
                        movement
                            .deliverable
                            .as_ref()
                            .and_then(|deliverable| deliverable.url.as_deref())
                            .or_else(|| {
                                movement
                                    .review_target
                                    .as_ref()
                                    .and_then(|target| target.url.as_deref())
                            })
                    })
                },
                _ => None,
            };
            if let Some(url) = url {
                open_url(url);
            }
        },

        // Search
        KeyCode::Char('/') => {
            if app.active_tab == app::ActiveTab::Triggers {
                app.search_active = true;
                app.search_query.clear();
            } else if app.active_tab == app::ActiveTab::Logs {
                app.logs_search_active = true;
                app.logs_search_query.clear();
            }
        },

        // Dispatch selected trigger
        KeyCode::Char('d') => {
            if app.active_tab == app::ActiveTab::Triggers
                && let Some(trigger) = app.selected_trigger(snapshot)
            {
                return Some(match trigger.kind {
                    VisibleTriggerKind::Issue => RuntimeCommand::DispatchIssue {
                        issue_id: trigger.trigger_id.clone(),
                        agent_name: None,
                    },
                    VisibleTriggerKind::PullRequestReview
                    | VisibleTriggerKind::PullRequestComment
                    | VisibleTriggerKind::PullRequestConflict => {
                        RuntimeCommand::DispatchPullRequestTrigger {
                            trigger_id: trigger.trigger_id.clone(),
                        }
                    },
                });
            }
        },
        KeyCode::Char('a') => {
            if app.active_tab == app::ActiveTab::Triggers
                && let Some(trigger) = app.selected_trigger(snapshot)
                && trigger.kind == VisibleTriggerKind::Issue
                && trigger.approval_state == polyphony_core::IssueApprovalState::Waiting
            {
                return Some(RuntimeCommand::ApproveIssueTrigger {
                    issue_id: trigger.trigger_id.clone(),
                    source: trigger.source.clone(),
                });
            }
            if let Some(movement) = selected_deliverable_movement(app, snapshot) {
                return Some(RuntimeCommand::ResolveMovementDeliverable {
                    movement_id: movement.id.clone(),
                    decision: polyphony_core::DeliverableDecision::Accepted,
                });
            }
        },

        // Dispatch issue (pick agent)
        KeyCode::Char('D') => {
            if app.active_tab == app::ActiveTab::Triggers
                && let Some(issue) = app.selected_trigger(snapshot)
                && issue.kind == VisibleTriggerKind::Issue
                && !snapshot.agent_profile_names.is_empty()
            {
                app.show_agent_picker = true;
                app.agent_picker_selected = 0;
                app.agent_picker_issue_id = Some(issue.trigger_id.clone());
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
            };
        },
        KeyCode::Char('x') => {
            if let Some(movement) = selected_deliverable_movement(app, snapshot) {
                return Some(RuntimeCommand::ResolveMovementDeliverable {
                    movement_id: movement.id.clone(),
                    decision: polyphony_core::DeliverableDecision::Rejected,
                });
            }
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

fn selected_deliverable_movement<'a>(
    app: &AppState,
    snapshot: &'a RuntimeSnapshot,
) -> Option<&'a polyphony_core::MovementRow> {
    match app.active_tab {
        app::ActiveTab::Deliverables => app.selected_deliverable(snapshot),
        app::ActiveTab::Orchestrator => app
            .selected_movement(snapshot)
            .filter(|movement| movement.deliverable.is_some()),
        _ => None,
    }
}

fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
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

    if app.show_task_detail {
        if scrolling_down {
            app.task_detail_scroll = app.task_detail_scroll.saturating_add(1);
        } else {
            app.task_detail_scroll = app.task_detail_scroll.saturating_sub(1);
        }
        return;
    }

    if app.show_issue_detail {
        if scrolling_down {
            app.detail_scroll = app.detail_scroll.saturating_add(1);
        } else {
            app.detail_scroll = app.detail_scroll.saturating_sub(1);
        }
        return;
    }

    if app.active_tab == app::ActiveTab::Orchestrator
        && mouse_in_rect(mouse.column, mouse.row, app.movement_detail_area)
    {
        if scrolling_down {
            app.movement_detail_scroll = app.movement_detail_scroll.saturating_add(1);
        } else {
            app.movement_detail_scroll = app.movement_detail_scroll.saturating_sub(1);
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
    use {
        crate::{LogBuffer, event_loop::AgentArtifactRequest},
        chrono::Utc,
        crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind},
        polyphony_core::{
            AgentContextEntry, AgentContextSnapshot, AgentEventKind, AttemptStatus, Deliverable,
            DeliverableDecision, DeliverableKind, DeliverableStatus, IssueApprovalState,
            PersistedRunRecord, RuntimeSnapshot, SnapshotCounts, TokenUsage, VisibleTriggerKind,
            VisibleTriggerRow, workspace_run_history_artifact_path,
            workspace_saved_context_artifact_path,
        },
    };

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
            Some(polyphony_orchestrator::RuntimeCommand::ResolveMovementDeliverable {
                movement_id,
                decision: polyphony_core::DeliverableDecision::Accepted,
            }) if movement_id == "mov-1"
        ));
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
            Some(polyphony_orchestrator::RuntimeCommand::ResolveMovementDeliverable {
                movement_id,
                decision: polyphony_core::DeliverableDecision::Rejected,
            }) if movement_id == "mov-1"
        ));
    }

    #[test]
    fn triggers_tab_approves_waiting_issue() {
        let mut app =
            crate::app::AppState::new(crate::theme::default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot_with_deliverable();
        snapshot.visible_triggers = vec![VisibleTriggerRow {
            trigger_id: "7".into(),
            kind: VisibleTriggerKind::Issue,
            source: "github".into(),
            identifier: "#7".into(),
            title: "Untrusted issue".into(),
            status: "Todo".into(),
            approval_state: IssueApprovalState::Waiting,
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
        app.active_tab = crate::app::ActiveTab::Triggers;

        let command = crate::event_loop::handle_key(&mut app, KeyCode::Char('a'), &snapshot);

        assert!(matches!(
            command,
            Some(polyphony_orchestrator::RuntimeCommand::ApproveIssueTrigger {
                issue_id,
                source,
            }) if issue_id == "7" && source == "github"
        ));
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
        let record = PersistedRunRecord {
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
            workspace_run_history_artifact_path(&workspace),
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
            generated_at: Utc::now(),
            counts: SnapshotCounts::default(),
            cadence: Default::default(),
            visible_issues: vec![],
            visible_triggers: vec![],
            approved_issue_keys: vec![],
            running: vec![],
            agent_history: vec![],
            retrying: vec![],
            codex_totals: Default::default(),
            rate_limits: None,
            throttles: vec![],
            budgets: vec![],
            agent_catalogs: vec![],
            saved_contexts: vec![],
            recent_events: vec![],
            movements: vec![polyphony_core::MovementRow {
                id: "mov-1".into(),
                kind: polyphony_core::MovementKind::IssueDelivery,
                issue_identifier: Some("#7".into()),
                title: "Ship PR".into(),
                status: polyphony_core::MovementStatus::Delivered,
                task_count: 1,
                tasks_completed: 1,
                deliverable: Some(Deliverable {
                    kind: DeliverableKind::GithubPullRequest,
                    status: DeliverableStatus::Open,
                    url: Some("https://github.com/penso/polyphony/pull/8".into()),
                    decision: DeliverableDecision::Waiting,
                }),
                has_deliverable: true,
                review_target: None,
                workspace_key: None,
                workspace_path: None,
                created_at: Utc::now(),
            }],
            tasks: vec![],
            loading: Default::default(),
            dispatch_mode: Default::default(),
            tracker_kind: Default::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: vec![],
        }
    }
}
