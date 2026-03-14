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
        event::{self, Event, KeyCode},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    polyphony_core::RuntimeSnapshot,
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

use app::AppState;
use theme::{Theme, default_theme, detect_terminal_theme};

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
    _log_buffer: LogBuffer,
) -> Result<(), Error> {
    let theme = detect_terminal_theme().unwrap_or_else(default_theme);
    enable_raw_mode()?;
    drain_pending_input();
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = AppState::new(theme);
    let mut snapshot = snapshot_rx.borrow().clone();
    app.on_snapshot(&snapshot);

    let result = loop {
        terminal.draw(|frame| {
            render::render(frame, &snapshot, &mut app);
        })?;

        if let Some(since) = app.leaving_since {
            if since.elapsed() > Duration::from_secs(3) {
                break Ok(());
            }
        }

        let mut key_handled = false;
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if app.leaving {
                    // Ignore keys while leaving
                } else if app.show_issue_detail {
                    // Modal is open — handle modal keys
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                            app.show_issue_detail = false;
                        },
                        _ => {},
                    }
                } else if let Some(command) = handle_key(&mut app, key.code, &snapshot) {
                    let shutdown = matches!(command, RuntimeCommand::Shutdown);
                    let _ = command_tx.send(command);
                    if shutdown {
                        app.leaving = true;
                        app.leaving_since = Some(Instant::now());
                    }
                }
                key_handled = true;
            }
        }

        if !key_handled {
            tokio::select! {
                changed = snapshot_rx.changed() => {
                    if changed.is_err() {
                        break Ok(());
                    }
                    snapshot = snapshot_rx.borrow().clone();
                    app.on_snapshot(&snapshot);
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        } else if snapshot_rx.has_changed().unwrap_or(false) {
            snapshot = snapshot_rx.borrow().clone();
            app.on_snapshot(&snapshot);
        }
    };

    drain_pending_input();
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
        KeyCode::Char('1') => app.active_tab = app::ActiveTab::Issues,
        KeyCode::Char('2') => app.active_tab = app::ActiveTab::Orchestrator,
        KeyCode::Char('3') => app.active_tab = app::ActiveTab::Tasks,
        KeyCode::Char('4') => app.active_tab = app::ActiveTab::Deliverables,

        // Navigation (works on active tab's table)
        KeyCode::Char('j') | KeyCode::Down => {
            let len = app.active_table_len(snapshot);
            app.move_down(len, 1);
        },
        KeyCode::Char('k') | KeyCode::Up => {
            let len = app.active_table_len(snapshot);
            app.move_up(len, 1);
        },
        KeyCode::PageDown => {
            let len = app.active_table_len(snapshot);
            app.move_down(len, 8);
        },
        KeyCode::PageUp => {
            let len = app.active_table_len(snapshot);
            app.move_up(len, 8);
        },

        // Sort cycling (Issues tab)
        KeyCode::Char('s') => {
            if app.active_tab == app::ActiveTab::Issues {
                app.issue_sort = app.issue_sort.cycle();
                app.rebuild_sorted_indices(snapshot);
            }
        },

        // Issue detail modal
        KeyCode::Enter => {
            if app.active_tab == app::ActiveTab::Issues
                && app.selected_issue(snapshot).is_some()
            {
                app.show_issue_detail = true;
            }
        },

        _ => {},
    }
    None
}

// --- Helper functions ---

fn drain_pending_input() {
    while event::poll(Duration::from_millis(10)).unwrap_or(false) {
        let _ = event::read();
    }
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
            CodexTotals, RuntimeCadence, RuntimeSnapshot, SnapshotCounts, VisibleIssueRow,
        },
        ratatui::{Terminal, backend::TestBackend},
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
                    updated_at: None,
                    created_at: None,
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
            from_cache: false,
        }
    }

    #[test]
    fn app_state_selection_syncs() {
        let mut app = AppState::new(default_theme());
        let snapshot = test_snapshot(5);
        app.on_snapshot(&snapshot);
        assert_eq!(app.issues_state.selected(), Some(0));
    }

    #[test]
    fn app_state_empty_snapshot() {
        let mut app = AppState::new(default_theme());
        let snapshot = test_snapshot(0);
        app.on_snapshot(&snapshot);
        assert_eq!(app.issues_state.selected(), None);
    }

    #[test]
    fn render_does_not_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let snapshot = test_snapshot(3);
        let mut app = AppState::new(default_theme());
        app.on_snapshot(&snapshot);

        terminal
            .draw(|frame| {
                render::render(frame, &snapshot, &mut app);
            })
            .unwrap();
    }

    #[test]
    fn tab_switching() {
        let mut app = AppState::new(default_theme());
        assert_eq!(app.active_tab, app::ActiveTab::Issues);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Orchestrator);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Tasks);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Deliverables);

        app.active_tab = app.active_tab.next();
        assert_eq!(app.active_tab, app::ActiveTab::Issues);
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
