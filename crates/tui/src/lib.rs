use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use {
    chrono::{DateTime, Utc},
    crossterm::{
        event::{self, Event, KeyCode},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    polyphony_core::{RunningRow, RuntimeEvent, RuntimeSnapshot, VisibleIssueRow},
    polyphony_orchestrator::RuntimeCommand,
    ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Constraint, Direction, Layout, Margin, Rect},
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

const HISTORY_LEN: usize = 48;
const LOG_BUFFER_CAPACITY: usize = 2_000;
const BLOCK_SYMBOL: &str = " ";

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
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
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
        lines.push_front(line);
        while lines.len() > self.max_lines {
            lines.pop_back();
        }
    }

    pub fn recent_lines(&self, limit: usize) -> Vec<String> {
        lock_or_recover(&self.lines)
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn all_lines(&self) -> Vec<String> {
        lock_or_recover(&self.lines).iter().cloned().collect()
    }

    pub fn drain_oldest_first(&self) -> Vec<String> {
        let mut lines = lock_or_recover(&self.lines);
        let drained = lines.drain(..).collect::<Vec<_>>();
        drained.into_iter().rev().collect()
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
    scope: String,
    message: String,
}

impl From<&RuntimeEvent> for EventMarker {
    fn from(event: &RuntimeEvent) -> Self {
        Self {
            at: event.at,
            scope: event.scope.clone(),
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
    active_tab: ActiveTab,
    history: MetricHistory,
    running_state: TableState,
    visible_state: TableState,
    models_state: TableState,
    logs_scroll: usize,
    events_scroll: usize,
    last_total_tokens: u64,
    last_event_marker: Option<EventMarker>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_tab: ActiveTab::Overview,
            history: MetricHistory::default(),
            running_state: TableState::default(),
            visible_state: TableState::default(),
            models_state: TableState::default(),
            logs_scroll: 0,
            events_scroll: 0,
            last_total_tokens: 0,
            last_event_marker: None,
        }
    }
}

impl AppState {
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
                self.logs_scroll = self.logs_scroll.saturating_add(amount);
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
                self.logs_scroll = self.logs_scroll.saturating_sub(amount);
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
            ActiveTab::Logs => self.logs_scroll = 0,
            _ => {},
        }
    }

    fn jump_to_bottom(&mut self) {
        match self.active_tab {
            ActiveTab::Activity => self.events_scroll = usize::MAX,
            ActiveTab::Logs => self.logs_scroll = usize::MAX,
            _ => {},
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
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = BootstrapState::default();

    let result = loop {
        terminal.draw(|frame| draw_workflow_bootstrap(frame, workflow_path, &state))?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && let Some(create) = state.handle_key(key.code)
        {
            break Ok(create);
        }
    };

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
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = AppState::default();
    let mut snapshot = snapshot_rx.borrow().clone();
    app.on_snapshot(&snapshot);

    let result = loop {
        terminal.draw(|frame| draw(frame, &snapshot, &log_buffer, &mut app))?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && let Some(command) = app.handle_key(key.code, &snapshot)
        {
            let shutdown = matches!(command, RuntimeCommand::Shutdown);
            let _ = command_tx.send(command);
            if shutdown {
                break Ok(());
            }
        }

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
    };

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn draw_workflow_bootstrap(
    frame: &mut ratatui::Frame<'_>,
    workflow_path: &Path,
    state: &BootstrapState,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme().background)),
        frame.area(),
    );

    let outer = centered_rect(frame.area(), 94, 21);
    let top = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(outer);

    frame.render_widget(
        Tabs::new(vec![
            Line::from(Span::styled(
                "Initialize",
                Style::default()
                    .fg(theme().highlight)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled("Workflow", Style::default().fg(theme().muted))),
            Line::from(Span::styled(
                "Repo Local",
                Style::default().fg(theme().muted),
            )),
        ])
        .divider(Span::raw("  "))
        .select(0)
        .block(panel_block("Polyphony")),
        top[0],
    );

    let modal = centered_rect(top[1], 88, 18);
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme().panel_alt)),
        modal,
    );
    frame.render_widget(
        Block::default()
            .title(Line::from(Span::styled(
                " Initialize Polyphony ",
                Style::default()
                    .fg(theme().highlight)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme().highlight))
            .style(Style::default().bg(theme().panel_alt)),
        modal,
    );

    let inner = modal.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "This folder has not been initialized yet.",
                Style::default()
                    .fg(theme().foreground)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Create a repo-local WORKFLOW.md so Polyphony can start with sane defaults.",
                Style::default().fg(theme().muted),
            )),
        ]),
        rows[0],
    );

    let workflow_label = truncate_middle(
        &workflow_path.display().to_string(),
        rows[1].width.saturating_sub(6) as usize,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("path ", Style::default().fg(theme().muted)),
                Span::styled(
                    workflow_label,
                    Style::default().fg(theme().foreground),
                ),
            ]),
            Line::from(Span::styled(
                "Shared credentials and reusable agent profiles stay in ~/.config/polyphony/config.toml.",
                Style::default().fg(theme().muted),
            )),
        ])
        .block(card_block("Workflow File", theme().info)),
        rows[1],
    );

    let cards = if rows[2].width >= 84 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
            ])
            .split(rows[2])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
            ])
            .split(rows[2])
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Repo Local",
                Style::default()
                    .fg(theme().info)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("tracker kind and project or repo"),
            Line::from("workspace checkout and hooks"),
        ])
        .block(card_block("WORKFLOW.md", theme().info)),
        cards[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Shared",
                Style::default()
                    .fg(theme().success)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("API keys and provider auth"),
            Line::from("reusable agent profiles"),
        ])
        .block(card_block("User Config", theme().success)),
        cards[1],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Controls",
                Style::default()
                    .fg(theme().highlight)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("Enter or y to create"),
            Line::from("Esc, n, or q to cancel"),
        ])
        .block(card_block("Next Step", theme().highlight)),
        cards[2],
    );

    let buttons = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(18),
            Constraint::Length(3),
            Constraint::Length(18),
            Constraint::Fill(1),
        ])
        .split(rows[3]);
    draw_bootstrap_button(
        frame,
        buttons[1],
        "Create WORKFLOW.md",
        state.choice == BootstrapChoice::Create,
        theme().highlight,
    );
    draw_bootstrap_button(
        frame,
        buttons[3],
        "Cancel",
        state.choice == BootstrapChoice::Cancel,
        theme().muted,
    );
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &RuntimeSnapshot,
    log_buffer: &LogBuffer,
    app: &mut AppState,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme().background)),
        frame.area(),
    );

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Min(12),
            Constraint::Length(2),
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
    draw_footer(frame, areas[3], app);
}

fn draw_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let tabs = Tabs::new(
        ActiveTab::ALL
            .into_iter()
            .map(|tab| {
                Line::from(Span::styled(
                    tab.title(),
                    Style::default().fg(theme().muted),
                ))
            })
            .collect::<Vec<_>>(),
    )
    .select(app.active_tab.index())
    .divider(Span::raw("  "))
    .highlight_style(
        Style::default()
            .fg(theme().highlight)
            .add_modifier(Modifier::BOLD),
    )
    .block(panel_block("Polyphony"));
    frame.render_widget(tabs, sections[0]);

    let summary = vec![
        Line::from(vec![
            Span::styled("visible ", Style::default().fg(theme().muted)),
            Span::styled(
                snapshot.visible_issues.len().to_string(),
                Style::default()
                    .fg(theme().foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("running ", Style::default().fg(theme().muted)),
            Span::styled(
                snapshot.counts.running.to_string(),
                Style::default()
                    .fg(theme().success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("retry ", Style::default().fg(theme().muted)),
            Span::styled(
                snapshot.counts.retrying.to_string(),
                Style::default()
                    .fg(theme().warning)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("updated ", Style::default().fg(theme().muted)),
            Span::styled(
                snapshot.generated_at.format("%H:%M:%S").to_string(),
                Style::default().fg(theme().foreground),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(summary).block(panel_block("Live")),
        sections[1],
    );
}

fn draw_summary_strip(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
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
        theme().info,
    );
    draw_metric_card(
        frame,
        top[1],
        "Running",
        snapshot.counts.running.to_string(),
        "active workers",
        &app.history.running,
        theme().success,
    );
    draw_metric_card(
        frame,
        top[2],
        "Retry Queue",
        snapshot.counts.retrying.to_string(),
        "pending retries",
        &app.history.retrying,
        theme().warning,
    );
    draw_metric_card(
        frame,
        top[3],
        "Total Tokens",
        human_count(snapshot.codex_totals.total_tokens),
        "lifetime usage",
        &app.history.token_delta,
        theme().highlight,
    );

    draw_cadence_card(
        frame,
        bottom[0],
        "Tracker Poll",
        snapshot.generated_at,
        snapshot.cadence.last_tracker_poll_at,
        snapshot.cadence.tracker_poll_interval_ms,
        theme().info,
    );
    draw_cadence_card(
        frame,
        bottom[1],
        "Budget Refresh",
        snapshot.generated_at,
        snapshot.cadence.last_budget_poll_at,
        snapshot.cadence.budget_poll_interval_ms,
        theme().success,
    );
    draw_cadence_card(
        frame,
        bottom[2],
        "Model Scan",
        snapshot.generated_at,
        snapshot.cadence.last_model_discovery_at,
        snapshot.cadence.model_discovery_interval_ms,
        theme().highlight,
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
        theme().danger,
    );
}

fn draw_overview_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
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
    draw_retry_panel(frame, left[1], snapshot);
    draw_issue_detail(frame, right[0], snapshot, app);
    draw_budget_throttle_panel(frame, right[1], snapshot);
}

fn draw_activity_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
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
    draw_network_panel(frame, side[0], snapshot);
    draw_budget_throttle_panel(frame, side[1], snapshot);
}

fn draw_logs_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    log_buffer: &LogBuffer,
    app: &mut AppState,
) {
    let captured_lines = log_buffer.all_lines();
    let line_count = captured_lines.len();
    let mut lines = captured_lines
        .into_iter()
        .map(|line| Line::from(Span::raw(line)))
        .collect::<Vec<_>>();
    let subtitle = if line_count == 0 {
        lines = vec![
            Line::from(Span::styled(
                "No tracing lines captured yet.",
                Style::default()
                    .fg(theme().muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "If this stays empty, raise RUST_LOG (for example RUST_LOG=info).",
                Style::default().fg(theme().muted),
            )),
            Line::from(Span::styled(
                "Use PgUp/PgDn or g/G when the log view fills up.",
                Style::default().fg(theme().muted),
            )),
        ];
        "latest first | waiting for tracing output"
    } else {
        "latest first | PgUp PgDn g G scroll"
    };
    draw_scrollable_lines(
        frame,
        area,
        &format!("Logs  {line_count} lines"),
        &lines,
        &mut app.logs_scroll,
        subtitle,
    );
}

fn draw_agents_tab(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
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
    draw_budget_gauges(frame, side[1], snapshot);
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{} ", app.active_tab.title()),
            Style::default()
                .fg(theme().highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("1-4 tabs", Style::default().fg(theme().muted)),
        Span::raw("  "),
        Span::styled("tab / shift-tab switch", Style::default().fg(theme().muted)),
        Span::raw("  "),
        Span::styled("j k / arrows move", Style::default().fg(theme().muted)),
        Span::raw("  "),
        Span::styled("pgup pgdn g G scroll", Style::default().fg(theme().muted)),
        Span::raw("  "),
        Span::styled("r refresh", Style::default().fg(theme().muted)),
        Span::raw("  "),
        Span::styled("q quit", Style::default().fg(theme().muted)),
    ]))
    .block(panel_block("Controls"));
    frame.render_widget(footer, area);
}

fn draw_metric_card(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    value: String,
    subtitle: &str,
    history: &VecDeque<u64>,
    accent: Color,
) {
    frame.render_widget(card_block(title, accent), area);
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
                .fg(theme().foreground)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            subtitle,
            Style::default().fg(theme().muted),
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
) {
    let (ratio, label) = cadence_progress(now, last_at, interval_ms);
    frame.render_widget(
        LineGauge::default()
            .block(card_block(title, accent))
            .filled_style(Style::default().fg(accent))
            .unfilled_style(Style::default().fg(theme().border))
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
                .fg(theme().foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(selected_row_style())
    .highlight_symbol(">> ")
    .highlight_spacing(HighlightSpacing::Always)
    .block(panel_block("Running"));
    frame.render_stateful_widget(table, area, &mut app.running_state);
    draw_table_scrollbar(
        frame,
        area,
        snapshot.running.len(),
        app.running_state.selected(),
    );
}

fn draw_visible_issues_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let rows = snapshot.visible_issues.iter().map(|issue| {
        Row::new([
            Cell::from(issue.issue_identifier.clone()),
            Cell::from(issue.state.clone()),
            Cell::from(
                issue
                    .priority
                    .map(|priority| priority.to_string())
                    .unwrap_or_else(|| "-".into()),
            ),
            Cell::from(issue.labels.join(", ")),
            Cell::from(issue.title.clone()),
        ])
    });
    let table = Table::new(rows, [
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(18),
        Constraint::Min(18),
    ])
    .header(
        Row::new(["Issue", "State", "Pri", "Labels", "Title"]).style(
            Style::default()
                .fg(theme().foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(selected_row_style())
    .highlight_symbol(">> ")
    .highlight_spacing(HighlightSpacing::Always)
    .block(panel_block("Visible Issues"));
    frame.render_stateful_widget(table, area, &mut app.visible_state);
    draw_table_scrollbar(
        frame,
        area,
        snapshot.visible_issues.len(),
        app.visible_state.selected(),
    );
}

fn draw_retry_panel(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    if snapshot.retrying.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No queued retries.",
                Style::default().fg(theme().muted),
            )))
            .block(panel_block("Retry Queue")),
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
                .fg(theme().foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(panel_block("Retry Queue"));
    frame.render_widget(table, area);
}

fn draw_issue_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    frame.render_widget(panel_block("Inspector"), area);
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
                        .fg(theme().foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(running.state.clone(), Style::default().fg(theme().info)),
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
                .block(card_block("Turn Progress", theme().highlight))
                .gauge_style(Style::default().fg(theme().highlight).bg(theme().panel_alt))
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
                        .fg(theme().foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(issue.state.clone(), Style::default().fg(theme().info)),
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
                .block(card_block("Dispatch Coverage", theme().info))
                .gauge_style(Style::default().fg(theme().info).bg(theme().panel_alt))
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
                Style::default().fg(theme().muted),
            )))
            .wrap(ratatui::widgets::Wrap { trim: false }),
            chunks[2],
        );
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Nothing selected yet.",
            Style::default().fg(theme().muted),
        ))),
        inner,
    );
}

fn draw_budget_throttle_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
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
                        .fg(theme().foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(value, Style::default().fg(theme().muted)),
            ])
        })
        .collect::<Vec<_>>();

    lines.extend(snapshot.throttles.iter().map(|throttle| {
        Line::from(vec![
            Span::styled(
                "throttle",
                Style::default()
                    .fg(theme().warning)
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
                Style::default().fg(theme().muted),
            ),
        ])
    }));

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No budget probes or active throttles.",
            Style::default().fg(theme().muted),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Budgets & Throttles"))
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
    let lines = snapshot
        .recent_events
        .iter()
        .map(|event| {
            Line::from(vec![
                Span::styled(
                    event.at.format("%H:%M:%S").to_string(),
                    Style::default().fg(theme().muted),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("[{}]", event.scope),
                    Style::default().fg(scope_color(&event.scope)),
                ),
                Span::raw("  "),
                Span::styled(
                    event.message.clone(),
                    Style::default().fg(theme().foreground),
                ),
            ])
        })
        .collect::<Vec<_>>();
    draw_scrollable_lines(
        frame,
        area,
        &format!("Recent Events  {} items", lines.len()),
        &lines,
        &mut app.events_scroll,
        "latest first",
    );
}

fn draw_network_panel(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    frame.render_widget(panel_block("Network Cadence"), area);
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
            theme().info,
        ),
        (
            "budgets",
            snapshot.cadence.last_budget_poll_at,
            snapshot.cadence.budget_poll_interval_ms,
            theme().success,
        ),
        (
            "models",
            snapshot.cadence.last_model_discovery_at,
            snapshot.cadence.model_discovery_interval_ms,
            theme().highlight,
        ),
    ];

    for (index, (label, last_at, interval_ms, accent)) in gauges.into_iter().enumerate() {
        let (ratio, status) = cadence_progress(snapshot.generated_at, last_at, interval_ms);
        frame.render_widget(
            LineGauge::default()
                .ratio(ratio)
                .label(format!("{label}  {status}"))
                .filled_style(Style::default().fg(accent))
                .unfilled_style(Style::default().fg(theme().border))
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
                .fg(theme().foreground)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(selected_row_style())
    .highlight_symbol(">> ")
    .highlight_spacing(HighlightSpacing::Always)
    .block(panel_block("Agent Catalogs"));
    frame.render_stateful_widget(table, area, &mut app.models_state);
    draw_table_scrollbar(frame, area, catalogs.len(), app.models_state.selected());
}

fn draw_agent_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    frame.render_widget(panel_block("Selected Agent"), area);
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
                Style::default().fg(theme().muted),
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
                    .fg(theme().foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                catalog.provider_kind.clone(),
                Style::default().fg(theme().info),
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

fn draw_budget_gauges(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    frame.render_widget(panel_block("Budget Gauges"), area);
    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    if snapshot.budgets.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No budget probes configured.",
                Style::default().fg(theme().muted),
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
        let (ratio, label, accent) = budget_ratio_label(&budget);
        if let Some(ratio) = ratio {
            frame.render_widget(
                LineGauge::default()
                    .ratio(ratio)
                    .label(label)
                    .filled_style(Style::default().fg(accent))
                    .unfilled_style(Style::default().fg(theme().border))
                    .line_set(symbols::line::THICK),
                chunks[index],
            );
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    label,
                    Style::default().fg(theme().muted),
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
) {
    let max_scroll = max_scroll(lines.len(), area.height);
    *scroll = (*scroll).min(max_scroll);
    let block = panel_block(title).title_bottom(Line::from(Span::styled(
        subtitle,
        Style::default().fg(theme().muted),
    )));
    frame.render_widget(
        Paragraph::new(lines.to_vec())
            .block(block)
            .scroll((to_u16(*scroll), 0)),
        area,
    );
    let mut state = ScrollbarState::default()
        .content_length(lines.len())
        .position(*scroll);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .track_symbol(Some(BLOCK_SYMBOL))
            .end_symbol(None),
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
) {
    if rows_len <= 1 {
        return;
    }
    let mut state = ScrollbarState::default()
        .content_length(rows_len)
        .position(selected.unwrap_or_default());
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .track_symbol(Some(BLOCK_SYMBOL))
            .end_symbol(None),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

fn draw_bootstrap_button(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    label: &str,
    active: bool,
    accent: Color,
) {
    let (background, foreground, border) = if active {
        (accent, theme().background, accent)
    } else {
        (theme().panel, theme().foreground, theme().border)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().fg(foreground).add_modifier(Modifier::BOLD),
        )))
        .centered()
        .block(
            Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border))
                .style(Style::default().bg(background)),
        ),
        area,
    );
}

fn panel_block(title: &str) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme().foreground)
                .add_modifier(Modifier::BOLD),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme().border))
        .style(Style::default().bg(theme().panel))
}

fn card_block(title: &str, accent: Color) -> Block<'static> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )))
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme().border))
        .style(Style::default().bg(theme().panel_alt))
}

fn selected_row_style() -> Style {
    Style::default()
        .bg(theme().selection)
        .fg(theme().foreground)
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

fn budget_ratio_label(budget: &polyphony_core::BudgetSnapshot) -> (Option<f64>, String, Color) {
    if let (Some(remaining), Some(total)) = (budget.credits_remaining, budget.credits_total)
        && total > 0.0
    {
        let ratio = (remaining / total).clamp(0.0, 1.0);
        let accent = if ratio > 0.5 {
            theme().success
        } else if ratio > 0.2 {
            theme().warning
        } else {
            theme().danger
        };
        return (
            Some(ratio),
            format!(
                "{}  {:.1}/{:.1} credits",
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
            theme().success
        } else if ratio < 0.85 {
            theme().warning
        } else {
            theme().danger
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
        theme().muted,
    )
}

fn scope_color(scope: &str) -> Color {
    match scope {
        "dispatch" | "workflow" => theme().info,
        "retry" => theme().warning,
        "handoff" => theme().highlight,
        "throttle" => theme().danger,
        _ => theme().muted,
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

#[derive(Clone, Copy)]
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

const fn theme() -> Theme {
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

#[cfg(test)]
mod tests {
    use {
        super::{AppState, BootstrapChoice, BootstrapState, LogBuffer},
        chrono::Utc,
        polyphony_core::{
            CodexTotals, RuntimeCadence, RuntimeEvent, RuntimeSnapshot, SnapshotCounts,
            VisibleIssueRow,
        },
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
                scope: "workflow".into(),
                message: "updated".into(),
            }],
        }
    }

    #[test]
    fn log_buffer_keeps_newest_entries_within_capacity() {
        let buffer = LogBuffer::with_capacity(2);

        buffer.push_line("one");
        buffer.push_line("two");
        buffer.push_line("three");

        assert_eq!(buffer.recent_lines(3), vec!["three", "two"]);
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
}
