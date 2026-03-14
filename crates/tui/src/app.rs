use std::time::Instant;

use {
    polyphony_core::{RuntimeSnapshot, VisibleIssueRow},
    ratatui::widgets::TableState,
};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Issues,
    Orchestrator,
    Tasks,
    Deliverables,
}

impl ActiveTab {
    pub const ALL: [Self; 4] = [Self::Issues, Self::Orchestrator, Self::Tasks, Self::Deliverables];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Issues => "Issues",
            Self::Orchestrator => "Orchestrator",
            Self::Tasks => "Tasks",
            Self::Deliverables => "PR/MR",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Issues => 0,
            Self::Orchestrator => 1,
            Self::Tasks => 2,
            Self::Deliverables => 3,
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
    pub issue_sort: IssueSortKey,
    /// Sorted index mapping: sorted_issues[display_index] = original snapshot index
    pub sorted_issue_indices: Vec<usize>,
    pub frame_count: u64,
    pub leaving: bool,
    pub leaving_since: Option<Instant>,
}

impl AppState {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            active_tab: ActiveTab::Issues,
            issues_state: TableState::default(),
            tasks_state: TableState::default(),
            deliverables_state: TableState::default(),
            movements_state: TableState::default(),
            show_issue_detail: false,
            issue_sort: IssueSortKey::Newest,
            sorted_issue_indices: Vec::new(),
            frame_count: 0,
            leaving: false,
            leaving_since: None,
        }
    }

    pub fn on_snapshot(&mut self, snapshot: &RuntimeSnapshot) {
        self.rebuild_sorted_indices(snapshot);
        sync_selection(&mut self.issues_state, snapshot.visible_issues.len());
        sync_selection(&mut self.tasks_state, snapshot.tasks.len());
        let deliverable_count = snapshot
            .movements
            .iter()
            .filter(|m| m.has_deliverable)
            .count();
        sync_selection(&mut self.deliverables_state, deliverable_count);
        sync_selection(&mut self.movements_state, snapshot.movements.len());
    }

    pub fn rebuild_sorted_indices(&mut self, snapshot: &RuntimeSnapshot) {
        let issues = &snapshot.visible_issues;
        let mut indices: Vec<usize> = (0..issues.len()).collect();
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
            ActiveTab::Issues => snapshot.visible_issues.len(),
            ActiveTab::Orchestrator => snapshot.movements.len(),
            ActiveTab::Tasks => snapshot.tasks.len(),
            ActiveTab::Deliverables => snapshot
                .movements
                .iter()
                .filter(|m| m.has_deliverable)
                .count(),
        }
    }

    pub fn active_table_state_mut(&mut self) -> &mut TableState {
        match self.active_tab {
            ActiveTab::Issues => &mut self.issues_state,
            ActiveTab::Orchestrator => &mut self.movements_state,
            ActiveTab::Tasks => &mut self.tasks_state,
            ActiveTab::Deliverables => &mut self.deliverables_state,
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
