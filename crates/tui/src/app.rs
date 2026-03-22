use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Instant,
};

use polyphony_core::{
    AgentContextSnapshot, AgentHistoryRow, MovementRow, RunningRow, RuntimeSnapshot, TaskRow,
    VisibleTriggerRow,
};
use ratatui::{layout::Rect, widgets::TableState};

const RPS_HISTORY_CAP: usize = 120;
pub(crate) const TAB_DIVIDER: &str = "  ";
pub(crate) const TAB_PADDING_LEFT: &str = "";
pub(crate) const TAB_PADDING_RIGHT: &str = "";

use crate::{LogBuffer, theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub title: String,
    pub description: Option<String>,
    pub level: ToastLevel,
    pub created_at: Instant,
}

/// What to launch when the user presses `c` on an agent.
#[derive(Debug)]
pub(crate) enum CastPlayback {
    /// Replay a finished recording in the browser.
    Replay(std::path::PathBuf),
}

pub(crate) enum SelectedAgentRow<'a> {
    Running(&'a RunningRow),
    History(&'a AgentHistoryRow),
}

#[derive(Debug, Clone)]
pub(crate) struct AgentDetailArtifactCache {
    pub key: String,
    pub saved_context: Option<AgentContextSnapshot>,
}

/// Which section of a detail page has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DetailSection {
    /// The main body / description area (scrollable text).
    #[default]
    Body,
    /// A numbered sub-section (e.g., 0 = movements mini-list, 1 = agents mini-list).
    Section(u8),
}

/// In split (master-detail) mode, which pane has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SplitFocus {
    /// The list pane on the left.
    #[default]
    List,
    /// The detail pane on the right.
    Detail,
}

/// A detail view on the navigation stack. Each variant holds its own scroll
/// state and any cached artifacts needed for rendering.
#[derive(Debug, Clone)]
pub(crate) enum DetailView {
    Trigger {
        trigger_id: String,
        scroll: u16,
        focus: DetailSection,
        movements_selected: usize,
        agents_selected: usize,
    },
    Movement {
        movement_id: String,
        scroll: u16,
    },
    Task {
        task_id: String,
        scroll: u16,
    },
    Agent {
        agent_index: usize,
        scroll: u16,
        artifact_cache: Box<Option<AgentDetailArtifactCache>>,
    },
    Deliverable {
        movement_id: String,
        scroll: u16,
    },
    /// Full-screen filtered event log for a specific issue/trigger.
    Events {
        /// Filter key: events whose message contains this string are shown.
        filter: String,
        scroll: u16,
    },
    /// Live terminal output viewer for a running agent.
    LiveLog {
        log_path: std::path::PathBuf,
        agent_name: String,
        issue_identifier: String,
        scroll: u16,
        /// Cached rendered content — refreshed each tick.
        cached_content: String,
        /// Whether to auto-scroll to the bottom.
        auto_scroll: bool,
    },
}

impl DetailView {
    pub(crate) fn scroll(&self) -> u16 {
        match self {
            Self::Trigger { scroll, .. }
            | Self::Movement { scroll, .. }
            | Self::Task { scroll, .. }
            | Self::Agent { scroll, .. }
            | Self::Deliverable { scroll, .. }
            | Self::Events { scroll, .. }
            | Self::LiveLog { scroll, .. } => *scroll,
        }
    }

    pub(crate) fn scroll_mut(&mut self) -> &mut u16 {
        match self {
            Self::Trigger { scroll, .. }
            | Self::Movement { scroll, .. }
            | Self::Task { scroll, .. }
            | Self::Agent { scroll, .. }
            | Self::Deliverable { scroll, .. }
            | Self::Events { scroll, .. }
            | Self::LiveLog { scroll, .. } => scroll,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Triggers,
    Orchestrator,
    Tasks,
    Deliverables,
    Agents,
    Logs,
}

impl ActiveTab {
    pub const ALL: [Self; 6] = [
        Self::Triggers,
        Self::Orchestrator,
        Self::Tasks,
        Self::Deliverables,
        Self::Agents,
        Self::Logs,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Triggers => "Triggers",
            Self::Orchestrator => "Orchestration",
            Self::Tasks => "Tasks",
            Self::Deliverables => "Outcomes",
            Self::Agents => "Agents",
            Self::Logs => "Logs",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Triggers => 0,
            Self::Orchestrator => 1,
            Self::Tasks => 2,
            Self::Deliverables => 3,
            Self::Agents => 4,
            Self::Logs => 5,
        }
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Triggers)
    }

    pub fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }

    pub fn previous(self) -> Self {
        Self::from_index((self.index() + Self::ALL.len() - 1) % Self::ALL.len())
    }
}

/// A row in the Orchestration tab's tree view: movement, trigger, task, or outcome.
#[derive(Debug, Clone)]
pub(crate) enum OrchestratorTreeRow {
    Movement {
        snapshot_index: usize,
    },
    Trigger {
        trigger_index: usize,
        movement_snapshot_index: usize,
        is_last_child: bool,
    },
    /// An agent session (from history) shown under a movement.
    AgentSession {
        history_index: usize,
        is_last_child: bool,
    },
    /// A currently running agent shown under a movement.
    RunningAgent {
        running_index: usize,
        is_last_child: bool,
    },
    Task {
        snapshot_index: usize,
        is_last_child: bool,
    },
    Outcome {
        movement_snapshot_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementSortKey {
    Newest,
    Oldest,
    Status,
}

impl MovementSortKey {
    pub const ALL: [Self; 3] = [Self::Newest, Self::Oldest, Self::Status];

    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::Status => "status",
        }
    }

    pub fn cycle(self) -> Self {
        let idx = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSortKey {
    Newest,
    Oldest,
    Priority,
    State,
}

impl IssueSortKey {
    pub const ALL: [Self; 4] = [Self::Newest, Self::Oldest, Self::Priority, Self::State];

    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::Priority => "priority",
            Self::State => "state",
        }
    }

    pub fn cycle(self) -> Self {
        let idx = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

#[derive(Debug)]
pub struct AppState {
    pub theme: Theme,
    pub active_tab: ActiveTab,
    pub issues_state: TableState,
    pub agents_state: TableState,
    pub tasks_state: TableState,
    pub deliverables_state: TableState,
    pub movements_state: TableState,
    pub detail_stack: Vec<DetailView>,
    pub split_focus: SplitFocus,
    pub issue_sort: IssueSortKey,
    pub movement_sort: MovementSortKey,
    /// Sorted index mapping: sorted_issues[display_index] = original snapshot index
    pub sorted_issue_indices: Vec<usize>,
    /// Tree depth for each entry in sorted_issue_indices (0=root, 1=child)
    pub tree_depth: Vec<u8>,
    /// Whether each entry is the last child under its parent
    pub tree_last_child: Vec<bool>,
    pub frame_count: u64,
    pub leaving: bool,
    pub leaving_since: Option<Instant>,
    pub log_buffer: LogBuffer,
    pub prev_request_count: Option<u64>,
    pub prev_request_count_at: Option<Instant>,
    pub requests_per_sec: f64,
    pub search_active: bool,
    pub search_query: String,
    pub refresh_requested: bool,
    pub logs_state: TableState,
    pub movements_search_active: bool,
    pub movements_search_query: String,
    pub logs_search_active: bool,
    pub logs_search_query: String,
    pub logs_auto_scroll: bool,
    /// Cached parsed log entries — re-parsed only when the buffer grows.
    pub cached_log_entries: Vec<crate::render::logs::LogEntry>,
    pub cached_log_entry_count: usize,
    pub rps_history: VecDeque<u64>,
    pub prev_credits_used: f64,
    /// Inner area of the tab bar block, set each frame by draw_header.
    pub tab_inner_area: Rect,
    /// Content area (areas[1]), set each frame for mouse click row mapping.
    pub content_area: Rect,
    /// Whether the list pane border should use the focused style.
    pub list_border_focused: bool,
    /// Whether the detail pane border should use the focused style.
    pub detail_border_focused: bool,
    pub confirm_quit: bool,
    pub show_help_modal: bool,
    pub show_mode_modal: bool,
    pub mode_modal_selected: usize,
    pub show_agent_picker: bool,
    pub agent_picker_selected: usize,
    pub agent_picker_issue_id: Option<String>,
    /// Last left-click time for double-click detection.
    pub last_click_at: Option<Instant>,
    /// Column/row of last click for double-click detection.
    pub last_click_pos: (u16, u16),
    /// Last scroll event time for debouncing.
    pub last_scroll_at: Option<Instant>,
    /// Scroll offset for the orchestrator events panel.
    pub events_scroll: u16,
    /// Bounding rect of the events panel (set each frame by draw_events_panel).
    pub events_area: Rect,
    /// Sorted task indices: sorted_task_indices[display_index] = original snapshot index
    pub sorted_task_indices: Vec<usize>,
    /// Sorted movement indices for the Orchestration tab.
    pub sorted_movement_indices: Vec<usize>,
    pub orchestrator_tree_rows: Vec<OrchestratorTreeRow>,
    pub collapsed_movements: HashSet<String>,
    collapsed_movements_initialized: bool,
    /// Sorted agent indices: each entry is (is_running, original_index).
    /// Sorted by started_at ascending (oldest first, newest at bottom).
    pub sorted_agent_indices: Vec<(bool, usize)>,
    /// When set, the main loop will suspend the TUI and play this agent recording.
    pub pending_cast_playback: Option<CastPlayback>,
    /// Toast notification shown briefly at the bottom of the screen.
    pub toast: Option<Toast>,
    /// Trigger IDs that have been dispatched but not yet running.
    /// Cleared when the trigger appears in `snapshot.running`.
    pub dispatching_triggers: HashSet<String>,
}

impl AppState {
    pub fn new(theme: Theme, log_buffer: LogBuffer) -> Self {
        Self {
            theme,
            active_tab: ActiveTab::Triggers,
            issues_state: TableState::default(),
            agents_state: TableState::default(),
            tasks_state: TableState::default(),
            deliverables_state: TableState::default(),
            movements_state: TableState::default(),
            detail_stack: Vec::new(),
            split_focus: SplitFocus::default(),
            issue_sort: IssueSortKey::Oldest,
            movement_sort: MovementSortKey::Oldest,
            sorted_issue_indices: Vec::new(),
            tree_depth: Vec::new(),
            tree_last_child: Vec::new(),
            frame_count: 0,
            leaving: false,
            leaving_since: None,
            log_buffer,
            prev_request_count: None,
            prev_request_count_at: None,
            requests_per_sec: 0.0,
            search_active: false,
            search_query: String::new(),
            refresh_requested: false,
            logs_state: TableState::default(),
            movements_search_active: false,
            movements_search_query: String::new(),
            logs_search_active: false,
            logs_search_query: String::new(),
            logs_auto_scroll: true,
            cached_log_entries: Vec::new(),
            cached_log_entry_count: 0,
            rps_history: VecDeque::with_capacity(RPS_HISTORY_CAP),
            prev_credits_used: 0.0,
            tab_inner_area: Rect::default(),
            content_area: Rect::default(),
            list_border_focused: true,
            detail_border_focused: false,
            confirm_quit: false,
            show_help_modal: false,
            show_mode_modal: false,
            mode_modal_selected: 0,
            show_agent_picker: false,
            agent_picker_selected: 0,
            agent_picker_issue_id: None,
            last_click_at: None,
            last_click_pos: (0, 0),
            last_scroll_at: None,
            events_scroll: 0,
            events_area: Rect::default(),
            sorted_task_indices: Vec::new(),
            sorted_movement_indices: Vec::new(),
            orchestrator_tree_rows: Vec::new(),
            collapsed_movements: HashSet::new(),
            collapsed_movements_initialized: false,
            sorted_agent_indices: Vec::new(),
            pending_cast_playback: None,
            toast: None,
            dispatching_triggers: HashSet::new(),
        }
    }

    /// Show a toast notification that auto-expires after a few seconds.
    pub fn show_toast(
        &mut self,
        level: ToastLevel,
        title: impl Into<String>,
        description: Option<String>,
    ) {
        self.toast = Some(Toast {
            title: title.into(),
            description,
            level,
            created_at: Instant::now(),
        });
    }

    /// Clear expired toasts.
    pub fn expire_toast(&mut self) {
        if let Some(toast) = &self.toast {
            let ttl = match toast.level {
                ToastLevel::Error => std::time::Duration::from_secs(5),
                ToastLevel::Warning => std::time::Duration::from_secs(4),
                ToastLevel::Info => std::time::Duration::from_secs(3),
            };
            if toast.created_at.elapsed() > ttl {
                self.toast = None;
            }
        }
    }

    pub fn on_snapshot(&mut self, snapshot: &RuntimeSnapshot) {
        // Clear refresh indicator once we get a live (non-cached) snapshot
        if self.refresh_requested && !snapshot.from_cache {
            self.refresh_requested = false;
        }
        // Clear dispatching indicators for triggers that are now running or have movements
        if !self.dispatching_triggers.is_empty() {
            let running_ids: HashSet<&str> = snapshot
                .running
                .iter()
                .map(|r| r.issue_id.as_str())
                .collect();
            let movement_ids: HashSet<&str> = snapshot
                .movements
                .iter()
                .filter_map(|m| m.issue_identifier.as_deref())
                .collect();
            self.dispatching_triggers.retain(|id| {
                !running_ids.contains(id.as_str()) && !movement_ids.contains(id.as_str())
            });
        }
        self.rebuild_sorted_indices(snapshot);
        sync_selection(&mut self.issues_state, self.sorted_issue_indices.len());
        // Rebuild sorted agent indices (oldest first, newest at bottom)
        {
            let mut indices: Vec<(bool, usize)> =
                Vec::with_capacity(snapshot.running.len() + snapshot.agent_history.len());
            for i in 0..snapshot.running.len() {
                indices.push((true, i));
            }
            for i in 0..snapshot.agent_history.len() {
                indices.push((false, i));
            }
            indices.sort_by_key(|&(is_running, idx)| {
                if is_running {
                    snapshot.running[idx].started_at
                } else {
                    snapshot.agent_history[idx].started_at
                }
            });
            self.sorted_agent_indices = indices;
        }
        let previous_agent_selection = self.agents_state.selected();
        sync_selection(&mut self.agents_state, self.sorted_agent_indices.len());
        if self.agents_state.selected() != previous_agent_selection
            && let Some(DetailView::Agent {
                scroll,
                artifact_cache,
                ..
            }) = self.detail_stack.last_mut()
        {
            *scroll = 0;
            **artifact_cache = None;
        }
        sync_selection(&mut self.tasks_state, snapshot.tasks.len());
        let deliverable_count = snapshot
            .movements
            .iter()
            .filter(|m| m.deliverable.is_some())
            .count();
        sync_selection(&mut self.deliverables_state, deliverable_count);
        // Keep sorted movement indices in sync with the snapshot.
        let mut movement_indices: Vec<usize> = (0..snapshot.movements.len()).collect();
        match self.movement_sort {
            MovementSortKey::Oldest => {
                movement_indices.sort_by_key(|&i| snapshot.movements[i].created_at);
            },
            MovementSortKey::Newest => {
                movement_indices
                    .sort_by_key(|&i| std::cmp::Reverse(snapshot.movements[i].created_at));
            },
            MovementSortKey::Status => {
                movement_indices.sort_by_key(|&i| snapshot.movements[i].status.to_string());
            },
        }
        self.sorted_movement_indices = movement_indices;
        self.rebuild_orchestrator_tree(snapshot);
        let previous_movement_selection = self.movements_state.selected();
        sync_selection(&mut self.movements_state, self.orchestrator_tree_rows.len());
        if self.movements_state.selected() != previous_movement_selection
            && let Some(DetailView::Movement { scroll, .. }) = self.detail_stack.last_mut()
        {
            *scroll = 0;
        }

        // Compute requests/sec from budget raw data
        let total_requests: u64 = snapshot
            .budgets
            .iter()
            .filter_map(|b| {
                b.raw
                    .as_ref()
                    .and_then(|v| v.get("requests"))
                    .and_then(|v| v.as_u64())
            })
            .sum();

        if total_requests > 0 {
            let now = Instant::now();
            if let (Some(prev_count), Some(prev_at)) =
                (self.prev_request_count, self.prev_request_count_at)
            {
                let elapsed = now.duration_since(prev_at).as_secs_f64();
                if elapsed > 0.5 {
                    let delta = total_requests.saturating_sub(prev_count);
                    self.requests_per_sec = delta as f64 / elapsed;
                    self.prev_request_count = Some(total_requests);
                    self.prev_request_count_at = Some(now);
                }
            } else {
                self.prev_request_count = Some(total_requests);
                self.prev_request_count_at = Some(now);
            }
        }

        // Sparkline: track credit consumption deltas
        let credits_used: f64 = snapshot
            .budgets
            .iter()
            .map(|b| {
                let total = b.credits_total.unwrap_or(0.0);
                let remaining = b.credits_remaining.unwrap_or(total);
                (total - remaining).max(0.0)
            })
            .sum();
        let credit_delta = (credits_used - self.prev_credits_used).max(0.0);
        self.prev_credits_used = credits_used;
        // Scale up for sparkline visibility (1 credit = 10 units)
        self.rps_history.push_back((credit_delta * 10.0) as u64);
        if self.rps_history.len() > RPS_HISTORY_CAP {
            self.rps_history.pop_front();
        }

        // Auto-pop detail if the viewed entity disappeared from the snapshot
        if let Some(detail) = self.detail_stack.last() {
            let missing = match detail {
                DetailView::Trigger { trigger_id, .. } => !snapshot
                    .visible_triggers
                    .iter()
                    .any(|t| t.trigger_id == *trigger_id),
                DetailView::Movement { movement_id, .. }
                | DetailView::Deliverable { movement_id, .. } => {
                    !snapshot.movements.iter().any(|m| m.id == *movement_id)
                },
                DetailView::Task { task_id, .. } => {
                    !snapshot.tasks.iter().any(|t| t.id == *task_id)
                },
                DetailView::Agent { agent_index, .. } => {
                    *agent_index >= self.sorted_agent_indices.len()
                },
                DetailView::Events { .. } | DetailView::LiveLog { .. } => false,
            };
            if missing {
                self.detail_stack.pop();
            }
        }
    }

    pub fn rebuild_orchestrator_tree(&mut self, snapshot: &RuntimeSnapshot) {
        use std::collections::HashMap as StdMap;

        use polyphony_core::MovementStatus;

        // Auto-collapse terminal movements on first load
        if !self.collapsed_movements_initialized && !snapshot.movements.is_empty() {
            for m in &snapshot.movements {
                if matches!(
                    m.status,
                    MovementStatus::Delivered | MovementStatus::Failed | MovementStatus::Cancelled
                ) {
                    self.collapsed_movements.insert(m.id.clone());
                }
            }
            self.collapsed_movements_initialized = true;
        }

        // Group tasks by movement_id, sorted by ordinal
        let mut tasks_by_movement: StdMap<&str, Vec<usize>> = StdMap::new();
        for (i, task) in snapshot.tasks.iter().enumerate() {
            tasks_by_movement
                .entry(&task.movement_id)
                .or_default()
                .push(i);
        }
        for tasks in tasks_by_movement.values_mut() {
            tasks.sort_by_key(|&i| snapshot.tasks[i].ordinal);
        }

        // Build trigger lookup by identifier
        let trigger_by_identifier: StdMap<&str, usize> = snapshot
            .visible_triggers
            .iter()
            .enumerate()
            .map(|(i, t)| (t.identifier.as_str(), i))
            .collect();

        // Build agent session lookup by issue_id (from history), sorted by started_at
        let mut sessions_by_issue: StdMap<&str, Vec<usize>> = StdMap::new();
        for (i, session) in snapshot.agent_history.iter().enumerate() {
            sessions_by_issue
                .entry(&session.issue_id)
                .or_default()
                .push(i);
        }
        for sessions in sessions_by_issue.values_mut() {
            sessions.sort_by_key(|&i| snapshot.agent_history[i].started_at);
        }
        // Also index running sessions
        let mut running_by_issue: StdMap<&str, Vec<usize>> = StdMap::new();
        for (i, running) in snapshot.running.iter().enumerate() {
            running_by_issue
                .entry(&running.issue_id)
                .or_default()
                .push(i);
        }

        let mut rows = Vec::new();
        for &mov_idx in &self.sorted_movement_indices {
            let movement = &snapshot.movements[mov_idx];
            rows.push(OrchestratorTreeRow::Movement {
                snapshot_index: mov_idx,
            });
            if !self.collapsed_movements.contains(&movement.id) {
                let task_indices = tasks_by_movement.get(movement.id.as_str());
                let has_tasks = task_indices.is_some_and(|t| !t.is_empty());
                let has_outcome = movement.deliverable.is_some()
                    || matches!(
                        movement.status,
                        polyphony_core::MovementStatus::Delivered
                            | polyphony_core::MovementStatus::Failed
                    );
                // Collect agent sessions for this movement's issue
                let issue_id = movement.issue_identifier.as_deref().and_then(|ident| {
                    snapshot
                        .visible_triggers
                        .iter()
                        .find(|t| t.identifier == ident)
                        .map(|t| t.trigger_id.as_str())
                });
                let session_indices = issue_id.and_then(|id| sessions_by_issue.get(id));
                let running_indices = issue_id.and_then(|id| running_by_issue.get(id));
                let has_sessions = session_indices.is_some_and(|s| !s.is_empty());
                let has_running = running_indices.is_some_and(|r| !r.is_empty());
                let has_children = has_tasks || has_outcome || has_sessions || has_running;

                // Trigger row (first child)
                if let Some(identifier) = movement.issue_identifier.as_deref()
                    && let Some(&trigger_idx) = trigger_by_identifier.get(identifier)
                {
                    rows.push(OrchestratorTreeRow::Trigger {
                        trigger_index: trigger_idx,
                        movement_snapshot_index: mov_idx,
                        is_last_child: !has_children,
                    });
                }

                // Build a chronological timeline of sessions and tasks.
                // Collect task agent_names to separate planning vs execution sessions.
                let task_agent_names: std::collections::HashSet<&str> = task_indices
                    .map(|idxs| {
                        idxs.iter()
                            .filter_map(|&i| snapshot.tasks[i].agent_name.as_deref())
                            .collect()
                    })
                    .unwrap_or_default();

                // Split sessions into planning (non-task) and execution (task-matching).
                let mut planning_sessions: Vec<usize> = Vec::new();
                let mut execution_sessions: Vec<usize> = Vec::new();
                if let Some(indices) = session_indices {
                    for &idx in indices {
                        let session = &snapshot.agent_history[idx];
                        if task_agent_names.contains(session.agent_name.as_str()) {
                            execution_sessions.push(idx);
                        } else {
                            planning_sessions.push(idx);
                        }
                    }
                }
                planning_sessions.sort_by_key(|&i| snapshot.agent_history[i].started_at);
                execution_sessions.sort_by_key(|&i| snapshot.agent_history[i].started_at);

                // Assign execution sessions to tasks by time windows.
                // Each task claims sessions between its started_at and the next task's started_at.
                let sorted_tasks: Vec<usize> = task_indices
                    .map(|idxs| {
                        let mut v = idxs.clone();
                        v.sort_by_key(|&i| snapshot.tasks[i].started_at);
                        v
                    })
                    .unwrap_or_default();
                let mut sessions_per_task: Vec<Vec<usize>> = vec![Vec::new(); sorted_tasks.len()];
                for &sess_idx in &execution_sessions {
                    let sess_start = snapshot.agent_history[sess_idx].started_at;
                    // Find the last task whose started_at <= session started_at
                    let mut best_task = None;
                    for (ti, &task_idx) in sorted_tasks.iter().enumerate() {
                        if let Some(task_start) = snapshot.tasks[task_idx].started_at {
                            if task_start <= sess_start {
                                best_task = Some(ti);
                            }
                        }
                    }
                    if let Some(ti) = best_task {
                        sessions_per_task[ti].push(sess_idx);
                    } else {
                        // Session predates all tasks — treat as planning
                        planning_sessions.push(sess_idx);
                    }
                }
                // Re-sort planning sessions since we may have appended
                planning_sessions.sort_by_key(|&i| snapshot.agent_history[i].started_at);

                // Count remaining children after planning sessions
                let total_remaining = sorted_tasks.len()
                    + sessions_per_task.iter().map(|s| s.len()).sum::<usize>()
                    + running_indices.map_or(0, |r| r.len())
                    + usize::from(has_outcome);

                // Emit planning sessions
                let plan_count = planning_sessions.len();
                for (ci, &hist_idx) in planning_sessions.iter().enumerate() {
                    let is_last = ci == plan_count - 1 && total_remaining == 0;
                    rows.push(OrchestratorTreeRow::AgentSession {
                        history_index: hist_idx,
                        is_last_child: is_last,
                    });
                }

                // Emit tasks interleaved with their execution sessions
                let remaining_after_tasks =
                    running_indices.map_or(0, |r| r.len()) + usize::from(has_outcome);
                for (ti, &task_idx) in sorted_tasks.iter().enumerate() {
                    let task_sessions = &sessions_per_task[ti];
                    let tasks_after = sorted_tasks.len() - ti - 1;
                    let sessions_after: usize =
                        sessions_per_task[ti + 1..].iter().map(|s| s.len()).sum();
                    let items_after = tasks_after + sessions_after + remaining_after_tasks;

                    let is_last_task = items_after == 0 && task_sessions.is_empty();
                    rows.push(OrchestratorTreeRow::Task {
                        snapshot_index: task_idx,
                        is_last_child: is_last_task,
                    });
                    let sess_count = task_sessions.len();
                    for (si, &sess_idx) in task_sessions.iter().enumerate() {
                        let is_last = si == sess_count - 1 && items_after == 0;
                        rows.push(OrchestratorTreeRow::AgentSession {
                            history_index: sess_idx,
                            is_last_child: is_last,
                        });
                    }
                }

                // Running agent rows (after tasks, before outcome)
                if let Some(running_indices) = running_indices {
                    let count = running_indices.len();
                    for (ci, &run_idx) in running_indices.iter().enumerate() {
                        let is_last = ci == count - 1 && !has_outcome;
                        rows.push(OrchestratorTreeRow::RunningAgent {
                            running_index: run_idx,
                            is_last_child: is_last,
                        });
                    }
                }
                if has_outcome {
                    rows.push(OrchestratorTreeRow::Outcome {
                        movement_snapshot_index: mov_idx,
                    });
                }
            }
        }
        self.orchestrator_tree_rows = rows;
    }

    pub fn toggle_movement_collapse(&mut self, movement_id: &str) {
        if !self.collapsed_movements.remove(movement_id) {
            self.collapsed_movements.insert(movement_id.to_string());
        }
    }

    pub fn rebuild_sorted_indices(&mut self, snapshot: &RuntimeSnapshot) {
        let issues = &snapshot.visible_triggers;
        let mut indices: Vec<usize> = if self.search_query.is_empty() {
            (0..issues.len()).collect()
        } else {
            let q = self.search_query.to_lowercase();
            (0..issues.len())
                .filter(|&i| issue_matches(&issues[i], &q))
                .collect()
        };
        match self.issue_sort {
            IssueSortKey::Newest => {
                indices.sort_by(|&a, &b| {
                    let ta = issues[a].created_at.as_ref();
                    let tb = issues[b].created_at.as_ref();
                    match (ta, tb) {
                        (Some(a_t), Some(b_t)) => b_t.cmp(a_t),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => {
                            let na = extract_issue_number(&issues[a].identifier);
                            let nb = extract_issue_number(&issues[b].identifier);
                            nb.cmp(&na)
                        },
                    }
                });
            },
            IssueSortKey::Oldest => {
                indices.sort_by(|&a, &b| {
                    let ta = issues[a].created_at.as_ref();
                    let tb = issues[b].created_at.as_ref();
                    match (ta, tb) {
                        (Some(a_t), Some(b_t)) => a_t.cmp(b_t),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => {
                            let na = extract_issue_number(&issues[a].identifier);
                            let nb = extract_issue_number(&issues[b].identifier);
                            na.cmp(&nb)
                        },
                    }
                });
            },
            IssueSortKey::Priority => {
                indices.sort_by(|&a, &b| {
                    let pa = issues[a].priority.unwrap_or(i32::MAX);
                    let pb = issues[b].priority.unwrap_or(i32::MAX);
                    pa.cmp(&pb)
                });
            },
            IssueSortKey::State => {
                indices.sort_by(|&a, &b| issues[a].status.cmp(&issues[b].status))
            },
        }
        // Tree grouping: place children immediately after their parent
        let mut children_by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
        for &idx in &indices {
            if let Some(ref pid) = issues[idx].parent_id {
                children_by_parent
                    .entry(pid.as_str())
                    .or_default()
                    .push(idx);
            }
        }
        for child_indices in children_by_parent.values_mut() {
            child_indices.sort_by(|&a, &b| {
                let na = extract_issue_number(&issues[a].identifier);
                let nb = extract_issue_number(&issues[b].identifier);
                na.cmp(&nb)
                    .then_with(|| issues[a].identifier.cmp(&issues[b].identifier))
                    .then_with(|| issues[a].title.cmp(&issues[b].title))
            });
        }
        // Only group if there are any parent-child relationships
        if !children_by_parent.is_empty() {
            let child_set: std::collections::HashSet<usize> = children_by_parent
                .values()
                .flat_map(|v| v.iter().copied())
                .collect();
            // Build a set of parent issue_ids that are visible in this list
            let visible_parents: std::collections::HashSet<&str> = indices
                .iter()
                .filter(|&&idx| children_by_parent.contains_key(issues[idx].trigger_id.as_str()))
                .map(|&idx| issues[idx].trigger_id.as_str())
                .collect();

            let mut grouped = Vec::with_capacity(indices.len());
            let mut depth = Vec::with_capacity(indices.len());
            let mut last_child = Vec::with_capacity(indices.len());
            for &idx in &indices {
                let is_child = child_set.contains(&idx);
                // Skip children here; they'll be inserted after their parent
                if is_child
                    && let Some(ref pid) = issues[idx].parent_id
                    && visible_parents.contains(pid.as_str())
                {
                    continue;
                }
                grouped.push(idx);
                depth.push(0);
                last_child.push(false);
                // Insert children after this parent
                if let Some(kids) = children_by_parent.get(issues[idx].trigger_id.as_str()) {
                    for (ci, &kid_idx) in kids.iter().enumerate() {
                        grouped.push(kid_idx);
                        depth.push(1);
                        last_child.push(ci == kids.len() - 1);
                    }
                }
            }
            self.sorted_issue_indices = grouped;
            self.tree_depth = depth;
            self.tree_last_child = last_child;
        } else {
            let len = indices.len();
            self.sorted_issue_indices = indices;
            self.tree_depth = vec![0; len];
            self.tree_last_child = vec![false; len];
        }
    }

    /// Get the trigger at the given display row (sorted).
    pub fn sorted_trigger<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
        display_index: usize,
    ) -> Option<&'a VisibleTriggerRow> {
        self.sorted_issue_indices
            .get(display_index)
            .and_then(|&orig| snapshot.visible_triggers.get(orig))
    }

    /// Get the currently selected trigger (in sorted order).
    pub fn selected_trigger<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
    ) -> Option<&'a VisibleTriggerRow> {
        self.issues_state
            .selected()
            .and_then(|display_idx| self.sorted_trigger(snapshot, display_idx))
    }

    #[allow(dead_code)]
    pub fn selected_agent<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
    ) -> Option<SelectedAgentRow<'a>> {
        let display_idx = self.agents_state.selected()?;
        let &(is_running, orig_idx) = self.sorted_agent_indices.get(display_idx)?;
        if is_running {
            snapshot
                .running
                .get(orig_idx)
                .map(SelectedAgentRow::Running)
        } else {
            snapshot
                .agent_history
                .get(orig_idx)
                .map(SelectedAgentRow::History)
        }
    }

    /// Resolve an agent from a sorted display index.
    pub fn resolve_agent<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
        display_index: usize,
    ) -> Option<SelectedAgentRow<'a>> {
        let &(is_running, orig_idx) = self.sorted_agent_indices.get(display_index)?;
        if is_running {
            snapshot
                .running
                .get(orig_idx)
                .map(SelectedAgentRow::Running)
        } else {
            snapshot
                .agent_history
                .get(orig_idx)
                .map(SelectedAgentRow::History)
        }
    }

    pub fn selected_movement<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a MovementRow> {
        self.movements_state
            .selected()
            .and_then(|idx| self.orchestrator_tree_rows.get(idx))
            .and_then(|row| match row {
                OrchestratorTreeRow::Movement { snapshot_index } => {
                    snapshot.movements.get(*snapshot_index)
                },
                OrchestratorTreeRow::Trigger { .. }
                | OrchestratorTreeRow::AgentSession { .. }
                | OrchestratorTreeRow::RunningAgent { .. }
                | OrchestratorTreeRow::Task { .. }
                | OrchestratorTreeRow::Outcome { .. } => None,
            })
    }

    pub fn selected_orchestrator_row(&self) -> Option<&OrchestratorTreeRow> {
        self.movements_state
            .selected()
            .and_then(|idx| self.orchestrator_tree_rows.get(idx))
    }

    pub fn selected_task<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a TaskRow> {
        self.tasks_state
            .selected()
            .and_then(|display_idx| self.sorted_task_indices.get(display_idx))
            .and_then(|&orig_idx| snapshot.tasks.get(orig_idx))
    }

    pub fn selected_deliverable<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
    ) -> Option<&'a MovementRow> {
        let selected = self.deliverables_state.selected()?;
        snapshot
            .movements
            .iter()
            .filter(|movement| movement.deliverable.is_some())
            .nth(selected)
    }

    pub fn active_table_len(&self, snapshot: &RuntimeSnapshot) -> usize {
        match self.active_tab {
            ActiveTab::Triggers => self.sorted_issue_indices.len(),
            ActiveTab::Agents => self.sorted_agent_indices.len(),
            ActiveTab::Orchestrator => self.orchestrator_tree_rows.len(),
            ActiveTab::Tasks => snapshot.tasks.len(),
            ActiveTab::Deliverables => snapshot
                .movements
                .iter()
                .filter(|m| m.deliverable.is_some())
                .count(),
            ActiveTab::Logs => self.filtered_log_count(),
        }
    }

    pub fn active_table_state_mut(&mut self) -> &mut TableState {
        match self.active_tab {
            ActiveTab::Triggers => &mut self.issues_state,
            ActiveTab::Agents => &mut self.agents_state,
            ActiveTab::Orchestrator => &mut self.movements_state,
            ActiveTab::Tasks => &mut self.tasks_state,
            ActiveTab::Deliverables => &mut self.deliverables_state,
            ActiveTab::Logs => &mut self.logs_state,
        }
    }

    pub fn filtered_log_count(&self) -> usize {
        let lines = self.log_buffer.all_lines();
        if self.logs_search_query.is_empty() {
            lines.len()
        } else {
            let q = self.logs_search_query.to_lowercase();
            lines
                .iter()
                .filter(|l| l.to_lowercase().contains(&q))
                .count()
        }
    }

    pub fn move_down(&mut self, len: usize, amount: usize) {
        move_selection(self.active_table_state_mut(), len, amount);
    }

    pub fn move_up(&mut self, len: usize, amount: usize) {
        move_selection_back(self.active_table_state_mut(), len, amount);
    }

    // --- Detail stack helpers ---

    pub(crate) fn has_detail(&self) -> bool {
        !self.detail_stack.is_empty()
    }

    pub(crate) fn current_detail(&self) -> Option<&DetailView> {
        self.detail_stack.last()
    }

    pub(crate) fn current_detail_mut(&mut self) -> Option<&mut DetailView> {
        self.detail_stack.last_mut()
    }

    pub(crate) fn push_detail(&mut self, view: DetailView) {
        self.detail_stack.push(view);
    }

    pub(crate) fn pop_detail(&mut self) {
        self.detail_stack.pop();
    }

    pub(crate) fn clear_detail_stack(&mut self) {
        self.detail_stack.clear();
        self.split_focus = SplitFocus::default();
    }

    /// Whether the terminal is wide enough for the split master-detail layout.
    /// Must be called with the current terminal width (from frame area).
    pub(crate) fn is_split_eligible(&self) -> bool {
        self.detail_stack.len() == 1 && !matches!(self.active_tab, ActiveTab::Logs)
    }

    pub(crate) fn current_detail_scroll(&self) -> u16 {
        self.detail_stack.last().map(|d| d.scroll()).unwrap_or(0)
    }

    pub(crate) fn set_current_detail_scroll(&mut self, value: u16) {
        if let Some(detail) = self.detail_stack.last_mut() {
            *detail.scroll_mut() = value;
        }
    }

    /// Return the issue table row index for a click position, if valid.
    /// The table has: 1 border + 1 header row before data rows.
    pub fn issue_row_at_position(&self, row: u16) -> Option<usize> {
        let area = self.content_area;
        if area.height == 0 || row < area.y + 2 || row >= area.y + area.height - 1 {
            return None;
        }
        let clicked_row = (row - area.y - 2) as usize;
        let index = clicked_row + self.issues_state.offset();
        if index < self.sorted_issue_indices.len() {
            Some(index)
        } else {
            None
        }
    }

    /// Return the task table row index for a click position, if valid.
    /// The table has: 1 border + 1 header row before data rows.
    pub fn table_row_at_position(&self, row: u16) -> Option<usize> {
        let area = self.content_area;
        if area.height == 0 || row < area.y + 2 || row >= area.y + area.height - 1 {
            return None;
        }
        let clicked_row = (row - area.y - 2) as usize;
        let index = clicked_row + self.tasks_state.offset();
        if index < self.sorted_task_indices.len() {
            Some(index)
        } else {
            None
        }
    }

    /// Return the tab that was clicked, if the position falls within a tab label.
    pub fn tab_at_position(&self, column: u16, row: u16) -> Option<ActiveTab> {
        let area = self.tab_inner_area;
        if area.width == 0 || row < area.y || row >= area.y + area.height {
            return None;
        }
        if column < area.x || column >= area.x + area.width {
            return None;
        }
        // Ratatui Tabs renders: [pad_left][title][pad_right][divider] for each tab.
        // Keep this in sync with render::header tab padding/divider.
        let rel = (column - area.x) as usize;
        let pad_left = TAB_PADDING_LEFT.len();
        let pad_right = TAB_PADDING_RIGHT.len();
        let divider_len = TAB_DIVIDER.len();
        let mut pos = 0;
        let tab_count = ActiveTab::ALL.len();
        for (i, tab) in ActiveTab::ALL.iter().enumerate() {
            let title_len = tab.title().len();
            let clickable = pad_left + title_len + pad_right;
            if rel >= pos && rel < pos + clickable {
                return Some(*tab);
            }
            // After the last tab there's no divider.
            pos += clickable
                + if i < tab_count - 1 {
                    divider_len
                } else {
                    0
                };
        }
        None
    }
}

/// Extract a numeric issue number from identifiers like "#375", "GH-42", "PROJ-123".
fn extract_issue_number(identifier: &str) -> u64 {
    identifier
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

fn issue_matches(issue: &VisibleTriggerRow, query: &str) -> bool {
    issue.title.to_lowercase().contains(query)
        || issue.identifier.to_lowercase().contains(query)
        || issue.status.to_lowercase().contains(query)
        || issue.source.to_lowercase().contains(query)
        || issue.kind.to_string().contains(query)
        || issue
            .labels
            .iter()
            .any(|l| l.to_lowercase().contains(query))
        || issue
            .description
            .as_deref()
            .map(|d| d.to_lowercase().contains(query))
            .unwrap_or(false)
        || issue
            .url
            .as_deref()
            .map(|u| u.to_lowercase().contains(query))
            .unwrap_or(false)
}

fn sync_selection(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    match state.selected() {
        Some(index) if index < len => {},
        Some(_) => state.select(Some(len - 1)),
        None => state.select(Some(len - 1)),
    }
}

fn move_selection(state: &mut TableState, len: usize, amount: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or_default();
    let next = (current + amount).min(len - 1);
    state.select(Some(next));
}

fn move_selection_back(state: &mut TableState, len: usize, amount: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or_default();
    let previous = current.saturating_sub(amount);
    state.select(Some(previous));
}
