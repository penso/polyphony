use std::time::Instant;

use {
    polyphony_core::{RuntimeSnapshot, VisibleIssueRow},
    ratatui::widgets::TableState,
};

use crate::{LogBuffer, theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Issues,
    Orchestrator,
    Tasks,
    Deliverables,
    Logs,
}

impl ActiveTab {
    pub const ALL: [Self; 5] = [
        Self::Issues,
        Self::Orchestrator,
        Self::Tasks,
        Self::Deliverables,
        Self::Logs,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Issues => "Issues",
            Self::Orchestrator => "Orchestrator",
            Self::Tasks => "Tasks",
            Self::Deliverables => "PR/MR",
            Self::Logs => "Logs",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Issues => 0,
            Self::Orchestrator => 1,
            Self::Tasks => 2,
            Self::Deliverables => 3,
            Self::Logs => 4,
        }
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Issues)
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
    pub tasks_state: TableState,
    pub deliverables_state: TableState,
    pub movements_state: TableState,
    pub show_issue_detail: bool,
    pub detail_scroll: u16,
    pub issue_sort: IssueSortKey,
    /// Sorted index mapping: sorted_issues[display_index] = original snapshot index
    pub sorted_issue_indices: Vec<usize>,
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
}

impl AppState {
    pub fn new(theme: Theme, log_buffer: LogBuffer) -> Self {
        Self {
            theme,
            active_tab: ActiveTab::Issues,
            issues_state: TableState::default(),
            tasks_state: TableState::default(),
            deliverables_state: TableState::default(),
            movements_state: TableState::default(),
            show_issue_detail: false,
            detail_scroll: 0,
            issue_sort: IssueSortKey::Newest,
            sorted_issue_indices: Vec::new(),
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
        }
    }

    pub fn on_snapshot(&mut self, snapshot: &RuntimeSnapshot) {
        // Clear refresh indicator once we get a live (non-cached) snapshot
        if self.refresh_requested && !snapshot.from_cache {
            self.refresh_requested = false;
        }
        self.rebuild_sorted_indices(snapshot);
        sync_selection(&mut self.issues_state, self.sorted_issue_indices.len());
        sync_selection(&mut self.tasks_state, snapshot.tasks.len());
        let deliverable_count = snapshot
            .movements
            .iter()
            .filter(|m| m.has_deliverable)
            .count();
        sync_selection(&mut self.deliverables_state, deliverable_count);
        sync_selection(&mut self.movements_state, snapshot.movements.len());

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
    }

    pub fn rebuild_sorted_indices(&mut self, snapshot: &RuntimeSnapshot) {
        let issues = &snapshot.visible_issues;
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
                    let ta = issues[a].updated_at.as_ref();
                    let tb = issues[b].updated_at.as_ref();
                    match (ta, tb) {
                        (Some(a_t), Some(b_t)) => b_t.cmp(a_t),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => {
                            let na = extract_issue_number(&issues[a].issue_identifier);
                            let nb = extract_issue_number(&issues[b].issue_identifier);
                            nb.cmp(&na)
                        },
                    }
                });
            },
            IssueSortKey::Oldest => {
                indices.sort_by(|&a, &b| {
                    let ta = issues[a].updated_at.as_ref();
                    let tb = issues[b].updated_at.as_ref();
                    match (ta, tb) {
                        (Some(a_t), Some(b_t)) => a_t.cmp(b_t),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => {
                            let na = extract_issue_number(&issues[a].issue_identifier);
                            let nb = extract_issue_number(&issues[b].issue_identifier);
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
                indices.sort_by(|&a, &b| issues[a].state.cmp(&issues[b].state));
            },
        }
        self.sorted_issue_indices = indices;
    }

    /// Get the issue at the given display row (sorted).
    pub fn sorted_issue<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
        display_index: usize,
    ) -> Option<&'a VisibleIssueRow> {
        self.sorted_issue_indices
            .get(display_index)
            .and_then(|&orig| snapshot.visible_issues.get(orig))
    }

    /// Get the currently selected issue (in sorted order).
    pub fn selected_issue<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
    ) -> Option<&'a VisibleIssueRow> {
        self.issues_state
            .selected()
            .and_then(|display_idx| self.sorted_issue(snapshot, display_idx))
    }

    pub fn active_table_len(&self, snapshot: &RuntimeSnapshot) -> usize {
        match self.active_tab {
            ActiveTab::Issues => self.sorted_issue_indices.len(),
            ActiveTab::Orchestrator => snapshot.movements.len(),
            ActiveTab::Tasks => snapshot.tasks.len(),
            ActiveTab::Deliverables => snapshot
                .movements
                .iter()
                .filter(|m| m.has_deliverable)
                .count(),
            ActiveTab::Logs => 0,
        }
    }

    pub fn active_table_state_mut(&mut self) -> &mut TableState {
        match self.active_tab {
            ActiveTab::Issues => &mut self.issues_state,
            ActiveTab::Orchestrator => &mut self.movements_state,
            ActiveTab::Tasks => &mut self.tasks_state,
            ActiveTab::Deliverables | ActiveTab::Logs => &mut self.deliverables_state,
        }
    }

    pub fn move_down(&mut self, len: usize, amount: usize) {
        move_selection(self.active_table_state_mut(), len, amount);
    }

    pub fn move_up(&mut self, len: usize, amount: usize) {
        move_selection_back(self.active_table_state_mut(), len, amount);
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

fn issue_matches(issue: &VisibleIssueRow, query: &str) -> bool {
    issue.title.to_lowercase().contains(query)
        || issue.issue_identifier.to_lowercase().contains(query)
        || issue.state.to_lowercase().contains(query)
        || issue.labels.iter().any(|l| l.to_lowercase().contains(query))
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
