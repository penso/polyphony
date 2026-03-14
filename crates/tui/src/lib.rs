use std::{
    collections::VecDeque,
    io::{IsTerminal as _, Read as _, Write as _},
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use {
    chrono::{DateTime, Utc},
    crossterm::{
        event::{self, Event, KeyCode},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    polyphony_core::{EventScope, RunningRow, RuntimeEvent, RuntimeSnapshot, VisibleIssueRow},
    polyphony_orchestrator::RuntimeCommand,
    ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
        style::{Color, Modifier, Style},
        symbols,
        text::{Line, Span},
        widgets::{
            Block, BorderType, Cell, Clear, Gauge, HighlightSpacing, LineGauge, Paragraph, Row,
            Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline, Table, TableState, Tabs,
        },
    },
    thiserror::Error,
    tokio::sync::{mpsc, watch},
};

#[cfg(unix)]
use std::os::fd::AsRawFd as _;

const HISTORY_LEN: usize = 48;
const LOG_BUFFER_CAPACITY: usize = 2_000;

#[derive(Clone, Copy)]
struct ShortcutHint {
    keys: &'static str,
    label: &'static str,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Overview,
    Activity,
    Logs,
    Agents,
}

impl ActiveTab {
    const ALL: [Self; 4] = [Self::Overview, Self::Activity, Self::Logs, Self::Agents];

    const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Activity => "Activity",
            Self::Logs => "Logs",
            Self::Agents => "Agents",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Activity => 1,
            Self::Logs => 2,
            Self::Agents => 3,
        }
    }

    fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Overview)
    }

    fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }

    fn previous(self) -> Self {
        Self::from_index((self.index() + Self::ALL.len() - 1) % Self::ALL.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventMarker {
    at: DateTime<Utc>,
    scope: EventScope,
    message: String,
}

impl From<&RuntimeEvent> for EventMarker {
    fn from(event: &RuntimeEvent) -> Self {
        Self {
            at: event.at,
            scope: event.scope,
            message: event.message.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct MetricHistory {
    visible: VecDeque<u64>,
    running: VecDeque<u64>,
    retrying: VecDeque<u64>,
    token_delta: VecDeque<u64>,
    event_burst: VecDeque<u64>,
}

impl MetricHistory {
    fn push(&mut self, snapshot: &RuntimeSnapshot, token_delta: u64, event_burst: u64) {
        push_history_point(&mut self.visible, snapshot.visible_issues.len() as u64);
        push_history_point(&mut self.running, snapshot.counts.running as u64);
        push_history_point(&mut self.retrying, snapshot.counts.retrying as u64);
        push_history_point(&mut self.token_delta, token_delta);
        push_history_point(&mut self.event_burst, event_burst);
    }
}

#[derive(Debug)]
struct AppState {
    theme: Theme,
    active_tab: ActiveTab,
    history: MetricHistory,
    running_state: TableState,
    visible_state: TableState,
    models_state: TableState,
    logs_scroll: usize,
    logs_max_scroll: usize,
    logs_follow_tail: bool,
    events_scroll: usize,
    last_total_tokens: u64,
    last_event_marker: Option<EventMarker>,
    frame_count: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            active_tab: ActiveTab::Overview,
            history: MetricHistory::default(),
            running_state: TableState::default(),
            visible_state: TableState::default(),
            models_state: TableState::default(),
            logs_scroll: 0,
            logs_max_scroll: 0,
            logs_follow_tail: true,
            events_scroll: 0,
            last_total_tokens: 0,
            last_event_marker: None,
            frame_count: 0,
        }
    }
}

impl AppState {
    fn with_theme(theme: Theme) -> Self {
        Self {
            theme,
            ..Self::default()
        }
    }

    fn on_snapshot(&mut self, snapshot: &RuntimeSnapshot) {
        let token_delta = snapshot
            .codex_totals
            .total_tokens
            .saturating_sub(self.last_total_tokens);
        self.last_total_tokens = snapshot.codex_totals.total_tokens;

        let event_burst = match self.last_event_marker.as_ref() {
            Some(previous) => snapshot
                .recent_events
                .iter()
                .take_while(|event| EventMarker::from(*event) != *previous)
                .count() as u64,
            None => 0,
        };
        self.last_event_marker = snapshot.recent_events.first().map(EventMarker::from);
        self.history.push(snapshot, token_delta, event_burst);

        sync_selection(&mut self.running_state, snapshot.running.len());
        sync_selection(&mut self.visible_state, snapshot.visible_issues.len());
        sync_selection(&mut self.models_state, snapshot.agent_catalogs.len());
    }

    fn handle_key(&mut self, key: KeyCode, snapshot: &RuntimeSnapshot) -> Option<RuntimeCommand> {
        match key {
            KeyCode::Char('q') => return Some(RuntimeCommand::Shutdown),
            KeyCode::Char('r') => return Some(RuntimeCommand::Refresh),
            KeyCode::Tab | KeyCode::Right => {
                self.active_tab = self.active_tab.next();
                return None;
            },
            KeyCode::BackTab | KeyCode::Left => {
                self.active_tab = self.active_tab.previous();
                return None;
            },
            KeyCode::Char('1') => {
                self.active_tab = ActiveTab::Overview;
                return None;
            },
            KeyCode::Char('2') => {
                self.active_tab = ActiveTab::Activity;
                return None;
            },
            KeyCode::Char('3') => {
                self.active_tab = ActiveTab::Logs;
                return None;
            },
            KeyCode::Char('4') => {
                self.active_tab = ActiveTab::Agents;
                return None;
            },
            KeyCode::Char('j') | KeyCode::Down => self.advance(snapshot, 1),
            KeyCode::Char('k') | KeyCode::Up => self.rewind(snapshot, 1),
            KeyCode::PageDown => self.advance(snapshot, 8),
            KeyCode::PageUp => self.rewind(snapshot, 8),
            KeyCode::Char('g') | KeyCode::Home => self.jump_to_top(),
            KeyCode::Char('G') | KeyCode::End => self.jump_to_bottom(),
            _ => {},
        }
        None
    }

    fn advance(&mut self, snapshot: &RuntimeSnapshot, amount: usize) {
        match self.active_tab {
            ActiveTab::Overview => {
                if snapshot.running.is_empty() {
                    move_selection(
                        &mut self.visible_state,
                        snapshot.visible_issues.len(),
                        amount,
                    );
                } else {
                    move_selection(&mut self.running_state, snapshot.running.len(), amount);
                }
            },
            ActiveTab::Activity => {
                self.events_scroll = self.events_scroll.saturating_add(amount);
            },
            ActiveTab::Logs => {
                if self.logs_follow_tail {
                    self.logs_scroll = self.logs_max_scroll;
                } else {
                    self.logs_scroll = self.logs_scroll.saturating_add(amount);
                    if self.logs_scroll >= self.logs_max_scroll {
                        self.logs_scroll = self.logs_max_scroll;
                        self.logs_follow_tail = true;
                    }
                }
            },
            ActiveTab::Agents => {
                move_selection(
                    &mut self.models_state,
                    snapshot.agent_catalogs.len(),
                    amount,
                );
            },
        }
    }

    fn rewind(&mut self, snapshot: &RuntimeSnapshot, amount: usize) {
        match self.active_tab {
            ActiveTab::Overview => {
                if snapshot.running.is_empty() {
                    move_selection_back(
                        &mut self.visible_state,
                        snapshot.visible_issues.len(),
                        amount,
                    );
                } else {
                    move_selection_back(&mut self.running_state, snapshot.running.len(), amount);
                }
            },
            ActiveTab::Activity => {
                self.events_scroll = self.events_scroll.saturating_sub(amount);
            },
            ActiveTab::Logs => {
                let current = if self.logs_follow_tail {
                    self.logs_max_scroll
                } else {
                    self.logs_scroll
                };
                self.logs_follow_tail = false;
                self.logs_scroll = current.saturating_sub(amount);
            },
            ActiveTab::Agents => {
                move_selection_back(
                    &mut self.models_state,
                    snapshot.agent_catalogs.len(),
                    amount,
                );
            },
        }
    }

    fn jump_to_top(&mut self) {
        match self.active_tab {
            ActiveTab::Activity => self.events_scroll = 0,
            ActiveTab::Logs => {
                self.logs_follow_tail = false;
                self.logs_scroll = 0;
            },
            _ => {},
        }
    }

    fn jump_to_bottom(&mut self) {
        match self.active_tab {
            ActiveTab::Activity => self.events_scroll = usize::MAX,
            ActiveTab::Logs => {
                self.logs_follow_tail = true;
                self.logs_scroll = self.logs_max_scroll;
            },
            _ => {},
        }
    }

    fn sync_logs(&mut self, line_count: usize, area_height: u16) {
        self.logs_max_scroll = max_scroll(line_count, area_height);
        if self.logs_follow_tail {
            self.logs_scroll = self.logs_max_scroll;
        } else {
            self.logs_scroll = self.logs_scroll.min(self.logs_max_scroll);
        }
    }

    fn selected_running<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a RunningRow> {
        self.running_state
            .selected()
            .and_then(|index| snapshot.running.get(index))
    }

    fn selected_visible<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a VisibleIssueRow> {
        self.visible_state
            .selected()
            .and_then(|index| snapshot.visible_issues.get(index))
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

pub async fn run(
    mut snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    log_buffer: LogBuffer,
) -> Result<(), Error> {
    // Detect terminal colors before raw mode to avoid leftover OSC responses
    // being misinterpreted as key events (e.g. spurious Tab → Activity tab).
    let theme = detect_terminal_theme().unwrap_or_else(default_theme);
    enable_raw_mode()?;
    drain_pending_input();
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = AppState::with_theme(theme);
    let mut snapshot = snapshot_rx.borrow().clone();
    app.on_snapshot(&snapshot);

    let result = loop {
        terminal.draw(|frame| draw(frame, &snapshot, &log_buffer, &mut app))?;

        let mut key_handled = false;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            if let Some(command) = app.handle_key(key.code, &snapshot) {
                let shutdown = matches!(command, RuntimeCommand::Shutdown);
                let _ = command_tx.send(command);
                if shutdown {
                    break Ok(());
                }
            }
            key_handled = true;
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

fn draw_workflow_bootstrap(
    frame: &mut ratatui::Frame<'_>,
    _workflow_path: &Path,
    state: &BootstrapState,
    theme: Theme,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background)),
        frame.area(),
    );

    let shell = centered_rect(frame.area(), 62, 11);
    let shadow = Rect::new(
        shell.x.saturating_add(1),
        shell.y.saturating_add(1),
        shell.width.saturating_sub(1),
        shell.height.saturating_sub(1),
    );
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.panel)),
        shadow,
    );
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
            .border_style(Style::default().fg(theme.highlight))
            .style(Style::default().bg(theme.panel_alt)),
        shell,
    );

    let inner = shell.inner(Margin {
        vertical: 1,
        horizontal: 3,
    });

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),   // top spacer
            Constraint::Length(2),  // body text
            Constraint::Fill(1),   // bottom spacer
            Constraint::Length(1),  // buttons (pinned to bottom)
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
            .fg(theme.background)
            .bg(theme.muted)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let create_style = if state.choice == BootstrapChoice::Create {
        Style::default()
            .fg(theme.background)
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

const BRAILLE_SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn spinner_char(frame: u64) -> char {
    BRAILLE_SPINNER[(frame / 4) as usize % BRAILLE_SPINNER.len()]
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &RuntimeSnapshot,
    log_buffer: &LogBuffer,
    app: &mut AppState,
) {
    app.frame_count = app.frame_count.wrapping_add(1);
    let theme = app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background)),
        frame.area(),
    );

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, areas[0], snapshot, app);
    draw_summary_strip(frame, areas[1], snapshot, app);
    match app.active_tab {
        ActiveTab::Overview => draw_overview_tab(frame, areas[2], snapshot, app),
        ActiveTab::Activity => draw_activity_tab(frame, areas[2], snapshot, app),
        ActiveTab::Logs => draw_logs_tab(frame, areas[2], log_buffer, app),
        ActiveTab::Agents => draw_agents_tab(frame, areas[2], snapshot, app),
    }
    draw_footer(frame, areas[3], snapshot, app);
}

fn draw_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let tabs = Tabs::new(
        ActiveTab::ALL
            .into_iter()
            .map(|tab| Line::from(Span::styled(tab.title(), Style::default().fg(theme.muted))))
            .collect::<Vec<_>>(),
    )
    .select(app.active_tab.index())
    .divider(Span::raw("  "))
    .highlight_style(
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )
    .block(panel_block("Polyphony", theme));
    frame.render_widget(tabs, sections[0]);

    let summary = vec![
        Line::from(vec![
            Span::styled("visible ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.visible_issues.len().to_string(),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("running ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.counts.running.to_string(),
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("retry ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.counts.retrying.to_string(),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("updated ", Style::default().fg(theme.muted)),
            Span::styled(
                snapshot.generated_at.format("%H:%M:%S").to_string(),
                Style::default().fg(theme.foreground),
            ),
        ]),
    ];
    let live_title = if snapshot.loading.any_active() {
        format!("{} syncing", spinner_char(app.frame_count))
    } else {
        "Live".into()
    };
    frame.render_widget(
        Paragraph::new(summary).block(panel_block(&live_title, theme)),
        sections[1],
    );
}

fn draw_summary_strip(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Length(4)])
        .split(area);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(rows[0]);
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(rows[1]);

    draw_metric_card(
        frame,
        top[0],
        "Visible Issues",
        snapshot.visible_issues.len().to_string(),
        "tracker backlog",
        &app.history.visible,
        theme.info,
        theme,
    );
    draw_metric_card(
        frame,
        top[1],
        "Running",
        snapshot.counts.running.to_string(),
        "active workers",
        &app.history.running,
        theme.success,
        theme,
    );
    draw_metric_card(
        frame,
        top[2],
        "Retry Queue",
        snapshot.counts.retrying.to_string(),
        "pending retries",
        &app.history.retrying,
        theme.warning,
        theme,
    );
    draw_metric_card(
        frame,
        top[3],
        "Total Tokens",
        human_count(snapshot.codex_totals.total_tokens),
        "lifetime usage",
        &app.history.token_delta,
        theme.highlight,
        theme,
    );

    draw_cadence_card(
        frame,
        bottom[0],
        "Tracker Poll",
        snapshot.generated_at,
        snapshot.cadence.last_tracker_poll_at,
        snapshot.cadence.tracker_poll_interval_ms,
        theme.info,
        theme,
        snapshot.loading.fetching_issues,
        app.frame_count,
    );
    draw_cadence_card(
        frame,
        bottom[1],
        "Budget Refresh",
        snapshot.generated_at,
        snapshot.cadence.last_budget_poll_at,
        snapshot.cadence.budget_poll_interval_ms,
        theme.success,
        theme,
        snapshot.loading.fetching_budgets,
        app.frame_count,
    );
    draw_cadence_card(
        frame,
        bottom[2],
        "Model Scan",
        snapshot.generated_at,
        snapshot.cadence.last_model_discovery_at,
        snapshot.cadence.model_discovery_interval_ms,
        theme.highlight,
        theme,
        snapshot.loading.fetching_models,
        app.frame_count,
    );
    draw_metric_card(
        frame,
        bottom[3],
        "Event Burst",
        app.history
            .event_burst
            .back()
            .copied()
            .unwrap_or_default()
            .to_string(),
        "new events",
        &app.history.event_burst,
        theme.danger,
        theme,
    );
}

fn draw_overview_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let columns = if area.width >= 120 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area)
    };

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(8)])
        .split(columns[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(8)])
        .split(columns[1]);

    if snapshot.running.is_empty() {
        draw_visible_issues_table(frame, left[0], snapshot, app);
    } else {
        draw_running_table(frame, left[0], snapshot, app);
    }
    draw_retry_panel(frame, left[1], snapshot, theme);
    draw_issue_detail(frame, right[0], snapshot, app);
    draw_budget_throttle_panel(frame, right[1], snapshot, theme);
}

fn draw_activity_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let columns = if area.width >= 120 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area)
    };
    draw_events_panel(frame, columns[0], snapshot, app);

    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(8)])
        .split(columns[1]);
    draw_network_panel(frame, side[0], snapshot, theme);
    draw_budget_throttle_panel(frame, side[1], snapshot, theme);
}

fn draw_logs_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    log_buffer: &LogBuffer,
    app: &mut AppState,
) {
    let theme = app.theme;
    let captured_lines = log_buffer.all_lines();
    let line_count = captured_lines.len();
    app.sync_logs(line_count, area.height);
    let mut lines = captured_lines
        .into_iter()
        .map(|line| Line::from(Span::raw(line)))
        .collect::<Vec<_>>();
    let subtitle = if line_count == 0 {
        lines = vec![
            Line::from(Span::styled(
                "No tracing lines captured yet.",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "If this stays empty, raise RUST_LOG (for example RUST_LOG=info).",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "Use PgUp/PgDn or g/G when the log view fills up.",
                Style::default().fg(theme.muted),
            )),
        ];
        "0 of 0".to_string()
    } else {
        log_indicator(
            line_count,
            app.logs_scroll,
            area.height,
            app.logs_follow_tail,
        )
    };
    draw_scrollable_lines(
        frame,
        area,
        &format!("Logs  {line_count} lines"),
        &lines,
        &mut app.logs_scroll,
        &subtitle,
        theme,
    );
}

fn draw_agents_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let columns = if area.width >= 120 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area)
    };
    draw_agent_catalogs(frame, columns[0], snapshot, app);

    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(8)])
        .split(columns[1]);
    draw_agent_detail(frame, side[0], snapshot, app);
    draw_budget_gauges(frame, side[1], snapshot, theme);
}

fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    let status = loading_status_line(snapshot, app.frame_count, theme);
    let status_width = status
        .spans
        .iter()
        .map(|s| s.content.len())
        .sum::<usize>() as u16
        + 4; // padding
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(20),
            Constraint::Length(status_width.max(12)),
        ])
        .split(area);
    let footer = Paragraph::new(shortcut_line(
        app.active_tab,
        sections[0].width.saturating_sub(2) as usize,
        theme,
    ))
    .block(panel_block("Controls", theme));
    frame.render_widget(footer, sections[0]);
    let status_widget =
        Paragraph::new(status).block(panel_block("Status", theme));
    frame.render_widget(status_widget, sections[1]);
}

fn loading_status_line(
    snapshot: &RuntimeSnapshot,
    frame_count: u64,
    theme: Theme,
) -> Line<'static> {
    let loading = &snapshot.loading;
    if !loading.any_active() && !snapshot.from_cache {
        return Line::from(Span::styled("ready", Style::default().fg(theme.success)));
    }
    let spinner = spinner_char(frame_count).to_string();
    let mut spans = vec![Span::styled(
        spinner,
        Style::default()
            .fg(theme.highlight)
            .add_modifier(Modifier::BOLD),
    )];
    let mut parts: Vec<&str> = Vec::new();
    if loading.fetching_issues {
        parts.push("issues");
    }
    if loading.fetching_budgets {
        parts.push("budgets");
    }
    if loading.fetching_models {
        parts.push("models");
    }
    if loading.reconciling {
        parts.push("reconciling");
    }
    if snapshot.from_cache {
        parts.push("cached");
    }
    if parts.is_empty() {
        spans.push(Span::styled(" syncing", Style::default().fg(theme.muted)));
    } else {
        spans.push(Span::styled(
            format!(" {}", parts.join(", ")),
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn shortcut_line(active_tab: ActiveTab, width: usize, theme: Theme) -> Line<'static> {
    let shortcuts = shortcuts_for(active_tab, width);
    let mut spans = Vec::with_capacity(shortcuts.len().saturating_mul(3));
    for (index, shortcut) in shortcuts.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(theme.border)));
        }
        spans.push(Span::styled(
            shortcut.keys,
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", shortcut.label),
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn shortcuts_for(active_tab: ActiveTab, width: usize) -> Vec<ShortcutHint> {
    let tab_hint = ShortcutHint {
        keys: "1-4/TAB",
        label: "switch tabs",
    };
    let refresh_hint = ShortcutHint {
        keys: "r",
        label: "refresh",
    };
    let quit_hint = ShortcutHint {
        keys: "q",
        label: "quit",
    };

    if width < 44 {
        return vec![tab_hint, quit_hint];
    }

    let mut shortcuts = vec![tab_hint];
    match active_tab {
        ActiveTab::Overview => {
            shortcuts.push(ShortcutHint {
                keys: "j/k",
                label: "select issue",
            });
        },
        ActiveTab::Activity => {
            shortcuts.push(ShortcutHint {
                keys: "j/k",
                label: "scroll events",
            });
            if width >= 76 {
                shortcuts.push(ShortcutHint {
                    keys: "PgUp/PgDn",
                    label: "jump faster",
                });
            }
            if width >= 96 {
                shortcuts.push(ShortcutHint {
                    keys: "g/G",
                    label: "top/bottom",
                });
            }
        },
        ActiveTab::Logs => {
            shortcuts.push(ShortcutHint {
                keys: "j/k",
                label: "tail or scroll",
            });
            if width >= 76 {
                shortcuts.push(ShortcutHint {
                    keys: "PgUp/PgDn",
                    label: "page logs",
                });
            }
            if width >= 96 {
                shortcuts.push(ShortcutHint {
                    keys: "g/G",
                    label: "top/bottom",
                });
            }
        },
        ActiveTab::Agents => {
            shortcuts.push(ShortcutHint {
                keys: "j/k",
                label: "select agent",
            });
        },
    }

    if width >= 58 {
        shortcuts.push(refresh_hint);
    }
    shortcuts.push(quit_hint);
    shortcuts
}

fn draw_metric_card(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    value: String,
    subtitle: &str,
    history: &VecDeque<u64>,
    accent: Color,
    theme: Theme,
) {
    frame.render_widget(card_block(title, accent, theme), area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            value,
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            subtitle,
            Style::default().fg(theme.muted),
        ))),
        rows[1],
    );
    let points = history.iter().copied().collect::<Vec<_>>();
    frame.render_widget(
        Sparkline::default()
            .data(&points)
            .style(Style::default().fg(accent))
            .bar_set(symbols::bar::NINE_LEVELS),
        rows[2],
    );
}

fn draw_cadence_card(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    now: DateTime<Utc>,
    last_at: Option<DateTime<Utc>>,
    interval_ms: u64,
    accent: Color,
    theme: Theme,
    is_loading: bool,
    frame_count: u64,
) {
    let (ratio, label) = cadence_progress(now, last_at, interval_ms);
    let display_title = if is_loading {
        format!("{} {}", spinner_char(frame_count), title)
    } else {
        title.to_string()
    };
    frame.render_widget(
        LineGauge::default()
            .block(card_block(&display_title, accent, theme))
            .filled_style(Style::default().fg(accent))
            .unfilled_style(Style::default().fg(theme.border))
            .line_set(symbols::line::THICK)
            .label(label)
            .ratio(ratio),
        area,
    );
}

fn draw_running_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let rows = snapshot.running.iter().map(|running| {
        let progress = format!("{}/{}", running.turn_count, running.max_turns);
        Row::new([
            Cell::from(running.issue_identifier.clone()),
            Cell::from(running.state.clone()),
            Cell::from(running.agent_name.clone()),
            Cell::from(progress),
            Cell::from(human_count(running.tokens.total_tokens)),
            Cell::from(
                running
                    .last_event
                    .clone()
                    .unwrap_or_else(|| "waiting".into()),
            ),
        ])
    });
    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Min(12),
    ])
    .header(
        Row::new(["Issue", "State", "Agent", "Turns", "Tokens", "Last Event"]).style(
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(selected_row_style(theme))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always)
    .block(panel_block("Running", theme).title_bottom(panel_footer(
        &selection_indicator(app.running_state.selected(), snapshot.running.len()),
        theme,
    )));
    frame.render_stateful_widget(table, area, &mut app.running_state);
    draw_table_scrollbar(
        frame,
        area,
        snapshot.running.len(),
        app.running_state.selected(),
        theme,
    );
}

fn draw_visible_issues_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let rows = snapshot.visible_issues.iter().map(|issue| {
        Row::new([
            Cell::from(issue.issue_identifier.clone()),
            Cell::from(issue.title.clone()),
            Cell::from(issue.state.clone()),
            Cell::from(issue.labels.join(", ")),
        ])
    });
    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Min(18),
        Constraint::Length(14),
        Constraint::Length(18),
    ])
    .header(
        Row::new(["Issue", "Title", "State", "Labels"]).style(
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(selected_row_style(theme))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
    let issues_title = if snapshot.from_cache {
        "Visible Issues (cached)"
    } else {
        "Visible Issues"
    };
    let table = table.block(
        panel_block(issues_title, theme).title_bottom(panel_footer(
            &selection_indicator(app.visible_state.selected(), snapshot.visible_issues.len()),
            theme,
        )),
    );
    frame.render_stateful_widget(table, area, &mut app.visible_state);
    draw_table_scrollbar(
        frame,
        area,
        snapshot.visible_issues.len(),
        app.visible_state.selected(),
        theme,
    );
}

fn draw_retry_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    theme: Theme,
) {
    if snapshot.retrying.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No queued retries.",
                Style::default().fg(theme.muted),
            )))
            .block(panel_block("Retry Queue", theme)),
            area,
        );
        return;
    }

    let rows = snapshot.retrying.iter().map(|retry| {
        Row::new([
            Cell::from(retry.issue_identifier.clone()),
            Cell::from(retry.attempt.to_string()),
            Cell::from(relative_due(retry.due_at)),
            Cell::from(retry.error.clone().unwrap_or_default()),
        ])
    });
    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Min(12),
    ])
    .header(
        Row::new(["Issue", "Attempt", "Due", "Error"]).style(
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(panel_block("Retry Queue", theme));
    frame.render_widget(table, area);
}

fn draw_issue_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    frame.render_widget(panel_block("Inspector", theme), area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    if let Some(running) = app.selected_running(snapshot) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(3),
            ])
            .split(inner);
        let metadata = vec![
            Line::from(vec![
                Span::styled(
                    running.issue_identifier.clone(),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(running.state.clone(), Style::default().fg(theme.info)),
            ]),
            Line::from(format!(
                "agent={}  model={}  attempt={}",
                running.agent_name,
                running.model.clone().unwrap_or_else(|| "auto".into()),
                running
                    .attempt
                    .map(|attempt| attempt.to_string())
                    .unwrap_or_else(|| "1".into())
            )),
            Line::from(format!(
                "workspace={}",
                truncate_middle(&running.workspace_path.display().to_string(), 64)
            )),
        ];
        frame.render_widget(Paragraph::new(metadata), chunks[0]);

        let ratio = if running.max_turns == 0 {
            0.0
        } else {
            (running.turn_count as f64 / running.max_turns as f64).clamp(0.0, 1.0)
        };
        frame.render_widget(
            Gauge::default()
                .block(card_block("Turn Progress", theme.highlight, theme))
                .gauge_style(Style::default().fg(theme.highlight).bg(theme.panel_alt))
                .label(format!(
                    "{}/{} turns",
                    running.turn_count, running.max_turns
                ))
                .use_unicode(true)
                .ratio(ratio),
            chunks[1],
        );

        let detail = vec![
            Line::from(format!(
                "last_event={}  at={}",
                running
                    .last_event
                    .clone()
                    .unwrap_or_else(|| "waiting".into()),
                running
                    .last_event_at
                    .map(|at| at.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "-".into())
            )),
            Line::from(format!(
                "session={}  thread={}",
                running
                    .session_id
                    .as_deref()
                    .map(|value| truncate_middle(value, 18))
                    .unwrap_or_else(|| "-".into()),
                running
                    .thread_id
                    .as_deref()
                    .map(|value| truncate_middle(value, 18))
                    .unwrap_or_else(|| "-".into())
            )),
            Line::from(format!(
                "message={}",
                running
                    .last_message
                    .clone()
                    .unwrap_or_else(|| "no message".into())
            )),
        ];
        frame.render_widget(
            Paragraph::new(detail).wrap(ratatui::widgets::Wrap { trim: false }),
            chunks[2],
        );
        return;
    }

    if let Some(issue) = app.selected_visible(snapshot) {
        let coverage = if snapshot.visible_issues.is_empty() {
            0.0
        } else {
            ((snapshot.counts.running + snapshot.counts.retrying) as f64
                / snapshot.visible_issues.len() as f64)
                .clamp(0.0, 1.0)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Length(3),
                Constraint::Min(3),
            ])
            .split(inner);
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    issue.issue_identifier.clone(),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(issue.state.clone(), Style::default().fg(theme.info)),
            ]),
            Line::from(format!(
                "priority={}  labels={}",
                issue
                    .priority
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".into()),
                if issue.labels.is_empty() {
                    "-".into()
                } else {
                    issue.labels.join(", ")
                }
            )),
            Line::from(issue.title.clone()),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            chunks[0],
        );
        frame.render_widget(
            Gauge::default()
                .block(card_block("Dispatch Coverage", theme.info, theme))
                .gauge_style(Style::default().fg(theme.info).bg(theme.panel_alt))
                .label(format!(
                    "{} of {} active",
                    snapshot.counts.running + snapshot.counts.retrying,
                    snapshot.visible_issues.len()
                ))
                .use_unicode(true)
                .ratio(coverage),
            chunks[1],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Configure agents in ~/.config/polyphony/config.toml to turn visible issues into active runs.",
                Style::default().fg(theme.muted),
            )))
            .wrap(ratatui::widgets::Wrap { trim: false }),
            chunks[2],
        );
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Nothing selected yet.",
            Style::default().fg(theme.muted),
        ))),
        inner,
    );
}

fn draw_budget_throttle_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    theme: Theme,
) {
    let mut lines = snapshot
        .budgets
        .iter()
        .map(|budget| {
            let value = if let (Some(remaining), Some(total)) =
                (budget.credits_remaining, budget.credits_total)
            {
                format!("{remaining:.1}/{total:.1} credits")
            } else if let (Some(spent), Some(limit)) = (budget.spent_usd, budget.hard_limit_usd) {
                format!("${spent:.2}/${limit:.2}")
            } else if let Some(spent) = budget.spent_usd {
                format!("spent ${spent:.2}")
            } else {
                "budget n/a".into()
            };
            Line::from(vec![
                Span::styled(
                    budget.component.clone(),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(value, Style::default().fg(theme.muted)),
            ])
        })
        .collect::<Vec<_>>();

    lines.extend(snapshot.throttles.iter().map(|throttle| {
        Line::from(vec![
            Span::styled(
                "throttle",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "{} until {} ({})",
                    throttle.component,
                    throttle.until.format("%H:%M:%S"),
                    throttle.reason
                ),
                Style::default().fg(theme.muted),
            ),
        ])
    }));

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No budget probes or active throttles.",
            Style::default().fg(theme.muted),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Budgets & Throttles", theme))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

fn draw_events_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let lines = snapshot
        .recent_events
        .iter()
        .map(|event| {
            Line::from(vec![
                Span::styled(
                    event.at.format("%H:%M:%S").to_string(),
                    Style::default().fg(theme.muted),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("[{}]", event.scope),
                    Style::default().fg(scope_color(event.scope, theme)),
                ),
                Span::raw("  "),
                Span::styled(event.message.clone(), Style::default().fg(theme.foreground)),
            ])
        })
        .collect::<Vec<_>>();
    let footer = viewport_indicator(lines.len(), app.events_scroll, area.height);
    draw_scrollable_lines(
        frame,
        area,
        &format!("Recent Events  {} items", lines.len()),
        &lines,
        &mut app.events_scroll,
        &footer,
        theme,
    );
}

fn draw_network_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    theme: Theme,
) {
    frame.render_widget(panel_block("Network Cadence", theme), area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(inner);

    let gauges = [
        (
            "tracker",
            snapshot.cadence.last_tracker_poll_at,
            snapshot.cadence.tracker_poll_interval_ms,
            theme.info,
        ),
        (
            "budgets",
            snapshot.cadence.last_budget_poll_at,
            snapshot.cadence.budget_poll_interval_ms,
            theme.success,
        ),
        (
            "models",
            snapshot.cadence.last_model_discovery_at,
            snapshot.cadence.model_discovery_interval_ms,
            theme.highlight,
        ),
    ];

    for (index, (label, last_at, interval_ms, accent)) in gauges.into_iter().enumerate() {
        let (ratio, status) = cadence_progress(snapshot.generated_at, last_at, interval_ms);
        frame.render_widget(
            LineGauge::default()
                .ratio(ratio)
                .label(format!("{label}  {status}"))
                .filled_style(Style::default().fg(accent))
                .unfilled_style(Style::default().fg(theme.border))
                .line_set(symbols::line::THICK),
            chunks[index],
        );
    }
}

fn draw_agent_catalogs(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let theme = app.theme;
    let mut catalogs = snapshot.agent_catalogs.clone();
    catalogs.sort_by(|left, right| left.agent_name.cmp(&right.agent_name));

    let rows = catalogs.iter().map(|catalog| {
        Row::new([
            Cell::from(catalog.agent_name.clone()),
            Cell::from(catalog.provider_kind.clone()),
            Cell::from(
                catalog
                    .selected_model
                    .clone()
                    .unwrap_or_else(|| "auto".into()),
            ),
            Cell::from(catalog.models.len().to_string()),
        ])
    });
    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Min(18),
        Constraint::Length(10),
    ])
    .header(
        Row::new(["Agent", "Provider", "Selected", "Catalog"]).style(
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(selected_row_style(theme))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always)
    .block(
        panel_block("Agent Catalogs", theme).title_bottom(panel_footer(
            &selection_indicator(app.models_state.selected(), catalogs.len()),
            theme,
        )),
    );
    frame.render_stateful_widget(table, area, &mut app.models_state);
    draw_table_scrollbar(
        frame,
        area,
        catalogs.len(),
        app.models_state.selected(),
        theme,
    );
}

fn draw_agent_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let theme = app.theme;
    frame.render_widget(panel_block("Selected Agent", theme), area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let mut catalogs = snapshot.agent_catalogs.clone();
    catalogs.sort_by(|left, right| left.agent_name.cmp(&right.agent_name));

    let selected = app
        .models_state
        .selected()
        .and_then(|index| catalogs.get(index));
    let Some(catalog) = selected else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No discovered agent catalogs yet.",
                Style::default().fg(theme.muted),
            ))),
            inner,
        );
        return;
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                catalog.agent_name.clone(),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                catalog.provider_kind.clone(),
                Style::default().fg(theme.info),
            ),
        ]),
        Line::from(format!(
            "selected={}  fetched={}",
            catalog
                .selected_model
                .clone()
                .unwrap_or_else(|| "auto".into()),
            catalog.fetched_at.format("%H:%M:%S")
        )),
        Line::from(format!("models={}", catalog.models.len())),
        Line::from(format!(
            "catalog={}",
            if catalog.models.is_empty() {
                "no discovered models".into()
            } else {
                catalog
                    .models
                    .iter()
                    .take(4)
                    .map(|model| model.id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

fn draw_budget_gauges(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    theme: Theme,
) {
    frame.render_widget(panel_block("Budget Gauges", theme), area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    if snapshot.budgets.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No budget probes configured.",
                Style::default().fg(theme.muted),
            ))),
            inner,
        );
        return;
    }

    let mut budgets = snapshot.budgets.clone();
    budgets.sort_by(|left, right| left.component.cmp(&right.component));
    let visible = budgets.len().min((inner.height as usize / 2).max(1));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(2); visible])
        .split(inner);

    for (index, budget) in budgets.into_iter().take(visible).enumerate() {
        let (ratio, label, accent) = budget_ratio_label(&budget, theme);
        if let Some(ratio) = ratio {
            frame.render_widget(
                LineGauge::default()
                    .ratio(ratio)
                    .label(label)
                    .filled_style(Style::default().fg(accent))
                    .unfilled_style(Style::default().fg(theme.border))
                    .line_set(symbols::line::THICK),
                chunks[index],
            );
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    label,
                    Style::default().fg(theme.muted),
                ))),
                chunks[index],
            );
        }
    }
}

fn draw_scrollable_lines(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    lines: &[Line<'static>],
    scroll: &mut usize,
    subtitle: &str,
    theme: Theme,
) {
    let max_scroll = max_scroll(lines.len(), area.height);
    *scroll = (*scroll).min(max_scroll);
    let block = panel_block(title, theme).title_bottom(panel_footer(subtitle, theme));
    frame.render_widget(
        Paragraph::new(lines.to_vec())
            .block(block)
            .scroll((to_u16(*scroll), 0)),
        area,
    );
    let viewport = area.height.saturating_sub(2) as usize;
    if lines.len() <= viewport {
        return;
    }
    let mut state = ScrollbarState::default()
        .content_length(lines.len())
        .position(*scroll);
    frame.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .style(Style::default().fg(theme.border)),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

fn draw_table_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    rows_len: usize,
    selected: Option<usize>,
    theme: Theme,
) {
    if rows_len <= 1 {
        return;
    }
    let mut state = ScrollbarState::default()
        .content_length(rows_len)
        .position(selected.unwrap_or_default());
    frame.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .style(Style::default().fg(theme.border)),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

fn panel_block(title: &str, theme: Theme) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel))
}

fn card_block(title: &str, accent: Color, theme: Theme) -> Block<'static> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.panel_alt))
}

fn selected_row_style(theme: Theme) -> Style {
    Style::default()
        .bg(theme.selection)
        .fg(theme.foreground)
        .add_modifier(Modifier::BOLD)
}

fn cadence_progress(
    now: DateTime<Utc>,
    last_at: Option<DateTime<Utc>>,
    interval_ms: u64,
) -> (f64, String) {
    if interval_ms == 0 {
        return (1.0, "manual".into());
    }
    let Some(last_at) = last_at else {
        return (0.0, "pending".into());
    };
    let elapsed_ms = now.signed_duration_since(last_at).num_milliseconds().max(0) as u64;
    if elapsed_ms >= interval_ms {
        return (1.0, "due now".into());
    }
    let ratio = elapsed_ms as f64 / interval_ms as f64;
    (
        ratio,
        format!(
            "{}s / {}s",
            elapsed_ms / 1_000,
            interval_ms.saturating_div(1_000)
        ),
    )
}

fn budget_ratio_label(
    budget: &polyphony_core::BudgetSnapshot,
    theme: Theme,
) -> (Option<f64>, String, Color) {
    if let (Some(remaining), Some(total)) = (budget.credits_remaining, budget.credits_total)
        && total > 0.0
    {
        let ratio = (remaining / total).clamp(0.0, 1.0);
        let accent = if ratio > 0.5 {
            theme.success
        } else if ratio > 0.2 {
            theme.warning
        } else {
            theme.danger
        };
        let requests_suffix = budget
            .raw
            .as_ref()
            .and_then(|raw| raw.get("requests"))
            .and_then(|v| v.as_u64())
            .map(|n| format!("  ({n} reqs)"))
            .unwrap_or_default();
        return (
            Some(ratio),
            format!(
                "{}  {:.0}/{:.0}{requests_suffix}",
                budget.component, remaining, total
            ),
            accent,
        );
    }

    if let (Some(spent), Some(limit)) = (
        budget.spent_usd,
        budget.hard_limit_usd.or(budget.soft_limit_usd),
    ) && limit > 0.0
    {
        let ratio = (spent / limit).clamp(0.0, 1.0);
        let accent = if ratio < 0.6 {
            theme.success
        } else if ratio < 0.85 {
            theme.warning
        } else {
            theme.danger
        };
        return (
            Some(ratio),
            format!("{}  ${:.2}/${:.2}", budget.component, spent, limit),
            accent,
        );
    }

    (
        None,
        format!("{}  budget details unavailable", budget.component),
        theme.muted,
    )
}

fn scope_color(scope: EventScope, theme: Theme) -> Color {
    match scope {
        EventScope::Dispatch | EventScope::Workflow => theme.info,
        EventScope::Retry => theme.warning,
        EventScope::Handoff => theme.highlight,
        EventScope::Throttle => theme.danger,
        EventScope::Agent | EventScope::Worker | EventScope::Reconcile | EventScope::Tracker
        | EventScope::Startup | EventScope::Feedback => theme.muted,
    }
}

fn human_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn relative_due(due_at: DateTime<Utc>) -> String {
    let delta = due_at.signed_duration_since(Utc::now()).num_seconds();
    if delta <= 0 {
        "now".into()
    } else if delta < 60 {
        format!("{delta}s")
    } else {
        format!("{}m", delta / 60)
    }
}

fn panel_footer(label: &str, theme: Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("{label}─"),
        Style::default().fg(theme.muted),
    ))
    .right_aligned()
}

fn selection_indicator(selected: Option<usize>, total: usize) -> String {
    if total == 0 {
        return "0 of 0".into();
    }
    format!("{} of {}", selected.unwrap_or_default() + 1, total)
}

fn viewport_indicator(total: usize, scroll: usize, area_height: u16) -> String {
    if total == 0 {
        return "0 of 0".into();
    }
    let viewport = area_height.saturating_sub(2) as usize;
    if viewport == 0 {
        return format!("0 of {total}");
    }
    let start = scroll.min(total.saturating_sub(1)) + 1;
    let end = (scroll + viewport).min(total);
    if start == end {
        format!("{start} of {total}")
    } else {
        format!("{start}-{end} of {total}")
    }
}

fn log_indicator(total: usize, scroll: usize, area_height: u16, follow_tail: bool) -> String {
    let viewport = viewport_indicator(total, scroll, area_height);
    if total == 0 {
        viewport
    } else if follow_tail {
        format!("tail ↓ {viewport}")
    } else {
        format!("scroll ↑ {viewport}")
    }
}

fn truncate_middle(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len || max_len < 7 {
        return value.into();
    }
    let left = (max_len - 1) / 2;
    let right = max_len.saturating_sub(left + 1);
    let start = value.chars().take(left).collect::<String>();
    let end = value
        .chars()
        .rev()
        .take(right)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{start}…{end}")
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

fn max_scroll(lines: usize, area_height: u16) -> usize {
    let viewport = area_height.saturating_sub(2) as usize;
    lines.saturating_sub(viewport)
}

fn to_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

fn push_history_point(history: &mut VecDeque<u64>, value: u64) {
    history.push_back(value);
    while history.len() > HISTORY_LEN {
        history.pop_front();
    }
}

fn sync_selection(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    match state.selected() {
        Some(index) if index < len => {},
        Some(_) => state.select(Some(len - 1)),
        None => state.select(Some(0)),
    }
}

fn move_selection(state: &mut TableState, len: usize, amount: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or_default();
    let next = (current + amount) % len;
    state.select(Some(next));
}

fn move_selection_back(state: &mut TableState, len: usize, amount: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or_default();
    let previous = (current + len - (amount % len)) % len;
    state.select(Some(previous));
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Theme {
    background: Color,
    panel: Color,
    panel_alt: Color,
    selection: Color,
    border: Color,
    foreground: Color,
    muted: Color,
    highlight: Color,
    info: Color,
    success: Color,
    warning: Color,
    danger: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RgbColor {
    red: u8,
    green: u8,
    blue: u8,
}

impl RgbColor {
    const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    const fn to_color(self) -> Color {
        Color::Rgb(self.red, self.green, self.blue)
    }

    fn mix(self, other: Self, ratio: f32) -> Self {
        let ratio = ratio.clamp(0.0, 1.0);
        let inverse = 1.0 - ratio;
        Self {
            red: ((self.red as f32 * inverse) + (other.red as f32 * ratio)).round() as u8,
            green: ((self.green as f32 * inverse) + (other.green as f32 * ratio)).round() as u8,
            blue: ((self.blue as f32 * inverse) + (other.blue as f32 * ratio)).round() as u8,
        }
    }

    fn luminance(self) -> f32 {
        fn channel(value: u8) -> f32 {
            let value = value as f32 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }

        (0.2126 * channel(self.red))
            + (0.7152 * channel(self.green))
            + (0.0722 * channel(self.blue))
    }

    fn contrast_ratio(self, other: Self) -> f32 {
        let lighter = self.luminance().max(other.luminance());
        let darker = self.luminance().min(other.luminance());
        (lighter + 0.05) / (darker + 0.05)
    }
}

const fn default_theme() -> Theme {
    Theme {
        background: Color::Rgb(11, 15, 20),
        panel: Color::Rgb(18, 24, 31),
        panel_alt: Color::Rgb(23, 30, 38),
        selection: Color::Rgb(37, 49, 61),
        border: Color::Rgb(71, 85, 105),
        foreground: Color::Rgb(226, 232, 240),
        muted: Color::Rgb(148, 163, 184),
        highlight: Color::Rgb(245, 158, 11),
        info: Color::Rgb(56, 189, 248),
        success: Color::Rgb(52, 211, 153),
        warning: Color::Rgb(250, 204, 21),
        danger: Color::Rgb(248, 113, 113),
    }
}

const HIGHLIGHT_ACCENT: RgbColor = RgbColor::new(245, 158, 11);
const INFO_ACCENT: RgbColor = RgbColor::new(56, 189, 248);
const SUCCESS_ACCENT: RgbColor = RgbColor::new(52, 211, 153);
const WARNING_ACCENT: RgbColor = RgbColor::new(250, 204, 21);
const DANGER_ACCENT: RgbColor = RgbColor::new(248, 113, 113);

/// Drain any pending crossterm input events so that leftover bytes
/// (e.g. from OSC color query responses) are not misinterpreted as key presses.
fn drain_pending_input() {
    while event::poll(Duration::from_millis(10)).unwrap_or(false) {
        let _ = event::read();
    }
}

fn detect_terminal_theme() -> Option<Theme> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    let (foreground, background) = query_terminal_colors()?;
    derive_terminal_theme(foreground, background)
}

fn derive_terminal_theme(foreground: RgbColor, background: RgbColor) -> Option<Theme> {
    if foreground.contrast_ratio(background) < 2.5 {
        return None;
    }

    let dark_background = background.luminance() <= foreground.luminance();
    let panel = background.mix(
        foreground,
        if dark_background {
            0.06
        } else {
            0.03
        },
    );
    let panel_alt = background.mix(
        foreground,
        if dark_background {
            0.10
        } else {
            0.06
        },
    );
    let selection = background.mix(
        foreground,
        if dark_background {
            0.18
        } else {
            0.12
        },
    );
    let border = background.mix(
        foreground,
        if dark_background {
            0.30
        } else {
            0.22
        },
    );
    let muted = background.mix(
        foreground,
        if dark_background {
            0.60
        } else {
            0.45
        },
    );

    Some(Theme {
        background: background.to_color(),
        panel: panel.to_color(),
        panel_alt: panel_alt.to_color(),
        selection: selection.to_color(),
        border: border.to_color(),
        foreground: foreground.to_color(),
        muted: muted.to_color(),
        highlight: tune_accent(HIGHLIGHT_ACCENT, foreground, background, dark_background)
            .to_color(),
        info: tune_accent(INFO_ACCENT, foreground, background, dark_background).to_color(),
        success: tune_accent(SUCCESS_ACCENT, foreground, background, dark_background).to_color(),
        warning: tune_accent(WARNING_ACCENT, foreground, background, dark_background).to_color(),
        danger: tune_accent(DANGER_ACCENT, foreground, background, dark_background).to_color(),
    })
}

fn tune_accent(
    accent: RgbColor,
    foreground: RgbColor,
    background: RgbColor,
    dark_background: bool,
) -> RgbColor {
    let toned = if dark_background {
        accent.mix(background, 0.14).mix(foreground, 0.06)
    } else {
        accent.mix(background, 0.26).mix(foreground, 0.08)
    };
    ensure_contrast(
        toned,
        foreground,
        background,
        if dark_background {
            2.6
        } else {
            2.1
        },
    )
}

fn ensure_contrast(
    color: RgbColor,
    foreground: RgbColor,
    background: RgbColor,
    minimum: f32,
) -> RgbColor {
    let mut candidate = color;
    let mut ratio = 0.14;
    while candidate.contrast_ratio(background) < minimum && ratio <= 0.70 {
        candidate = candidate.mix(foreground, ratio);
        ratio += 0.08;
    }
    candidate
}

fn query_terminal_colors() -> Option<(RgbColor, RgbColor)> {
    #[cfg(unix)]
    {
        query_unix_terminal_colors()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
fn query_unix_terminal_colors() -> Option<(RgbColor, RgbColor)> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(b"\x1b]10;?\x07\x1b]11;?\x07").ok()?;
    tty.flush().ok()?;

    let deadline = Instant::now() + Duration::from_millis(150);
    let mut buffer = Vec::with_capacity(128);
    let mut scratch = [0u8; 256];
    while Instant::now() < deadline && buffer.len() < 1024 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !poll_tty_readable(tty.as_raw_fd(), remaining).ok()? {
            break;
        }

        let read = match tty.read(&mut scratch) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        buffer.extend_from_slice(&scratch[..read]);
        if let Some(colors) = parse_osc_color_responses(&buffer) {
            return Some(colors);
        }
    }

    parse_osc_color_responses(&buffer)
}

fn parse_osc_color_responses(buffer: &[u8]) -> Option<(RgbColor, RgbColor)> {
    let text = String::from_utf8_lossy(buffer);
    let mut foreground = None;
    let mut background = None;
    let mut offset = 0usize;
    while let Some(start) = text[offset..].find("\u{1b}]") {
        let sequence_start = offset + start + 2;
        let Some(separator) = text[sequence_start..].find(';') else {
            break;
        };
        let code_end = sequence_start + separator;
        let Some((payload, consumed)) = osc_payload(&text[code_end + 1..]) else {
            break;
        };
        match &text[sequence_start..code_end] {
            "10" => foreground = parse_terminal_color(payload).or(foreground),
            "11" => background = parse_terminal_color(payload).or(background),
            _ => {},
        }
        offset = code_end + 1 + consumed;
    }

    Some((foreground?, background?))
}

fn osc_payload(text: &str) -> Option<(&str, usize)> {
    let bel = text.find('\u{7}');
    let st = text.find("\u{1b}\\");
    let (end, terminator_len) = match (bel, st) {
        (Some(bel), Some(st)) if bel < st => (bel, 1),
        (_, Some(st)) => (st, 2),
        (Some(bel), None) => (bel, 1),
        (None, None) => return None,
    };
    Some((&text[..end], end + terminator_len))
}

fn parse_terminal_color(value: &str) -> Option<RgbColor> {
    let value = value.trim();
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let mut parts = rgb.split('/');
        return Some(RgbColor::new(
            parse_hex_component(parts.next()?)?,
            parse_hex_component(parts.next()?)?,
            parse_hex_component(parts.next()?)?,
        ));
    }

    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() == 6 {
        return Some(RgbColor::new(
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ));
    }

    None
}

fn parse_hex_component(component: &str) -> Option<u8> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(component, 16).ok()?;
    let max = (1u32 << (component.len() as u32 * 4)) - 1;
    Some(((value * 255) / max.max(1)) as u8)
}

#[cfg(unix)]
fn poll_tty_readable(fd: std::os::fd::RawFd, timeout: Duration) -> std::io::Result<bool> {
    use std::os::raw::{c_int, c_short};

    #[cfg(any(target_os = "linux", target_os = "android"))]
    type PollCount = std::os::raw::c_ulong;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    type PollCount = std::os::raw::c_uint;

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: c_short,
        revents: c_short,
    }

    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: PollCount, timeout: c_int) -> c_int;
    }

    const POLLIN: c_short = 0x0001;
    let timeout_ms = timeout.as_millis().min(c_int::MAX as u128) as c_int;
    let mut descriptor = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };

    loop {
        let result = unsafe { poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 {
            return Ok(result > 0 && descriptor.revents & POLLIN != 0);
        }

        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            ActiveTab, AppState, BootstrapChoice, BootstrapState, LogBuffer, RgbColor,
            default_theme, derive_terminal_theme, draw_footer, draw_workflow_bootstrap,
            parse_osc_color_responses, shortcut_line,
        },
        chrono::Utc,
        polyphony_core::{
            CodexTotals, EventScope, RuntimeCadence, RuntimeEvent, RuntimeSnapshot,
            SnapshotCounts, VisibleIssueRow,
        },
        ratatui::{Terminal, backend::TestBackend},
        std::path::Path,
    };

    fn snapshot_with(
        visible: usize,
        running: usize,
        retrying: usize,
        total_tokens: u64,
    ) -> RuntimeSnapshot {
        RuntimeSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts { running, retrying },
            cadence: RuntimeCadence::default(),
            visible_issues: (0..visible)
                .map(|index| VisibleIssueRow {
                    issue_id: index.to_string(),
                    issue_identifier: format!("ISSUE-{index}"),
                    title: format!("Issue {index}"),
                    state: "Todo".into(),
                    priority: None,
                    labels: Vec::new(),
                })
                .collect(),
            running: Vec::new(),
            retrying: Vec::new(),
            codex_totals: CodexTotals {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens,
                seconds_running: 0.0,
            },
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: Vec::new(),
            recent_events: vec![RuntimeEvent {
                at: Utc::now(),
                scope: EventScope::Workflow,
                message: "updated".into(),
            }],
            loading: polyphony_core::LoadingState::default(),
            from_cache: false,
        }
    }

    #[test]
    fn log_buffer_keeps_newest_entries_within_capacity() {
        let buffer = LogBuffer::with_capacity(2);

        buffer.push_line("one");
        buffer.push_line("two");
        buffer.push_line("three");

        assert_eq!(buffer.recent_lines(3), vec!["two", "three"]);
    }

    #[test]
    fn log_buffer_drains_in_oldest_first_order() {
        let buffer = LogBuffer::with_capacity(3);

        buffer.push_line("one");
        buffer.push_line("two");
        buffer.push_line("three");

        assert_eq!(buffer.drain_oldest_first(), vec!["one", "two", "three"]);
        assert!(buffer.recent_lines(1).is_empty());
    }

    #[test]
    fn app_history_tracks_snapshot_counts_and_token_deltas() {
        let mut app = AppState::default();
        let first = snapshot_with(3, 1, 0, 100);
        let second = snapshot_with(5, 2, 1, 160);

        app.on_snapshot(&first);
        app.on_snapshot(&second);

        assert_eq!(app.history.visible.back().copied(), Some(5));
        assert_eq!(app.history.running.back().copied(), Some(2));
        assert_eq!(app.history.retrying.back().copied(), Some(1));
        assert_eq!(app.history.token_delta.back().copied(), Some(60));
    }

    #[test]
    fn log_view_follows_tail_until_user_scrolls_away() {
        let mut app = AppState {
            active_tab: super::ActiveTab::Logs,
            ..AppState::default()
        };
        app.sync_logs(20, 10);

        assert!(app.logs_follow_tail);
        assert_eq!(app.logs_scroll, 12);

        app.rewind(&snapshot_with(0, 0, 0, 0), 4);
        assert!(!app.logs_follow_tail);
        assert_eq!(app.logs_scroll, 8);

        app.advance(&snapshot_with(0, 0, 0, 0), 4);
        assert!(app.logs_follow_tail);
        assert_eq!(app.logs_scroll, 12);
    }

    #[test]
    fn app_selection_clamps_when_visible_rows_disappear() {
        let mut app = AppState::default();
        let initial = snapshot_with(3, 0, 0, 0);
        app.on_snapshot(&initial);
        app.visible_state.select(Some(2));

        let reduced = snapshot_with(1, 0, 0, 0);
        app.on_snapshot(&reduced);

        assert_eq!(app.visible_state.selected(), Some(0));
    }

    #[test]
    fn bootstrap_state_toggles_and_confirms_choices() {
        let mut state = BootstrapState::default();

        assert_eq!(state.choice, BootstrapChoice::Create);
        assert_eq!(state.handle_key(crossterm::event::KeyCode::Right), None);
        assert_eq!(state.choice, BootstrapChoice::Cancel);
        assert_eq!(state.handle_key(crossterm::event::KeyCode::Left), None);
        assert_eq!(state.choice, BootstrapChoice::Create);
        assert_eq!(
            state.handle_key(crossterm::event::KeyCode::Enter),
            Some(true)
        );
        assert_eq!(
            state.handle_key(crossterm::event::KeyCode::Esc),
            Some(false)
        );
    }

    #[test]
    fn parses_osc_color_queries_with_mixed_terminators() {
        let response = b"\x1b]10;rgb:e5/e7/eb\x07\x1b]11;rgb:0b0f/1414/2020\x1b\\";

        let colors = parse_osc_color_responses(response);

        assert_eq!(
            colors,
            Some((RgbColor::new(229, 231, 235), RgbColor::new(11, 20, 32),))
        );
    }

    #[test]
    fn derived_theme_uses_terminal_background_and_foreground() {
        let foreground = RgbColor::new(229, 231, 235);
        let background = RgbColor::new(21, 24, 28);

        let theme = match derive_terminal_theme(foreground, background) {
            Some(theme) => theme,
            None => panic!("theme should derive"),
        };

        assert_eq!(theme.background, background.to_color());
        assert_eq!(theme.foreground, foreground.to_color());
        assert_ne!(theme.panel, default_theme().panel);
    }

    #[test]
    fn low_contrast_terminal_colors_fall_back_to_default_theme() {
        let foreground = RgbColor::new(120, 120, 120);
        let background = RgbColor::new(118, 118, 118);

        let theme = derive_terminal_theme(foreground, background);

        assert!(theme.is_none());
    }

    #[test]
    fn footer_shortcuts_show_quit_and_refresh_when_space_allows() {
        let footer = shortcut_line(ActiveTab::Overview, 84, default_theme()).to_string();

        assert!(footer.contains("1-4/TAB switch tabs"));
        assert!(footer.contains("j/k select issue"));
        assert!(footer.contains("r refresh"));
        assert!(footer.contains("q quit"));
    }

    #[test]
    fn footer_panel_renders_shortcuts_into_visible_content_row() {
        let backend = TestBackend::new(84, 3);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => panic!("test terminal should initialize: {error}"),
        };
        let app = AppState::default();
        let snapshot = snapshot_with(0, 0, 0, 0);

        if let Err(error) = terminal.draw(|frame| {
            draw_footer(frame, frame.area(), &snapshot, &app);
        }) {
            panic!("footer should render: {error}");
        }

        let row = (0..84)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 1))
                    .map(|cell| cell.symbol())
                    .unwrap_or(" ")
                    .to_string()
            })
            .collect::<String>();

        assert!(row.contains("switch tabs"));
        assert!(row.contains("refresh"));
        assert!(row.contains("quit"));
    }

    #[test]
    fn bootstrap_modal_uses_one_consistent_shell_width() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => panic!("test terminal should initialize: {error}"),
        };
        let state = BootstrapState::default();

        if let Err(error) = terminal.draw(|frame| {
            draw_workflow_bootstrap(frame, Path::new("WORKFLOW.md"), &state, default_theme());
        }) {
            panic!("bootstrap modal should render: {error}");
        }

        let top_row = (0..120)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 9))
                    .map(|cell| cell.symbol())
                    .unwrap_or(" ")
                    .to_string()
            })
            .collect::<String>();
        let bottom_row = (0..120)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 19))
                    .map(|cell| cell.symbol())
                    .unwrap_or(" ")
                    .to_string()
            })
            .collect::<String>();

        assert_eq!(top_row.chars().position(|ch| ch == '╭'), Some(29));
        assert_eq!(top_row.chars().position(|ch| ch == '╮'), Some(90));
        assert_eq!(bottom_row.chars().position(|ch| ch == '╰'), Some(29));
        assert_eq!(bottom_row.chars().position(|ch| ch == '╯'), Some(90));
    }
}
