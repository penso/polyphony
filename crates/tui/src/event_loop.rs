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
                        if app.has_detail() {
                            // Click outside modal closes it
                            if mouse.kind == MouseEventKind::Down(event::MouseButton::Left) {
                                app.pop_detail();
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
                                        if is_double
                                            && let Some(trigger) = app.selected_trigger(&snapshot)
                                        {
                                            app.push_detail(
                                                crate::app::DetailView::Trigger {
                                                    trigger_id: trigger.trigger_id.clone(),
                                                    scroll: 0,
                                                    focus: Default::default(),
                                                    movements_selected: 0,
                                                    agents_selected: 0,
                                                },
                                            );
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
                                        if is_double
                                            && let Some(task) = app.selected_task(&snapshot)
                                        {
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
                    } else if app.has_detail()
                        && app.split_focus == crate::app::SplitFocus::Detail
                    {
                        if let Some(cmd) =
                            handle_detail_key(&mut app, key.code, &snapshot, &command_tx)
                        {
                            let _ = command_tx.send(cmd);
                        }
                    } else if app.has_detail()
                        && app.split_focus == crate::app::SplitFocus::List
                    {
                        // Split mode, list focused: route to list handler
                        // but Tab toggles focus, Esc closes the detail
                        match key.code {
                            KeyCode::Tab => {
                                app.split_focus = crate::app::SplitFocus::Detail;
                            },
                            KeyCode::Esc => {
                                app.pop_detail();
                                app.split_focus = crate::app::SplitFocus::default();
                            },
                            KeyCode::Enter => {
                                // In split mode the detail pane already shows the
                                // selected item — Enter is a no-op to avoid pushing
                                // duplicate details onto the stack.
                            },
                            _ => {
                                if let Some(command) =
                                    handle_key(&mut app, key.code, &snapshot)
                                {
                                    let shutdown =
                                        matches!(command, RuntimeCommand::Shutdown);
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
                                // After list navigation, update the detail entry
                                update_split_detail_from_selection(&mut app, &snapshot);
                            },
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
        // Clear artifact cache on the current agent detail if present
        if let Some(crate::app::DetailView::Agent {
            artifact_cache, ..
        }) = app.current_detail_mut()
        {
            **artifact_cache = None;
        }
        return;
    };
    // Check if we already have a matching cache
    let existing_key = if let Some(crate::app::DetailView::Agent {
        artifact_cache, ..
    }) = app.current_detail()
    {
        artifact_cache
            .as_ref()
            .as_ref()
            .map(|a| a.key.clone())
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
    if let Some(crate::app::DetailView::Agent {
        artifact_cache, ..
    }) = app.current_detail_mut()
    {
        **artifact_cache = Some(crate::app::AgentDetailArtifactCache {
            key: request_key,
            saved_context: loaded,
        });
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
            .agent_history
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
            app.clear_detail_stack();
            app.active_tab = app.active_tab.next();
        },
        KeyCode::BackTab | KeyCode::Left => {
            app.clear_detail_stack();
            app.active_tab = app.active_tab.previous();
        },
        KeyCode::Char('1') => {
            app.clear_detail_stack();
            app.active_tab = app::ActiveTab::Triggers;
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
            app.active_tab = app::ActiveTab::Logs;
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

        // Detail view (Enter opens for active tab)
        KeyCode::Enter => {
            if app.active_tab == app::ActiveTab::Triggers
                && let Some(trigger) = app.selected_trigger(snapshot)
            {
                app.push_detail(crate::app::DetailView::Trigger {
                    trigger_id: trigger.trigger_id.clone(),
                    scroll: 0,
                    focus: Default::default(),
                    movements_selected: 0,
                    agents_selected: 0,
                });
            } else if app.active_tab == app::ActiveTab::Tasks
                && let Some(task) = app.selected_task(snapshot)
            {
                app.push_detail(crate::app::DetailView::Task {
                    task_id: task.id.clone(),
                    scroll: 0,
                });
            } else if app.active_tab == app::ActiveTab::Orchestrator
                && let Some(movement) = app.selected_movement(snapshot)
            {
                app.push_detail(crate::app::DetailView::Movement {
                    movement_id: movement.id.clone(),
                    scroll: 0,
                    focus: Default::default(),
                    tasks_selected: 0,
                });
            } else if app.active_tab == app::ActiveTab::Deliverables
                && let Some(movement) = app.selected_deliverable(snapshot)
            {
                app.push_detail(crate::app::DetailView::Deliverable {
                    movement_id: movement.id.clone(),
                    scroll: 0,
                });
            } else if app.active_tab == app::ActiveTab::Agents
                && let Some(index) = app.agents_state.selected()
            {
                app.push_detail(crate::app::DetailView::Agent {
                    agent_index: index,
                    scroll: 0,
                    artifact_cache: Box::new(None),
                });
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
                DispatchMode::Stop => 4,
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

/// Handle key events when a detail view is on the stack.
/// Returns an optional command to send.
fn handle_detail_key(
    app: &mut AppState,
    key: KeyCode,
    snapshot: &RuntimeSnapshot,
    _command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> Option<RuntimeCommand> {
    // In split mode, Esc switches focus to list; a second Esc closes the detail
    let in_split = app.is_split_eligible()
        && app.split_focus == crate::app::SplitFocus::Detail;
    match key {
        KeyCode::Esc => {
            if in_split {
                app.split_focus = crate::app::SplitFocus::List;
            } else {
                app.pop_detail();
            }
            return None;
        },
        KeyCode::Char('q') => {
            app.pop_detail();
            return None;
        },
        _ => {},
    }

    // Dispatch based on current detail variant
    let detail = app.current_detail().cloned()?;

    match detail {
        crate::app::DetailView::Trigger {
            ref trigger_id,
            focus,
            ..
        } => match key {
                KeyCode::Tab => {
                    // Cycle: Body -> Section(0) movements -> Section(1) agents -> Body
                    if let Some(crate::app::DetailView::Trigger { focus, .. }) =
                        app.current_detail_mut()
                    {
                        *focus = match *focus {
                            crate::app::DetailSection::Body => {
                                crate::app::DetailSection::Section(0)
                            },
                            crate::app::DetailSection::Section(0) => {
                                crate::app::DetailSection::Section(1)
                            },
                            _ => crate::app::DetailSection::Body,
                        };
                    }
                },
                KeyCode::Enter => {
                    // Drill down into selected section item
                    match focus {
                        crate::app::DetailSection::Section(0) => {
                            // Movements section — push Movement detail
                            if let Some(crate::app::DetailView::Trigger {
                                movements_selected,
                                trigger_id,
                                ..
                            }) = app.current_detail().cloned()
                            {
                                let trigger = find_trigger_by_id(snapshot, &trigger_id);
                                let related: Vec<_> = snapshot
                                    .movements
                                    .iter()
                                    .filter(|m| {
                                        trigger.is_some_and(|t| {
                                            m.issue_identifier.as_deref() == Some(&*t.identifier)
                                        })
                                    })
                                    .collect();
                                if let Some(movement) = related.get(movements_selected) {
                                    app.push_detail(crate::app::DetailView::Movement {
                                        movement_id: movement.id.clone(),
                                        scroll: 0,
                                        focus: Default::default(),
                                        tasks_selected: 0,
                                    });
                                }
                            }
                        },
                        crate::app::DetailSection::Section(1) => {
                            // Agents section — push Agent detail
                            if let Some(crate::app::DetailView::Trigger {
                                agents_selected,
                                trigger_id,
                                ..
                            }) = app.current_detail().cloned()
                            {
                                let running_agents: Vec<_> = snapshot
                                    .running
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, r)| r.issue_id == trigger_id)
                                    .collect();
                                if let Some(&(idx, _)) = running_agents.get(agents_selected) {
                                    app.push_detail(crate::app::DetailView::Agent {
                                        agent_index: idx,
                                        scroll: 0,
                                        artifact_cache: Box::new(None),
                                    });
                                }
                            }
                        },
                        _ => {},
                    }
                },
                KeyCode::Char('j') | KeyCode::Down => {
                    navigate_section_or_scroll(app, snapshot, focus, 1, true);
                },
                KeyCode::Char('k') | KeyCode::Up => {
                    navigate_section_or_scroll(app, snapshot, focus, 1, false);
                },
                KeyCode::PageDown => {
                    scroll_detail(app, 8);
                },
                KeyCode::PageUp => {
                    scroll_detail_back(app, 8);
                },
                KeyCode::Char('o') => {
                    if let Some(trigger) = find_trigger_by_id(snapshot, trigger_id)
                        && let Some(url) = &trigger.url
                    {
                        open_url(url);
                    }
                },
                KeyCode::Char('a') => {
                    if let Some(trigger) = find_trigger_by_id(snapshot, trigger_id)
                        && trigger.kind == VisibleTriggerKind::Issue
                        && trigger.approval_state == polyphony_core::IssueApprovalState::Waiting
                    {
                        return Some(RuntimeCommand::ApproveIssueTrigger {
                            issue_id: trigger.trigger_id.clone(),
                            source: trigger.source.clone(),
                        });
                    }
                },
                KeyCode::Char('d') => {
                    if let Some(trigger) = find_trigger_by_id(snapshot, trigger_id) {
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
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    if let Some(trigger) = find_trigger_by_id(snapshot, trigger_id) {
                        app.push_detail(crate::app::DetailView::Events {
                            filter: trigger.identifier.clone(),
                            scroll: u16::MAX,
                        });
                    }
                },
                _ => {},
            },
        crate::app::DetailView::Movement {
            ref movement_id,
            focus,
            ..
        } => match key {
                KeyCode::Tab => {
                    // Cycle: Body -> Section(0) tasks -> Body
                    if let Some(crate::app::DetailView::Movement { focus, .. }) =
                        app.current_detail_mut()
                    {
                        *focus = match *focus {
                            crate::app::DetailSection::Body => {
                                crate::app::DetailSection::Section(0)
                            },
                            _ => crate::app::DetailSection::Body,
                        };
                    }
                },
                KeyCode::Enter => {
                    if let crate::app::DetailSection::Section(0) = focus {
                        // Tasks section — push Task detail
                        if let Some(crate::app::DetailView::Movement {
                            tasks_selected,
                            movement_id,
                            ..
                        }) = app.current_detail().cloned()
                        {
                            let related: Vec<_> = snapshot
                                .tasks
                                .iter()
                                .filter(|t| t.movement_id == movement_id)
                                .collect();
                            if let Some(task) = related.get(tasks_selected) {
                                app.push_detail(crate::app::DetailView::Task {
                                    task_id: task.id.clone(),
                                    scroll: 0,
                                });
                            }
                        }
                    }
                },
                KeyCode::Char('j') | KeyCode::Down => {
                    navigate_movement_section_or_scroll(app, snapshot, focus, true);
                },
                KeyCode::Char('k') | KeyCode::Up => {
                    navigate_movement_section_or_scroll(app, snapshot, focus, false);
                },
                KeyCode::PageDown => {
                    scroll_detail(app, 8);
                },
                KeyCode::PageUp => {
                    scroll_detail_back(app, 8);
                },
                KeyCode::Char('O') => {
                    if let Some(movement) = find_movement_by_id(snapshot, movement_id) {
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
                    if let Some(movement) = find_movement_by_id(snapshot, movement_id)
                        && movement.deliverable.is_some()
                    {
                        return Some(RuntimeCommand::ResolveMovementDeliverable {
                            movement_id: movement.id.clone(),
                            decision: polyphony_core::DeliverableDecision::Accepted,
                        });
                    }
                },
                KeyCode::Char('x') => {
                    if let Some(movement) = find_movement_by_id(snapshot, movement_id)
                        && movement.deliverable.is_some()
                    {
                        return Some(RuntimeCommand::ResolveMovementDeliverable {
                            movement_id: movement.id.clone(),
                            decision: polyphony_core::DeliverableDecision::Rejected,
                        });
                    }
                },
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    if let Some(movement) = find_movement_by_id(snapshot, movement_id) {
                        let filter = movement
                            .issue_identifier
                            .clone()
                            .unwrap_or_else(|| movement.id.clone());
                        app.push_detail(crate::app::DetailView::Events {
                            filter,
                            scroll: u16::MAX,
                        });
                    }
                },
                _ => {},
            },
        crate::app::DetailView::Task { .. } => match key {
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
            _ => {},
        },
        crate::app::DetailView::Deliverable {
            ref movement_id, ..
        } => match key {
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
                if let Some(movement) = find_movement_by_id(snapshot, movement_id)
                    && let Some(deliverable) = &movement.deliverable
                    && let Some(url) = &deliverable.url
                {
                    open_url(url);
                }
            },
            KeyCode::Char('a') => {
                if let Some(movement) = find_movement_by_id(snapshot, movement_id) {
                    return Some(RuntimeCommand::ResolveMovementDeliverable {
                        movement_id: movement.id.clone(),
                        decision: polyphony_core::DeliverableDecision::Accepted,
                    });
                }
            },
            KeyCode::Char('x') => {
                if let Some(movement) = find_movement_by_id(snapshot, movement_id) {
                    return Some(RuntimeCommand::ResolveMovementDeliverable {
                        movement_id: movement.id.clone(),
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
            KeyCode::Char('G') | KeyCode::End => {
                *app.current_detail_mut()?.scroll_mut() = u16::MAX;
            },
            KeyCode::Char('g') | KeyCode::Home => {
                *app.current_detail_mut()?.scroll_mut() = 0;
            },
            _ => {},
        },
    }
    None
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

/// Navigate within a Trigger detail view: when a section is focused, j/k moves
/// the mini-list selection; when Body is focused, j/k scrolls the page.
fn navigate_section_or_scroll(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    focus: crate::app::DetailSection,
    amount: u16,
    down: bool,
) {
    match focus {
        crate::app::DetailSection::Body => {
            if down {
                scroll_detail(app, amount);
            } else {
                scroll_detail_back(app, amount);
            }
        },
        crate::app::DetailSection::Section(0) => {
            // Movements mini-list
            if let Some(crate::app::DetailView::Trigger {
                ref trigger_id,
                movements_selected,
                ..
            }) = app.current_detail().cloned()
            {
                let trigger = find_trigger_by_id(snapshot, trigger_id);
                let count = snapshot
                    .movements
                    .iter()
                    .filter(|m| {
                        trigger.is_some_and(|t| {
                            m.issue_identifier.as_deref() == Some(&*t.identifier)
                        })
                    })
                    .count();
                if count > 0 {
                    let new_sel = if down {
                        (movements_selected + 1).min(count - 1)
                    } else {
                        movements_selected.saturating_sub(1)
                    };
                    if let Some(crate::app::DetailView::Trigger {
                        movements_selected, ..
                    }) = app.current_detail_mut()
                    {
                        *movements_selected = new_sel;
                    }
                }
            }
        },
        crate::app::DetailSection::Section(1) => {
            // Agents mini-list
            if let Some(crate::app::DetailView::Trigger {
                ref trigger_id,
                agents_selected,
                ..
            }) = app.current_detail().cloned()
            {
                let count = snapshot
                    .running
                    .iter()
                    .filter(|r| r.issue_id == *trigger_id)
                    .count();
                if count > 0 {
                    let new_sel = if down {
                        (agents_selected + 1).min(count - 1)
                    } else {
                        agents_selected.saturating_sub(1)
                    };
                    if let Some(crate::app::DetailView::Trigger {
                        agents_selected, ..
                    }) = app.current_detail_mut()
                    {
                        *agents_selected = new_sel;
                    }
                }
            }
        },
        _ => {},
    }
}

/// Navigate within a Movement detail view: when tasks section is focused, j/k
/// moves the mini-list selection; when Body is focused, j/k scrolls the page.
fn navigate_movement_section_or_scroll(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    focus: crate::app::DetailSection,
    down: bool,
) {
    match focus {
        crate::app::DetailSection::Body => {
            if down {
                scroll_detail(app, 1);
            } else {
                scroll_detail_back(app, 1);
            }
        },
        crate::app::DetailSection::Section(0) => {
            // Tasks mini-list
            if let Some(crate::app::DetailView::Movement {
                ref movement_id,
                tasks_selected,
                ..
            }) = app.current_detail().cloned()
            {
                let count = snapshot
                    .tasks
                    .iter()
                    .filter(|t| t.movement_id == *movement_id)
                    .count();
                if count > 0 {
                    let new_sel = if down {
                        (tasks_selected + 1).min(count - 1)
                    } else {
                        tasks_selected.saturating_sub(1)
                    };
                    if let Some(crate::app::DetailView::Movement {
                        tasks_selected, ..
                    }) = app.current_detail_mut()
                    {
                        *tasks_selected = new_sel;
                    }
                }
            }
        },
        _ => {},
    }
}

fn find_trigger_by_id<'a>(
    snapshot: &'a RuntimeSnapshot,
    trigger_id: &str,
) -> Option<&'a polyphony_core::VisibleTriggerRow> {
    snapshot
        .visible_triggers
        .iter()
        .find(|t| t.trigger_id == trigger_id)
}

fn find_movement_by_id<'a>(
    snapshot: &'a RuntimeSnapshot,
    movement_id: &str,
) -> Option<&'a polyphony_core::MovementRow> {
    snapshot.movements.iter().find(|m| m.id == movement_id)
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

/// In split mode, after navigating the list, replace the single detail stack
/// entry with the newly selected entity so the right pane updates live.
fn update_split_detail_from_selection(app: &mut AppState, snapshot: &RuntimeSnapshot) {
    if app.detail_stack.len() != 1 {
        return;
    }
    let new_detail = match app.active_tab {
        app::ActiveTab::Triggers => app.selected_trigger(snapshot).map(|t| {
            crate::app::DetailView::Trigger {
                trigger_id: t.trigger_id.clone(),
                scroll: 0,
                focus: Default::default(),
                movements_selected: 0,
                agents_selected: 0,
            }
        }),
        app::ActiveTab::Orchestrator => app.selected_movement(snapshot).map(|m| {
            crate::app::DetailView::Movement {
                movement_id: m.id.clone(),
                scroll: 0,
                focus: Default::default(),
                tasks_selected: 0,
            }
        }),
        app::ActiveTab::Tasks => app.selected_task(snapshot).map(|t| {
            crate::app::DetailView::Task {
                task_id: t.id.clone(),
                scroll: 0,
            }
        }),
        app::ActiveTab::Deliverables => app.selected_deliverable(snapshot).map(|m| {
            crate::app::DetailView::Deliverable {
                movement_id: m.id.clone(),
                scroll: 0,
            }
        }),
        app::ActiveTab::Agents => app.agents_state.selected().map(|idx| {
            crate::app::DetailView::Agent {
                agent_index: idx,
                scroll: 0,
                artifact_cache: Box::new(None),
            }
        }),
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
