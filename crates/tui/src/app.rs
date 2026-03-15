use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};

use {
    polyphony_core::{RunningRow, RuntimeSnapshot, VisibleTriggerRow},
    ratatui::{layout::Rect, widgets::TableState},
};

const RPS_HISTORY_CAP: usize = 120;

use crate::{LogBuffer, theme::Theme};

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
            Self::Orchestrator => "Flow",
            Self::Tasks => "Tasks",
            Self::Deliverables => "Output",
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
    pub show_issue_detail: bool,
    pub detail_scroll: u16,
    pub movement_detail_scroll: u16,
    pub agents_detail_scroll: u16,
    pub issue_sort: IssueSortKey,
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
    pub logs_search_active: bool,
    pub logs_search_query: String,
    pub logs_auto_scroll: bool,
    pub rps_history: VecDeque<u64>,
    pub prev_credits_used: f64,
    /// Inner area of the tab bar block, set each frame by draw_header.
    pub tab_inner_area: Rect,
    /// Content area (areas[1]), set each frame for mouse click row mapping.
    pub content_area: Rect,
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
    /// Bounding rect of the agent detail panel (set each frame by draw_agent_detail).
    pub agents_detail_area: Rect,
    /// Bounding rect of the movement detail panel (set each frame by draw_movement_detail).
    pub movement_detail_area: Rect,
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
            show_issue_detail: false,
            detail_scroll: 0,
            movement_detail_scroll: 0,
            agents_detail_scroll: 0,
            issue_sort: IssueSortKey::Newest,
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
            logs_search_active: false,
            logs_search_query: String::new(),
            logs_auto_scroll: true,
            rps_history: VecDeque::with_capacity(RPS_HISTORY_CAP),
            prev_credits_used: 0.0,
            tab_inner_area: Rect::default(),
            content_area: Rect::default(),
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
            agents_detail_area: Rect::default(),
            movement_detail_area: Rect::default(),
        }
    }

    pub fn on_snapshot(&mut self, snapshot: &RuntimeSnapshot) {
        // Clear refresh indicator once we get a live (non-cached) snapshot
        if self.refresh_requested && !snapshot.from_cache {
            self.refresh_requested = false;
        }
        self.rebuild_sorted_indices(snapshot);
        sync_selection(&mut self.issues_state, self.sorted_issue_indices.len());
        let previous_agent_selection = self.agents_state.selected();
        sync_selection(&mut self.agents_state, snapshot.running.len());
        if self.agents_state.selected() != previous_agent_selection {
            self.agents_detail_scroll = 0;
        }
        sync_selection(&mut self.tasks_state, snapshot.tasks.len());
        let deliverable_count = snapshot
            .movements
            .iter()
            .filter(|m| m.has_deliverable)
            .count();
        sync_selection(&mut self.deliverables_state, deliverable_count);
        let previous_movement_selection = self.movements_state.selected();
        sync_selection(&mut self.movements_state, snapshot.movements.len());
        if self.movements_state.selected() != previous_movement_selection {
            self.movement_detail_scroll = 0;
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

    pub fn selected_agent<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a RunningRow> {
        self.agents_state
            .selected()
            .and_then(|index| snapshot.running.get(index))
    }

    pub fn active_table_len(&self, snapshot: &RuntimeSnapshot) -> usize {
        match self.active_tab {
            ActiveTab::Triggers => self.sorted_issue_indices.len(),
            ActiveTab::Agents => snapshot.running.len(),
            ActiveTab::Orchestrator => snapshot.movements.len(),
            ActiveTab::Tasks => snapshot.tasks.len(),
            ActiveTab::Deliverables => snapshot
                .movements
                .iter()
                .filter(|m| m.has_deliverable)
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
        let lines = self.log_buffer.recent_lines(500);
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
        let previous_agent = matches!(self.active_tab, ActiveTab::Agents)
            .then(|| self.agents_state.selected())
            .flatten();
        let previous_movement = matches!(self.active_tab, ActiveTab::Orchestrator)
            .then(|| self.movements_state.selected())
            .flatten();
        move_selection(self.active_table_state_mut(), len, amount);
        if matches!(self.active_tab, ActiveTab::Agents)
            && self.agents_state.selected() != previous_agent
        {
            self.agents_detail_scroll = 0;
        }
        if matches!(self.active_tab, ActiveTab::Orchestrator)
            && self.movements_state.selected() != previous_movement
        {
            self.movement_detail_scroll = 0;
        }
    }

    pub fn move_up(&mut self, len: usize, amount: usize) {
        let previous_agent = matches!(self.active_tab, ActiveTab::Agents)
            .then(|| self.agents_state.selected())
            .flatten();
        let previous_movement = matches!(self.active_tab, ActiveTab::Orchestrator)
            .then(|| self.movements_state.selected())
            .flatten();
        move_selection_back(self.active_table_state_mut(), len, amount);
        if matches!(self.active_tab, ActiveTab::Agents)
            && self.agents_state.selected() != previous_agent
        {
            self.agents_detail_scroll = 0;
        }
        if matches!(self.active_tab, ActiveTab::Orchestrator)
            && self.movements_state.selected() != previous_movement
        {
            self.movement_detail_scroll = 0;
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
        // Default padding is 1 space on each side; our divider is "  " (2 chars).
        let rel = (column - area.x) as usize;
        let pad_left = 1_usize;
        let pad_right = 1_usize;
        let divider_len = 2_usize;
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
        None => state.select(Some(0)),
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
