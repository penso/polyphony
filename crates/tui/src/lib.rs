mod app;
mod render;
pub mod theme;

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use {
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseEventKind},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    polyphony_core::{DispatchMode, RuntimeSnapshot, VisibleTriggerKind},
    polyphony_orchestrator::RuntimeCommand,
    ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, BorderType, Clear, Paragraph},
    },
    thiserror::Error,
    tokio::sync::{mpsc, watch},
};

use {
    app::AppState,
    theme::{Theme, default_theme, detect_terminal_theme},
};

const LOG_BUFFER_CAPACITY: usize = 2_000;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// --- LogBuffer (public, used by CLI) ---

#[derive(Clone, Debug)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_capacity(LOG_BUFFER_CAPACITY)
    }
}

impl LogBuffer {
    pub fn with_capacity(max_lines: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(max_lines))),
            max_lines,
        }
    }

    pub fn push_line(&self, line: impl Into<String>) {
        let line = line.into();
        if line.trim().is_empty() {
            return;
        }
        let mut lines = lock_or_recover(&self.lines);
        lines.push_back(line);
        while lines.len() > self.max_lines {
            lines.pop_front();
        }
    }

    pub fn recent_lines(&self, limit: usize) -> Vec<String> {
        lock_or_recover(&self.lines)
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn all_lines(&self) -> Vec<String> {
        lock_or_recover(&self.lines).iter().cloned().collect()
    }

    pub fn drain_oldest_first(&self) -> Vec<String> {
        let mut lines = lock_or_recover(&self.lines);
        lines.drain(..).collect()
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

// --- Bootstrap (workflow initialization prompt) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootstrapChoice {
    Create,
    Cancel,
}

impl BootstrapChoice {
    fn toggle(&mut self) {
        *self = match self {
            Self::Create => Self::Cancel,
            Self::Cancel => Self::Create,
        };
    }
}

#[derive(Debug)]
struct BootstrapState {
    choice: BootstrapChoice,
}

impl Default for BootstrapState {
    fn default() -> Self {
        Self {
            choice: BootstrapChoice::Create,
        }
    }
}

impl BootstrapState {
    fn handle_key(&mut self, key: KeyCode) -> Option<bool> {
        match key {
            KeyCode::Enter => Some(self.choice == BootstrapChoice::Create),
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                Some(false)
            },
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                self.choice.toggle();
                None
            },
            _ => None,
        }
    }
}

pub fn prompt_workflow_initialization(workflow_path: &Path) -> Result<bool, Error> {
    let theme = detect_terminal_theme().unwrap_or_else(default_theme);
    enable_raw_mode()?;
    drain_pending_input();
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = BootstrapState::default();

    let result = loop {
        terminal.draw(|frame| draw_workflow_bootstrap(frame, workflow_path, &state, theme))?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && let Some(create) = state.handle_key(key.code)
        {
            break Ok(create);
        }
    };

    drain_pending_input();
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

// --- Main TUI loop ---

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

fn drain_pending_input() {
    while event::poll(Duration::from_millis(10)).unwrap_or(false) {
        let _ = event::read();
    }
}

fn mouse_in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

fn draw_workflow_bootstrap(
    frame: &mut ratatui::Frame<'_>,
    _workflow_path: &Path,
    state: &BootstrapState,
    theme: Theme,
) {
    let shell = centered_rect(frame.area(), 62, 11);
    frame.render_widget(Clear, shell);
    frame.render_widget(
        Block::default()
            .title(Line::from(Span::styled(
                " Initialize Polyphony ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.highlight)),
        shell,
    );

    let inner = shell.inner(Margin {
        vertical: 1,
        horizontal: 3,
    });

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Creates a WORKFLOW.md file for this repository.",
                Style::default().fg(theme.foreground),
            )),
            Line::from(Span::styled(
                "No existing files will be modified.",
                Style::default().fg(theme.foreground),
            )),
        ])
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let cancel_style = if state.choice == BootstrapChoice::Cancel {
        Style::default()
            .fg(Color::Black)
            .bg(theme.muted)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let create_style = if state.choice == BootstrapChoice::Create {
        Style::default()
            .fg(Color::Black)
            .bg(theme.highlight)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.foreground)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Cancel ", cancel_style),
            Span::raw("   "),
            Span::styled(" Initialize ", create_style),
        ]))
        .alignment(Alignment::Right),
        rows[3],
    );
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        chrono::Utc,
        polyphony_core::{
            CodexTotals, RuntimeCadence, RuntimeSnapshot, SnapshotCounts, TrackerConnectionStatus,
            VisibleIssueRow, VisibleTriggerKind, VisibleTriggerRow,
        },
        ratatui::{Terminal, backend::TestBackend, buffer::Buffer},
    };

    fn test_snapshot(visible: usize) -> RuntimeSnapshot {
        RuntimeSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts {
                running: 0,
                retrying: 0,
                ..Default::default()
            },
            cadence: RuntimeCadence::default(),
            visible_issues: (0..visible)
                .map(|i| VisibleIssueRow {
                    issue_id: format!("id-{i}"),
                    issue_identifier: format!("GH-{i}"),
                    title: format!("Test issue {i}"),
                    state: "open".into(),
                    priority: Some(2),
                    labels: vec![],
                    description: None,
                    url: None,
                    author: None,
                    parent_id: None,
                    updated_at: None,
                    created_at: None,
                    has_workspace: false,
                })
                .collect(),
            visible_triggers: (0..visible)
                .map(|i| VisibleTriggerRow {
                    trigger_id: format!("id-{i}"),
                    kind: VisibleTriggerKind::Issue,
                    source: "github".into(),
                    identifier: format!("GH-{i}"),
                    title: format!("Test issue {i}"),
                    status: "open".into(),
                    priority: Some(2),
                    labels: vec![],
                    description: None,
                    url: None,
                    author: None,
                    parent_id: None,
                    updated_at: None,
                    created_at: None,
                    has_workspace: false,
                })
                .collect(),
            running: vec![],
            retrying: vec![],
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: vec![],
            budgets: vec![],
            agent_catalogs: vec![],
            saved_contexts: vec![],
            recent_events: vec![],
            movements: vec![],
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

    fn buffer_text(buffer: &Buffer) -> String {
        let width = buffer.area.width as usize;
        buffer
            .content
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn app_state_selection_syncs() {
        let mut app = AppState::new(default_theme(), LogBuffer::default());
        let snapshot = test_snapshot(5);
        app.on_snapshot(&snapshot);
        assert_eq!(app.issues_state.selected(), Some(0));
    }

    #[test]
    fn app_state_empty_snapshot() {
        let mut app = AppState::new(default_theme(), LogBuffer::default());
        let snapshot = test_snapshot(0);
        app.on_snapshot(&snapshot);
        assert_eq!(app.issues_state.selected(), None);
    }

    #[test]
    fn render_does_not_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let snapshot = test_snapshot(3);
        let mut app = AppState::new(default_theme(), LogBuffer::default());
        app.on_snapshot(&snapshot);

        terminal
            .draw(|frame| {
                render::render(frame, &snapshot, &mut app);
            })
            .unwrap();
    }

    #[test]
    fn render_shows_connected_github_login_in_header() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut snapshot = test_snapshot(3);
        snapshot.tracker_connection = Some(TrackerConnectionStatus::connected("penso"));
        let mut app = AppState::new(default_theme(), LogBuffer::default());
        app.on_snapshot(&snapshot);

        terminal
            .draw(|frame| {
                render::render(frame, &snapshot, &mut app);
            })
            .unwrap();

        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("penso"), "{screen}");
    }

    #[test]
    fn tab_switching() {
        let mut app = AppState::new(default_theme(), LogBuffer::default());
        assert_eq!(app.active_tab, app::ActiveTab::Triggers);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Orchestrator);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Tasks);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Deliverables);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Agents);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Logs);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Triggers);
    }

    #[test]
    fn agent_detail_scroll_resets_when_agent_selection_changes() {
        let mut app = AppState::new(default_theme(), LogBuffer::default());
        let mut snapshot = test_snapshot(0);
        snapshot.running = vec![
            polyphony_core::RunningRow {
                issue_id: "issue-1".into(),
                issue_identifier: "GH-1".into(),
                agent_name: "opus".into(),
                model: Some("claude".into()),
                state: "running".into(),
                max_turns: 20,
                session_id: Some("opus-gh-1-0".into()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                turn_count: 1,
                last_event: Some("TurnStarted".into()),
                last_message: Some("hello".into()),
                started_at: Utc::now(),
                last_event_at: None,
                tokens: Default::default(),
                workspace_path: std::path::PathBuf::from("."),
                attempt: Some(0),
            },
            polyphony_core::RunningRow {
                issue_id: "issue-2".into(),
                issue_identifier: "GH-2".into(),
                agent_name: "codex".into(),
                model: Some("gpt-5".into()),
                state: "running".into(),
                max_turns: 20,
                session_id: Some("codex-gh-2-0".into()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                turn_count: 2,
                last_event: Some("TurnStarted".into()),
                last_message: Some("world".into()),
                started_at: Utc::now(),
                last_event_at: None,
                tokens: Default::default(),
                workspace_path: std::path::PathBuf::from("."),
                attempt: Some(0),
            },
        ];
        snapshot.counts.running = snapshot.running.len();

        app.on_snapshot(&snapshot);
        app.active_tab = app::ActiveTab::Agents;
        app.agents_detail_scroll = 5;

        app.move_down(snapshot.running.len(), 1);

        assert_eq!(app.agents_state.selected(), Some(1));
        assert_eq!(app.agents_detail_scroll, 0);
    }

    #[test]
    fn bootstrap_state_defaults_to_create() {
        let state = BootstrapState::default();
        assert_eq!(state.choice, BootstrapChoice::Create);
    }

    #[test]
    fn log_buffer_push_and_read() {
        let buf = LogBuffer::with_capacity(3);
        buf.push_line("one");
        buf.push_line("two");
        buf.push_line("three");
        buf.push_line("four");
        assert_eq!(buf.all_lines(), vec!["two", "three", "four"]);
    }
}
