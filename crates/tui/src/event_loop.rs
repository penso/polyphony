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
                                    }
                                },
                                MouseEventKind::ScrollDown => {
                                    let now = Instant::now();
                                    let skip = app.last_scroll_at.is_some_and(|prev| {
                                        now.duration_since(prev) < Duration::from_millis(50)
                                    });
                                    if !skip {
                                        app.last_scroll_at = Some(now);
                                        if app.active_tab == app::ActiveTab::Orchestrator
                                            && mouse_in_rect(
                                                mouse.column,
                                                mouse.row,
                                                app.movement_detail_area,
                                            )
                                        {
                                            app.movement_detail_scroll =
                                                app.movement_detail_scroll.saturating_add(1);
                                        } else if app.active_tab == app::ActiveTab::Orchestrator
                                            && mouse_in_rect(
                                                mouse.column,
                                                mouse.row,
                                                app.events_area,
                                            )
                                        {
                                            app.events_scroll = app.events_scroll.saturating_add(1);
                                        } else if app.active_tab == app::ActiveTab::Agents
                                            && mouse_in_rect(
                                                mouse.column,
                                                mouse.row,
                                                app.agents_detail_area,
                                            )
                                        {
                                            app.agents_detail_scroll =
                                                app.agents_detail_scroll.saturating_add(1);
                                        } else {
                                            let len = app.active_table_len(&snapshot);
                                            app.move_down(len, 1);
                                        }
                                    }
                                },
                                MouseEventKind::ScrollUp => {
                                    let now = Instant::now();
                                    let skip = app.last_scroll_at.is_some_and(|prev| {
                                        now.duration_since(prev) < Duration::from_millis(50)
                                    });
                                    if !skip {
                                        app.last_scroll_at = Some(now);
                                        if app.active_tab == app::ActiveTab::Orchestrator
                                            && mouse_in_rect(
                                                mouse.column,
                                                mouse.row,
                                                app.movement_detail_area,
                                            )
                                        {
                                            app.movement_detail_scroll =
                                                app.movement_detail_scroll.saturating_sub(1);
                                        } else if app.active_tab == app::ActiveTab::Orchestrator
                                            && mouse_in_rect(
                                                mouse.column,
                                                mouse.row,
                                                app.events_area,
                                            )
                                        {
                                            app.events_scroll = app.events_scroll.saturating_sub(1);
                                        } else if app.active_tab == app::ActiveTab::Agents
                                            && mouse_in_rect(
                                                mouse.column,
                                                mouse.row,
                                                app.agents_detail_area,
                                            )
                                        {
                                            app.agents_detail_scroll =
                                                app.agents_detail_scroll.saturating_sub(1);
                                        } else {
                                            let len = app.active_table_len(&snapshot);
                                            app.move_up(len, 1);
                                        }
                                    }
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
                                app.mode_modal_selected = (app.mode_modal_selected + 1) % 3;
                            },
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.mode_modal_selected = (app.mode_modal_selected + 2) % 3;
                            },
                            KeyCode::Enter => {
                                let modes = [
                                    DispatchMode::Manual,
                                    DispatchMode::Automatic,
                                    DispatchMode::Nightshift,
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

        // Always check for snapshot updates, whether or not a key was handled.
        // Use a short timeout so the draw loop stays responsive.
        tokio::select! {
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
                snapshot = snapshot_rx.borrow().clone();
                app.on_snapshot(&snapshot);
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
        KeyCode::Char('J') => {
            if app.active_tab == app::ActiveTab::Agents {
                app.agents_detail_scroll = app.agents_detail_scroll.saturating_add(1);
            } else if app.active_tab == app::ActiveTab::Orchestrator {
                app.movement_detail_scroll = app.movement_detail_scroll.saturating_add(1);
            }
        },
        KeyCode::Char('K') => {
            if app.active_tab == app::ActiveTab::Agents {
                app.agents_detail_scroll = app.agents_detail_scroll.saturating_sub(1);
            } else if app.active_tab == app::ActiveTab::Orchestrator {
                app.movement_detail_scroll = app.movement_detail_scroll.saturating_sub(1);
            }
        },

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

        // Trigger detail modal
        KeyCode::Enter => {
            if app.active_tab == app::ActiveTab::Triggers
                && app.selected_trigger(snapshot).is_some()
            {
                app.show_issue_detail = true;
            }
        },

        // Open trigger in browser
        KeyCode::Char('o') => {
            if app.active_tab == app::ActiveTab::Triggers
                && let Some(trigger) = app.selected_trigger(snapshot)
                && let Some(url) = &trigger.url
            {
                let _ = std::process::Command::new("open").arg(url).spawn();
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
            };
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
