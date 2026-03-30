use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Instant,
};

use chrono::{DateTime, Utc};
use polyphony_core::{
    AgentContextSnapshot, AgentRunHistoryRow, InboxItemKind, InboxItemRow, RepoRegistration,
    RunRow, RunningAgentRow, RuntimeSnapshot, TaskRow,
};
use ratatui::{layout::Rect, widgets::TableState};

const RPS_HISTORY_CAP: usize = 120;
/// How many live agent log lines to show under each running agent in the tree.
const MAX_VISIBLE_AGENT_LOG_LINES: usize = 1;
pub(crate) const TAB_DIVIDER: &str = " ";
pub(crate) const TAB_PADDING_LEFT: &str = " ";
pub(crate) const TAB_PADDING_RIGHT: &str = " ";

use crate::{LogBuffer, theme::Theme};

#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub title: String,
    pub description: Option<String>,
    pub created_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct StickyToast {
    pub title: String,
    pub description: Option<String>,
    pub started_at: DateTime<Utc>,
}

/// What to launch when the user presses `c` on an agent.
#[derive(Debug)]
pub(crate) enum CastPlayback {
    /// Replay a finished recording in the browser.
    Replay(std::path::PathBuf),
}

pub(crate) enum SelectedAgentRow<'a> {
    Running(&'a RunningAgentRow),
    History(&'a AgentRunHistoryRow),
}

#[derive(Debug, Clone)]
pub(crate) struct AgentDetailArtifactCache {
    pub key: String,
    pub saved_context: Option<AgentContextSnapshot>,
}

#[derive(Debug, Clone)]
pub(crate) struct DispatchModalState {
    pub item_id: String,
    pub item_identifier: String,
    pub item_title: String,
    pub item_kind: InboxItemKind,
    pub agent_name: Option<String>,
    pub directives: String,
    pub cursor: usize,
}

impl DispatchModalState {
    pub(crate) fn new(
        item_id: String,
        item_identifier: String,
        item_title: String,
        item_kind: InboxItemKind,
        agent_name: Option<String>,
    ) -> Self {
        Self {
            item_id,
            item_identifier,
            item_title,
            item_kind,
            agent_name,
            directives: String::new(),
            cursor: 0,
        }
    }

    pub(crate) fn normalized_directives(&self) -> Option<String> {
        let trimmed = self.directives.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        let index = self.byte_index();
        self.directives.insert(index, ch);
        self.cursor += 1;
    }

    pub(crate) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let remove_at = self.cursor - 1;
        let before = self.directives.chars().take(remove_at);
        let after = self.directives.chars().skip(self.cursor);
        self.directives = before.chain(after).collect();
        self.cursor = remove_at;
    }

    pub(crate) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.directives.chars().count());
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor = line_start(&self.directives, self.cursor);
    }

    pub(crate) fn move_end(&mut self) {
        self.cursor = line_end(&self.directives, self.cursor);
    }

    pub(crate) fn move_up(&mut self) {
        let current_start = line_start(&self.directives, self.cursor);
        if current_start == 0 {
            return;
        }
        let column = self.cursor.saturating_sub(current_start);
        let previous_start = line_start(&self.directives, current_start - 1);
        let previous_end = line_end(&self.directives, previous_start);
        self.cursor = previous_start + column.min(previous_end.saturating_sub(previous_start));
    }

    pub(crate) fn move_down(&mut self) {
        let current_start = line_start(&self.directives, self.cursor);
        let current_end = line_end(&self.directives, self.cursor);
        let Some(next_start) =
            (current_end < self.directives.chars().count()).then_some(current_end + 1)
        else {
            return;
        };
        let column = self.cursor.saturating_sub(current_start);
        let next_end = line_end(&self.directives, next_start);
        self.cursor = next_start + column.min(next_end.saturating_sub(next_start));
    }

    fn byte_index(&self) -> usize {
        self.directives
            .char_indices()
            .map(|(index, _)| index)
            .nth(self.cursor)
            .unwrap_or(self.directives.len())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CreateIssueModalState {
    pub title: String,
    pub description: String,
    pub repo_options: Vec<String>,
    pub selected_repo_idx: Option<usize>,
    /// Which field has focus: 0 = title, 1 = repo, 2 = description
    pub cursor_field: u8,
    /// Character cursor position within the focused field
    pub cursor_pos: usize,
}

impl CreateIssueModalState {
    pub(crate) fn new() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            repo_options: Vec::new(),
            selected_repo_idx: None,
            cursor_field: 0,
            cursor_pos: 0,
        }
    }

    pub(crate) fn set_repo_options(
        &mut self,
        repo_options: Vec<String>,
        selected_repo: Option<String>,
    ) {
        self.repo_options = repo_options;
        self.selected_repo_idx = if self.repo_options.is_empty() {
            None
        } else if let Some(selected_repo) = selected_repo {
            self.repo_options
                .iter()
                .position(|repo_id| repo_id == &selected_repo)
        } else if self.repo_options.len() == 1 {
            Some(0)
        } else {
            None
        };
    }

    pub(crate) fn has_repo_selector(&self) -> bool {
        !self.repo_options.is_empty()
    }

    pub(crate) fn selected_repo_id(&self) -> Option<&str> {
        self.selected_repo_idx
            .and_then(|idx| self.repo_options.get(idx))
            .map(String::as_str)
    }

    pub(crate) fn cycle_repo_next(&mut self) {
        if self.repo_options.is_empty() {
            return;
        }
        self.selected_repo_idx = Some(match self.selected_repo_idx {
            None => 0,
            Some(idx) => (idx + 1) % self.repo_options.len(),
        });
    }

    pub(crate) fn cycle_repo_previous(&mut self) {
        if self.repo_options.is_empty() {
            return;
        }
        self.selected_repo_idx = Some(match self.selected_repo_idx {
            None => self.repo_options.len().saturating_sub(1),
            Some(0) => self.repo_options.len().saturating_sub(1),
            Some(idx) => idx.saturating_sub(1),
        });
    }

    fn repo_field_index(&self) -> u8 {
        if self.has_repo_selector() {
            1
        } else {
            255
        }
    }

    pub(crate) fn description_field_index(&self) -> u8 {
        if self.has_repo_selector() {
            2
        } else {
            1
        }
    }

    pub(crate) fn focused_on_repo(&self) -> bool {
        self.cursor_field == self.repo_field_index()
    }

    pub(crate) fn focused_text(&self) -> &str {
        if self.cursor_field == 0 {
            &self.title
        } else {
            &self.description
        }
    }

    fn focused_text_mut(&mut self) -> &mut String {
        if self.cursor_field == 0 {
            &mut self.title
        } else {
            &mut self.description
        }
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        if self.focused_on_repo() {
            return;
        }
        let pos = self.cursor_pos;
        let text = self.focused_text_mut();
        let index = char_to_byte_index(text, pos);
        text.insert(index, ch);
        self.cursor_pos += 1;
    }

    pub(crate) fn insert_newline(&mut self) {
        if self.cursor_field == self.description_field_index() {
            self.insert_char('\n');
        }
    }

    pub(crate) fn backspace(&mut self) {
        if self.focused_on_repo() {
            return;
        }
        if self.cursor_pos == 0 {
            return;
        }
        let remove_at = self.cursor_pos - 1;
        let cursor = self.cursor_pos;
        let text = self.focused_text_mut();
        let before = text.chars().take(remove_at);
        let after = text.chars().skip(cursor);
        *text = before.chain(after).collect();
        self.cursor_pos = remove_at;
    }

    pub(crate) fn move_left(&mut self) {
        if self.focused_on_repo() {
            self.cycle_repo_previous();
            return;
        }
        self.cursor_pos = self.cursor_pos.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        if self.focused_on_repo() {
            self.cycle_repo_next();
            return;
        }
        let len = self.focused_text().chars().count();
        self.cursor_pos = (self.cursor_pos + 1).min(len);
    }

    pub(crate) fn move_home(&mut self) {
        if self.focused_on_repo() {
            self.selected_repo_idx = if self.repo_options.is_empty() {
                None
            } else {
                Some(0)
            };
            return;
        }
        self.cursor_pos = line_start(self.focused_text(), self.cursor_pos);
    }

    pub(crate) fn move_end(&mut self) {
        if self.focused_on_repo() {
            self.selected_repo_idx = if self.repo_options.is_empty() {
                None
            } else {
                Some(self.repo_options.len().saturating_sub(1))
            };
            return;
        }
        self.cursor_pos = line_end(self.focused_text(), self.cursor_pos);
    }

    pub(crate) fn move_up(&mut self) {
        if self.focused_on_repo() {
            self.cycle_repo_previous();
            return;
        }
        let text = self.focused_text();
        let current_start = line_start(text, self.cursor_pos);
        if current_start == 0 {
            return;
        }
        let column = self.cursor_pos.saturating_sub(current_start);
        let previous_start = line_start(text, current_start - 1);
        let previous_end = line_end(text, previous_start);
        self.cursor_pos = previous_start + column.min(previous_end.saturating_sub(previous_start));
    }

    pub(crate) fn move_down(&mut self) {
        if self.focused_on_repo() {
            self.cycle_repo_next();
            return;
        }
        let text = self.focused_text();
        let current_start = line_start(text, self.cursor_pos);
        let current_end = line_end(text, self.cursor_pos);
        let total = text.chars().count();
        let Some(next_start) = (current_end < total).then_some(current_end + 1) else {
            return;
        };
        let column = self.cursor_pos.saturating_sub(current_start);
        let next_end = line_end(text, next_start);
        self.cursor_pos = next_start + column.min(next_end.saturating_sub(next_start));
    }

    pub(crate) fn toggle_field(&mut self) {
        self.cursor_field = match self.cursor_field {
            0 if self.has_repo_selector() => 1,
            0 => 1,
            1 if self.has_repo_selector() => 2,
            _ => 0,
        };
        if !self.focused_on_repo() {
            let len = self.focused_text().chars().count();
            self.cursor_pos = self.cursor_pos.min(len);
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AddRepoModalState {
    pub source: String,
    pub branch: String,
    /// Which field has focus: 0 = source, 1 = branch
    pub cursor_field: u8,
    /// Character cursor position within the focused field
    pub cursor_pos: usize,
}

impl AddRepoModalState {
    pub(crate) fn new() -> Self {
        Self {
            source: String::new(),
            branch: "main".into(),
            cursor_field: 0,
            cursor_pos: 0,
        }
    }

    pub(crate) fn focused_text(&self) -> &str {
        if self.cursor_field == 0 {
            &self.source
        } else {
            &self.branch
        }
    }

    fn focused_text_mut(&mut self) -> &mut String {
        if self.cursor_field == 0 {
            &mut self.source
        } else {
            &mut self.branch
        }
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        let pos = self.cursor_pos;
        let text = self.focused_text_mut();
        let index = char_to_byte_index(text, pos);
        text.insert(index, ch);
        self.cursor_pos += 1;
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let remove_at = self.cursor_pos - 1;
        let cursor = self.cursor_pos;
        let text = self.focused_text_mut();
        let before = text.chars().take(remove_at);
        let after = text.chars().skip(cursor);
        *text = before.chain(after).collect();
        self.cursor_pos = remove_at;
    }

    pub(crate) fn move_left(&mut self) {
        self.cursor_pos = self.cursor_pos.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        let len = self.focused_text().chars().count();
        self.cursor_pos = (self.cursor_pos + 1).min(len);
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor_pos = line_start(self.focused_text(), self.cursor_pos);
    }

    pub(crate) fn move_end(&mut self) {
        self.cursor_pos = line_end(self.focused_text(), self.cursor_pos);
    }

    pub(crate) fn toggle_field(&mut self) {
        self.cursor_field = if self.cursor_field == 0 {
            1
        } else {
            0
        };
        let len = self.focused_text().chars().count();
        self.cursor_pos = self.cursor_pos.min(len);
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.source.trim().is_empty()
    }

    pub(crate) fn normalized_branch(&self) -> Option<String> {
        let branch = self.branch.trim();
        if branch.is_empty() {
            None
        } else {
            Some(branch.to_string())
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FeedbackModalState {
    pub run_id: String,
    pub run_title: String,
    pub prompt: String,
    pub agent_name: Option<String>,
    pub cursor: usize,
}

impl FeedbackModalState {
    pub(crate) fn new(run_id: String, run_title: String) -> Self {
        Self {
            run_id,
            run_title,
            prompt: String::new(),
            agent_name: None,
            cursor: 0,
        }
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        let index = char_to_byte_index(&self.prompt, self.cursor);
        self.prompt.insert(index, ch);
        self.cursor += 1;
    }

    pub(crate) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let remove_at = self.cursor - 1;
        let before = self.prompt.chars().take(remove_at);
        let after = self.prompt.chars().skip(self.cursor);
        self.prompt = before.chain(after).collect();
        self.cursor = remove_at;
    }

    pub(crate) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.prompt.chars().count());
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor = line_start(&self.prompt, self.cursor);
    }

    pub(crate) fn move_end(&mut self) {
        self.cursor = line_end(&self.prompt, self.cursor);
    }

    pub(crate) fn move_up(&mut self) {
        let current_start = line_start(&self.prompt, self.cursor);
        if current_start == 0 {
            return;
        }
        let column = self.cursor.saturating_sub(current_start);
        let previous_start = line_start(&self.prompt, current_start - 1);
        let previous_end = line_end(&self.prompt, previous_start);
        self.cursor = previous_start + column.min(previous_end.saturating_sub(previous_start));
    }

    pub(crate) fn move_down(&mut self) {
        let current_start = line_start(&self.prompt, self.cursor);
        let current_end = line_end(&self.prompt, self.cursor);
        let Some(next_start) =
            (current_end < self.prompt.chars().count()).then_some(current_end + 1)
        else {
            return;
        };
        let column = self.cursor.saturating_sub(current_start);
        let next_end = line_end(&self.prompt, next_start);
        self.cursor = next_start + column.min(next_end.saturating_sub(next_start));
    }

    pub(crate) fn normalized_prompt(&self) -> Option<String> {
        let trimmed = self.prompt.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

fn char_to_byte_index(text: &str, char_pos: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .nth(char_pos)
        .unwrap_or(text.len())
}

fn line_start(text: &str, cursor: usize) -> usize {
    let mut start = 0;
    for (index, ch) in text.chars().enumerate().take(cursor) {
        if ch == '\n' {
            start = index + 1;
        }
    }
    start
}

fn line_end(text: &str, cursor: usize) -> usize {
    let total = text.chars().count();
    for (index, ch) in text.chars().enumerate().skip(cursor) {
        if ch == '\n' {
            return index;
        }
    }
    total
}

/// Which section of a detail page has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DetailSection {
    /// The main body / description area (scrollable text).
    #[default]
    Body,
    /// A numbered sub-section (e.g., 0 = runs mini-list, 1 = agents mini-list).
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
    InboxItem {
        item_id: String,
        scroll: u16,
        focus: DetailSection,
        runs_selected: usize,
        agents_selected: usize,
    },
    Run {
        run_id: String,
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
        run_id: String,
        scroll: u16,
    },
    Repo {
        repo_id: String,
        scroll: u16,
    },
    /// Full-screen filtered event log for a specific inbox item.
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
        task_id: Option<String>,
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
            Self::InboxItem { scroll, .. }
            | Self::Run { scroll, .. }
            | Self::Task { scroll, .. }
            | Self::Agent { scroll, .. }
            | Self::Deliverable { scroll, .. }
            | Self::Repo { scroll, .. }
            | Self::Events { scroll, .. }
            | Self::LiveLog { scroll, .. } => *scroll,
        }
    }

    pub(crate) fn scroll_mut(&mut self) -> &mut u16 {
        match self {
            Self::InboxItem { scroll, .. }
            | Self::Run { scroll, .. }
            | Self::Task { scroll, .. }
            | Self::Agent { scroll, .. }
            | Self::Deliverable { scroll, .. }
            | Self::Repo { scroll, .. }
            | Self::Events { scroll, .. }
            | Self::LiveLog { scroll, .. } => scroll,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Inbox,
    Orchestrator,
    Tasks,
    Deliverables,
    Agents,
    Repos,
    Logs,
}

impl ActiveTab {
    pub const ALL: [Self; 7] = [
        Self::Inbox,
        Self::Orchestrator,
        Self::Tasks,
        Self::Deliverables,
        Self::Agents,
        Self::Repos,
        Self::Logs,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Orchestrator => "Orchestration",
            Self::Tasks => "Tasks",
            Self::Deliverables => "Outcomes",
            Self::Agents => "Agents",
            Self::Repos => "Repos",
            Self::Logs => "Logs",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Inbox => 0,
            Self::Orchestrator => 1,
            Self::Tasks => 2,
            Self::Deliverables => 3,
            Self::Agents => 4,
            Self::Repos => 5,
            Self::Logs => 6,
        }
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Inbox)
    }

    pub fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }

    pub fn previous(self) -> Self {
        Self::from_index((self.index() + Self::ALL.len() - 1) % Self::ALL.len())
    }
}

/// A row in the Orchestration tab's tree view: run, inbox item, task, or outcome.
#[derive(Debug, Clone)]
pub(crate) enum OrchestratorTreeRow {
    Run {
        snapshot_index: usize,
    },
    InboxItem {
        item_index: usize,
        run_snapshot_index: usize,
        is_last_child: bool,
    },
    /// An agent session (from history) shown under a run.
    AgentSession {
        history_index: usize,
        is_last_child: bool,
    },
    /// Task progress bar shown under a run.
    Progress {
        run_snapshot_index: usize,
        is_last_child: bool,
    },
    /// A currently running agent shown under a run.
    RunningAgent {
        running_index: usize,
        is_last_child: bool,
    },
    /// Live log line from a running agent session.
    AgentLogLine {
        running_index: usize,
        line_index: usize,
        is_last_child: bool,
    },
    Task {
        snapshot_index: usize,
        is_last_child: bool,
    },
    /// Persisted run log entry (reconciliation, pipeline events).
    LogEntry {
        log_index: usize,
        run_snapshot_index: usize,
        is_last_child: bool,
    },
    Outcome {
        run_snapshot_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSortKey {
    Newest,
    Oldest,
    Status,
}

impl RunSortKey {
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
    pub runs_state: TableState,
    pub detail_stack: Vec<DetailView>,
    pub split_focus: SplitFocus,
    pub issue_sort: IssueSortKey,
    pub run_sort: RunSortKey,
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
    pub repos_state: TableState,
    pub logs_state: TableState,
    pub runs_search_active: bool,
    pub runs_search_query: String,
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
    pub dispatch_modal: Option<DispatchModalState>,
    pub create_issue_modal: Option<CreateIssueModalState>,
    pub add_repo_modal: Option<AddRepoModalState>,
    pub feedback_modal: Option<FeedbackModalState>,
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
    /// Sorted run indices for the Orchestration tab.
    pub sorted_run_indices: Vec<usize>,
    pub orchestrator_tree_rows: Vec<OrchestratorTreeRow>,
    /// Run IDs that have expandable children (inbox, sessions, tasks, logs, outcome).
    pub runs_with_children: HashSet<String>,
    pub collapsed_runs: HashSet<String>,
    collapsed_runs_initialized: bool,
    /// Sorted agent indices: each entry is (is_running, original_index).
    /// Sorted by started_at ascending (oldest first, newest at bottom).
    pub sorted_agent_indices: Vec<(bool, usize)>,
    /// When set, the main loop will suspend the TUI and play this agent recording.
    pub pending_cast_playback: Option<CastPlayback>,
    /// Toast notification shown briefly at the bottom of the screen.
    pub toast: Option<Toast>,
    /// Sticky toast driven by runtime state, e.g. pending auth prompts.
    pub sticky_toast: Option<StickyToast>,
    /// Item IDs that have been dispatched but not yet running.
    /// Cleared when the item appears in `snapshot.running`.
    pub dispatching_inbox_items: HashSet<String>,
    /// Previous snapshot's running token counts, keyed by issue_id.
    /// Used to detect token direction (↑ sending / ↓ receiving).
    pub prev_running_tokens: HashMap<String, polyphony_core::TokenUsage>,
    /// When set, filter all views to show only items from this repo.
    /// `None` means show combined view from all repos.
    pub selected_repo: Option<String>,
}

impl AppState {
    pub fn new(theme: Theme, log_buffer: LogBuffer) -> Self {
        Self {
            theme,
            active_tab: ActiveTab::Inbox,
            issues_state: TableState::default(),
            agents_state: TableState::default(),
            tasks_state: TableState::default(),
            deliverables_state: TableState::default(),
            runs_state: TableState::default(),
            detail_stack: Vec::new(),
            split_focus: SplitFocus::default(),
            issue_sort: IssueSortKey::Oldest,
            run_sort: RunSortKey::Oldest,
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
            repos_state: TableState::default(),
            logs_state: TableState::default(),
            runs_search_active: false,
            runs_search_query: String::new(),
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
            dispatch_modal: None,
            create_issue_modal: None,
            add_repo_modal: None,
            feedback_modal: None,
            last_click_at: None,
            last_click_pos: (0, 0),
            last_scroll_at: None,
            events_scroll: 0,
            events_area: Rect::default(),
            sorted_task_indices: Vec::new(),
            sorted_run_indices: Vec::new(),
            orchestrator_tree_rows: Vec::new(),
            runs_with_children: HashSet::new(),
            collapsed_runs: HashSet::new(),
            collapsed_runs_initialized: false,
            sorted_agent_indices: Vec::new(),
            pending_cast_playback: None,
            toast: None,
            sticky_toast: None,
            dispatching_inbox_items: HashSet::new(),
            prev_running_tokens: HashMap::new(),
            selected_repo: None,
        }
    }

    /// Show a toast notification that auto-expires after a few seconds.
    pub fn show_toast(&mut self, title: impl Into<String>, description: Option<String>) {
        self.toast = Some(Toast {
            title: title.into(),
            description,
            created_at: Instant::now(),
        });
    }

    /// Check if an item's repo_id matches the current repo filter.
    /// Returns `true` if no filter is active or the repo_id matches.
    #[allow(dead_code)]
    pub fn matches_repo_filter(&self, repo_id: &str) -> bool {
        match &self.selected_repo {
            None => true,
            Some(filter) => repo_id.is_empty() || repo_id == filter,
        }
    }

    /// Clear expired toasts.
    pub fn expire_toast(&mut self) {
        if let Some(toast) = &self.toast {
            let ttl = std::time::Duration::from_secs(3);
            if toast.created_at.elapsed() > ttl {
                self.toast = None;
            }
        }
    }

    pub fn on_snapshot(&mut self, snapshot: &RuntimeSnapshot) {
        self.sync_sticky_toast(snapshot);
        // Clear refresh indicator once we get a live (non-cached) snapshot
        if self.refresh_requested && !snapshot.from_cache {
            self.refresh_requested = false;
        }
        // Clear dispatching indicators for items that are now running or have runs.
        // Item IDs are synthetic issue IDs for PR events, so compare against
        // run.issue_id instead of run.issue_identifier.
        if !self.dispatching_inbox_items.is_empty() {
            let running_ids: HashSet<&str> = snapshot
                .running
                .iter()
                .map(|r| r.issue_id.as_str())
                .collect();
            let run_identifiers: HashSet<&str> = snapshot
                .runs
                .iter()
                .filter_map(|m| m.issue_identifier.as_deref())
                .collect();
            self.dispatching_inbox_items.retain(|id| {
                if running_ids.contains(id.as_str()) {
                    return false;
                }
                let Some(item) = snapshot.inbox_items.iter().find(|item| item.item_id == *id)
                else {
                    return true;
                };
                !run_identifiers.contains(item.identifier.as_str())
            });
        }
        self.rebuild_sorted_indices(snapshot);
        sync_selection(&mut self.issues_state, self.sorted_issue_indices.len());
        // Rebuild sorted agent indices (oldest first, newest at bottom)
        {
            let mut indices: Vec<(bool, usize)> =
                Vec::with_capacity(snapshot.running.len() + snapshot.agent_run_history.len());
            for i in 0..snapshot.running.len() {
                indices.push((true, i));
            }
            for i in 0..snapshot.agent_run_history.len() {
                indices.push((false, i));
            }
            indices.sort_by_key(|&(is_running, idx)| {
                if is_running {
                    snapshot.running[idx].started_at
                } else {
                    snapshot.agent_run_history[idx].started_at
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
        sync_selection(&mut self.repos_state, snapshot.repo_registrations.len());
        let deliverable_count = snapshot
            .runs
            .iter()
            .filter(|m| m.deliverable.is_some())
            .count();
        sync_selection(&mut self.deliverables_state, deliverable_count);
        // Keep sorted run indices in sync with the snapshot.
        let mut run_indices: Vec<usize> = (0..snapshot.runs.len()).collect();
        match self.run_sort {
            RunSortKey::Oldest => {
                run_indices.sort_by_key(|&i| snapshot.runs[i].created_at);
            },
            RunSortKey::Newest => {
                run_indices.sort_by_key(|&i| std::cmp::Reverse(snapshot.runs[i].created_at));
            },
            RunSortKey::Status => {
                run_indices.sort_by_key(|&i| snapshot.runs[i].status.to_string());
            },
        }
        self.sorted_run_indices = run_indices;
        self.rebuild_orchestrator_tree(snapshot);
        let previous_run_selection = self.runs_state.selected();
        sync_selection(&mut self.runs_state, self.orchestrator_tree_rows.len());
        if self.runs_state.selected() != previous_run_selection
            && let Some(DetailView::Run { scroll, .. }) = self.detail_stack.last_mut()
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
                DetailView::InboxItem { item_id, .. } => {
                    !snapshot.inbox_items.iter().any(|t| t.item_id == *item_id)
                },
                DetailView::Run { run_id, .. } | DetailView::Deliverable { run_id, .. } => {
                    !snapshot.runs.iter().any(|m| m.id == *run_id)
                },
                DetailView::Repo { repo_id, .. } => !snapshot
                    .repo_registrations
                    .iter()
                    .any(|repo| repo.repo_id == *repo_id),
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

        // Save running token counts for direction detection on next snapshot
        self.prev_running_tokens = snapshot
            .running
            .iter()
            .map(|r| (r.issue_id.clone(), r.tokens.clone()))
            .collect();
    }

    fn sync_sticky_toast(&mut self, snapshot: &RuntimeSnapshot) {
        let Some(interaction) = snapshot.pending_user_interactions.last() else {
            self.sticky_toast = None;
            return;
        };
        let extra_count = snapshot.pending_user_interactions.len().saturating_sub(1);
        let description = if extra_count == 0 {
            interaction.description.clone()
        } else {
            Some(match interaction.description.as_deref() {
                Some(description) => format!("{description} ({extra_count} more pending)"),
                None => format!("{extra_count} more interactions pending"),
            })
        };
        self.sticky_toast = Some(StickyToast {
            title: interaction.title.clone(),
            description,
            started_at: interaction.started_at,
        });
    }

    pub fn rebuild_orchestrator_tree(&mut self, snapshot: &RuntimeSnapshot) {
        use std::collections::HashMap as StdMap;

        use polyphony_core::RunStatus;

        // Auto-collapse terminal runs on first load
        if !self.collapsed_runs_initialized && !snapshot.runs.is_empty() {
            for m in &snapshot.runs {
                if matches!(
                    m.status,
                    RunStatus::Delivered | RunStatus::Failed | RunStatus::Cancelled
                ) {
                    self.collapsed_runs.insert(m.id.clone());
                }
            }
            self.collapsed_runs_initialized = true;
        }

        // Group tasks by run_id, sorted by ordinal
        let mut tasks_by_run: StdMap<&str, Vec<usize>> = StdMap::new();
        for (i, task) in snapshot.tasks.iter().enumerate() {
            tasks_by_run.entry(&task.run_id).or_default().push(i);
        }
        for tasks in tasks_by_run.values_mut() {
            tasks.sort_by_key(|&i| snapshot.tasks[i].ordinal);
        }

        // Build inbox item lookup by identifier.
        let item_by_identifier: StdMap<&str, usize> = snapshot
            .inbox_items
            .iter()
            .enumerate()
            .map(|(i, t)| (t.identifier.as_str(), i))
            .collect();

        // Build agent session lookup by issue_id (from history), sorted by started_at
        let mut sessions_by_issue: StdMap<&str, Vec<usize>> = StdMap::new();
        for (i, session) in snapshot.agent_run_history.iter().enumerate() {
            sessions_by_issue
                .entry(&session.issue_id)
                .or_default()
                .push(i);
        }
        for sessions in sessions_by_issue.values_mut() {
            sessions.sort_by_key(|&i| snapshot.agent_run_history[i].started_at);
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
        let mut expandable_runs = HashSet::new();
        for &mov_idx in &self.sorted_run_indices {
            let run = &snapshot.runs[mov_idx];
            rows.push(OrchestratorTreeRow::Run {
                snapshot_index: mov_idx,
            });

            // Determine whether this run has expandable children (needed for
            // the collapse indicator even when collapsed).
            let task_indices = tasks_by_run.get(run.id.as_str());
            let has_tasks = task_indices.is_some_and(|t| !t.is_empty());
            let has_outcome = run.deliverable.is_some()
                || matches!(
                    run.status,
                    polyphony_core::RunStatus::Delivered | polyphony_core::RunStatus::Failed
                );
            let issue_id = run.issue_identifier.as_deref().and_then(|ident| {
                snapshot
                    .inbox_items
                    .iter()
                    .find(|t| t.identifier == ident)
                    .map(|t| t.item_id.as_str())
            });
            let session_indices = issue_id.and_then(|id| sessions_by_issue.get(id));
            let running_indices = issue_id.and_then(|id| running_by_issue.get(id));
            let has_sessions = session_indices.is_some_and(|s| !s.is_empty());
            let has_running = running_indices.is_some_and(|r| !r.is_empty());
            // Collect run activity log entries (last 5) — computed early
            // so that is_last_child calculations below account for them.
            let log_entries: Vec<usize> = {
                let total = run.activity_log.len();
                let start = total.saturating_sub(5);
                (start..total).collect()
            };
            let has_log_entries = !log_entries.is_empty();
            let has_inbox = run
                .issue_identifier
                .as_deref()
                .is_some_and(|id| item_by_identifier.contains_key(id));

            let has_children = has_inbox
                || has_tasks
                || has_outcome
                || has_sessions
                || has_running
                || has_log_entries;
            if has_children {
                expandable_runs.insert(run.id.clone());
            }

            if !self.collapsed_runs.contains(&run.id) {
                // Inbox item row (first child)
                if let Some(identifier) = run.issue_identifier.as_deref()
                    && let Some(&item_idx) = item_by_identifier.get(identifier)
                {
                    rows.push(OrchestratorTreeRow::InboxItem {
                        item_index: item_idx,
                        run_snapshot_index: mov_idx,
                        is_last_child: !has_children,
                    });
                }

                // Progress bar (shown when run has tasks)
                if run.task_count > 0 {
                    rows.push(OrchestratorTreeRow::Progress {
                        run_snapshot_index: mov_idx,
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
                        let session = &snapshot.agent_run_history[idx];
                        if task_agent_names.contains(session.agent_name.as_str()) {
                            execution_sessions.push(idx);
                        } else {
                            planning_sessions.push(idx);
                        }
                    }
                }
                planning_sessions.sort_by_key(|&i| snapshot.agent_run_history[i].started_at);
                execution_sessions.sort_by_key(|&i| snapshot.agent_run_history[i].started_at);

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
                    let sess_start = snapshot.agent_run_history[sess_idx].started_at;
                    // Find the last task whose started_at <= session started_at
                    let mut best_task = None;
                    for (ti, &task_idx) in sorted_tasks.iter().enumerate() {
                        if let Some(task_start) = snapshot.tasks[task_idx].started_at
                            && task_start <= sess_start
                        {
                            best_task = Some(ti);
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
                planning_sessions.sort_by_key(|&i| snapshot.agent_run_history[i].started_at);

                // Count remaining children after planning sessions
                let total_remaining = sorted_tasks.len()
                    + sessions_per_task.iter().map(|s| s.len()).sum::<usize>()
                    + running_indices.map_or(0, |r| r.len())
                    + log_entries.len()
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
                let remaining_after_tasks = running_indices.map_or(0, |r| r.len())
                    + log_entries.len()
                    + usize::from(has_outcome);
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

                // Running agent rows (after tasks, before outcome/log entries)
                if let Some(running_indices) = running_indices {
                    let count = running_indices.len();
                    for (ci, &run_idx) in running_indices.iter().enumerate() {
                        let log_lines = &snapshot.running[run_idx].recent_log;
                        let after_this_running = (ci + 1 < count) || has_outcome || has_log_entries;
                        let is_last_running = ci == count - 1
                            && !has_outcome
                            && !has_log_entries
                            && log_lines.is_empty();
                        rows.push(OrchestratorTreeRow::RunningAgent {
                            running_index: run_idx,
                            is_last_child: is_last_running,
                        });
                        // Emit live log lines under this running agent (last N only)
                        let visible_start =
                            log_lines.len().saturating_sub(MAX_VISIBLE_AGENT_LOG_LINES);
                        let visible_lines: Vec<usize> = (visible_start..log_lines.len()).collect();
                        let visible_count = visible_lines.len();
                        for (vi, &li) in visible_lines.iter().enumerate() {
                            let is_last_line = vi == visible_count - 1 && !after_this_running;
                            rows.push(OrchestratorTreeRow::AgentLogLine {
                                running_index: run_idx,
                                line_index: li,
                                is_last_child: is_last_line,
                            });
                        }
                    }
                }
                // Run activity log entries (before outcome)
                let log_count = log_entries.len();
                for (li, &log_idx) in log_entries.iter().enumerate() {
                    let is_last = li == log_count - 1 && !has_outcome;
                    rows.push(OrchestratorTreeRow::LogEntry {
                        log_index: log_idx,
                        run_snapshot_index: mov_idx,
                        is_last_child: is_last,
                    });
                }
                if has_outcome {
                    rows.push(OrchestratorTreeRow::Outcome {
                        run_snapshot_index: mov_idx,
                    });
                }
            }
        }
        self.orchestrator_tree_rows = rows;
        self.runs_with_children = expandable_runs;
    }

    pub fn toggle_run_collapse(&mut self, run_id: &str) {
        if !self.collapsed_runs.remove(run_id) {
            self.collapsed_runs.insert(run_id.to_string());
        }
    }

    pub fn rebuild_sorted_indices(&mut self, snapshot: &RuntimeSnapshot) {
        let issues = &snapshot.inbox_items;
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
                .filter(|&&idx| children_by_parent.contains_key(issues[idx].item_id.as_str()))
                .map(|&idx| issues[idx].item_id.as_str())
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
                if let Some(kids) = children_by_parent.get(issues[idx].item_id.as_str()) {
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

    /// Get the inbox item at the given display row (sorted).
    pub fn sorted_inbox_item<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
        display_index: usize,
    ) -> Option<&'a InboxItemRow> {
        self.sorted_issue_indices
            .get(display_index)
            .and_then(|&orig| snapshot.inbox_items.get(orig))
    }

    /// Get the currently selected inbox item (in sorted order).
    pub fn selected_inbox_item<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
    ) -> Option<&'a InboxItemRow> {
        self.issues_state
            .selected()
            .and_then(|display_idx| self.sorted_inbox_item(snapshot, display_idx))
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
                .agent_run_history
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
                .agent_run_history
                .get(orig_idx)
                .map(SelectedAgentRow::History)
        }
    }

    pub fn selected_run<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a RunRow> {
        self.runs_state
            .selected()
            .and_then(|idx| self.orchestrator_tree_rows.get(idx))
            .and_then(|row| match row {
                OrchestratorTreeRow::Run { snapshot_index } => snapshot.runs.get(*snapshot_index),
                OrchestratorTreeRow::InboxItem { .. }
                | OrchestratorTreeRow::Progress { .. }
                | OrchestratorTreeRow::AgentSession { .. }
                | OrchestratorTreeRow::RunningAgent { .. }
                | OrchestratorTreeRow::AgentLogLine { .. }
                | OrchestratorTreeRow::Task { .. }
                | OrchestratorTreeRow::LogEntry { .. }
                | OrchestratorTreeRow::Outcome { .. } => None,
            })
    }

    pub fn selected_orchestrator_row(&self) -> Option<&OrchestratorTreeRow> {
        self.runs_state
            .selected()
            .and_then(|idx| self.orchestrator_tree_rows.get(idx))
    }

    pub fn selected_task<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a TaskRow> {
        self.tasks_state
            .selected()
            .and_then(|display_idx| self.sorted_task_indices.get(display_idx))
            .and_then(|&orig_idx| snapshot.tasks.get(orig_idx))
    }

    pub fn selected_deliverable<'a>(&self, snapshot: &'a RuntimeSnapshot) -> Option<&'a RunRow> {
        let selected = self.deliverables_state.selected()?;
        snapshot
            .runs
            .iter()
            .filter(|run| run.deliverable.is_some())
            .nth(selected)
    }

    pub fn selected_repo_registration<'a>(
        &self,
        snapshot: &'a RuntimeSnapshot,
    ) -> Option<&'a RepoRegistration> {
        self.repos_state
            .selected()
            .and_then(|index| snapshot.repo_registrations.get(index))
    }

    pub fn active_table_len(&self, snapshot: &RuntimeSnapshot) -> usize {
        match self.active_tab {
            ActiveTab::Inbox => self.sorted_issue_indices.len(),
            ActiveTab::Agents => self.sorted_agent_indices.len(),
            ActiveTab::Orchestrator => self.orchestrator_tree_rows.len(),
            ActiveTab::Tasks => snapshot.tasks.len(),
            ActiveTab::Deliverables => snapshot
                .runs
                .iter()
                .filter(|m| m.deliverable.is_some())
                .count(),
            ActiveTab::Repos => snapshot.repo_registrations.len(),
            ActiveTab::Logs => self.filtered_log_count(),
        }
    }

    pub fn active_table_state_mut(&mut self) -> &mut TableState {
        match self.active_tab {
            ActiveTab::Inbox => &mut self.issues_state,
            ActiveTab::Agents => &mut self.agents_state,
            ActiveTab::Orchestrator => &mut self.runs_state,
            ActiveTab::Tasks => &mut self.tasks_state,
            ActiveTab::Deliverables => &mut self.deliverables_state,
            ActiveTab::Repos => &mut self.repos_state,
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

fn issue_matches(issue: &InboxItemRow, query: &str) -> bool {
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
