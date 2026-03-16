use crate::{prelude::*, *};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningRow {
    pub issue_id: String,
    pub issue_identifier: String,
    pub agent_name: String,
    pub model: Option<String>,
    pub state: String,
    pub max_turns: u32,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub tokens: TokenUsage,
    pub workspace_path: PathBuf,
    pub attempt: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryRow {
    pub issue_id: String,
    pub issue_identifier: String,
    pub attempt: u32,
    pub due_at: DateTime<Utc>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventScope {
    Workflow,
    Throttle,
    Dispatch,
    Handoff,
    Agent,
    Retry,
    Worker,
    Reconcile,
    Tracker,
    Startup,
    Feedback,
}

impl fmt::Display for EventScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub at: DateTime<Utc>,
    pub scope: EventScope,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeCadence {
    pub tracker_poll_interval_ms: u64,
    pub budget_poll_interval_ms: u64,
    pub model_discovery_interval_ms: u64,
    pub last_tracker_poll_at: Option<DateTime<Utc>>,
    pub last_budget_poll_at: Option<DateTime<Utc>>,
    pub last_model_discovery_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisibleIssueRow {
    pub issue_id: String,
    pub issue_identifier: String,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub approval_state: IssueApprovalState,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub has_workspace: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VisibleTriggerKind {
    #[default]
    Issue,
    PullRequestReview,
    PullRequestComment,
    PullRequestConflict,
}

impl fmt::Display for VisibleTriggerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Issue => "issue",
            Self::PullRequestReview => "pr_review",
            Self::PullRequestComment => "pr_comment",
            Self::PullRequestConflict => "pr_conflict",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisibleTriggerRow {
    pub trigger_id: String,
    pub kind: VisibleTriggerKind,
    pub source: String,
    pub identifier: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub approval_state: IssueApprovalState,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub has_workspace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    pub generated_at: DateTime<Utc>,
    pub counts: SnapshotCounts,
    #[serde(default)]
    pub cadence: RuntimeCadence,
    #[serde(default)]
    pub visible_issues: Vec<VisibleIssueRow>,
    #[serde(default)]
    pub visible_triggers: Vec<VisibleTriggerRow>,
    #[serde(default)]
    pub approved_issue_keys: Vec<String>,
    pub running: Vec<RunningRow>,
    #[serde(default)]
    pub agent_history: Vec<AgentHistoryRow>,
    pub retrying: Vec<RetryRow>,
    pub codex_totals: CodexTotals,
    pub rate_limits: Option<Value>,
    pub throttles: Vec<ThrottleWindow>,
    pub budgets: Vec<BudgetSnapshot>,
    pub agent_catalogs: Vec<AgentModelCatalog>,
    pub saved_contexts: Vec<AgentContextSnapshot>,
    pub recent_events: Vec<RuntimeEvent>,
    #[serde(default)]
    pub movements: Vec<MovementRow>,
    #[serde(default)]
    pub tasks: Vec<TaskRow>,
    #[serde(default)]
    pub loading: LoadingState,
    #[serde(default)]
    pub dispatch_mode: DispatchMode,
    #[serde(default)]
    pub tracker_kind: TrackerKind,
    #[serde(default)]
    pub tracker_connection: Option<TrackerConnectionStatus>,
    #[serde(default)]
    pub from_cache: bool,
    #[serde(default)]
    pub cached_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub agent_profile_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotCounts {
    pub running: usize,
    pub retrying: usize,
    #[serde(default)]
    pub movements: usize,
    #[serde(default)]
    pub tasks_pending: usize,
    #[serde(default)]
    pub tasks_in_progress: usize,
    #[serde(default)]
    pub tasks_completed: usize,
    #[serde(default)]
    pub worktrees: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackerConnectionState {
    #[default]
    Unknown,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TrackerConnectionStatus {
    pub state: TrackerConnectionState,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

impl TrackerConnectionStatus {
    #[must_use]
    pub fn connected(label: impl Into<String>) -> Self {
        Self {
            state: TrackerConnectionState::Connected,
            label: Some(label.into()),
            detail: None,
        }
    }

    #[must_use]
    pub fn disconnected(detail: impl Into<String>) -> Self {
        Self {
            state: TrackerConnectionState::Disconnected,
            label: None,
            detail: Some(detail.into()),
        }
    }

    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerQuery {
    pub project_slug: Option<String>,
    pub repository: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueStateUpdate {
    pub id: String,
    pub identifier: String,
    pub state: String,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHistoryRow {
    pub issue_id: String,
    pub issue_identifier: String,
    pub agent_name: String,
    pub model: Option<String>,
    pub status: AttemptStatus,
    pub attempt: Option<u32>,
    pub max_turns: u32,
    pub turn_count: u32,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub tokens: TokenUsage,
    #[serde(default)]
    pub workspace_path: Option<PathBuf>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub saved_context: Option<AgentContextSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedRunRecord {
    pub issue_id: String,
    pub issue_identifier: String,
    pub agent_name: String,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub status: AttemptStatus,
    pub attempt: Option<u32>,
    pub max_turns: u32,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub tokens: TokenUsage,
    #[serde(default)]
    pub workspace_path: Option<PathBuf>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub saved_context: Option<AgentContextSnapshot>,
}

impl PersistedRunRecord {
    #[must_use]
    pub fn to_history_row(&self) -> AgentHistoryRow {
        AgentHistoryRow {
            issue_id: self.issue_id.clone(),
            issue_identifier: self.issue_identifier.clone(),
            agent_name: self.agent_name.clone(),
            model: self.model.clone(),
            status: self.status,
            attempt: self.attempt,
            max_turns: self.max_turns,
            turn_count: self.turn_count,
            session_id: self.session_id.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: self.turn_id.clone(),
            codex_app_server_pid: self.codex_app_server_pid.clone(),
            last_event: self.last_event.clone(),
            last_message: self.last_message.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            last_event_at: self.last_event_at,
            tokens: self.tokens.clone(),
            workspace_path: self.workspace_path.clone(),
            error: self.error.clone(),
            saved_context: self.saved_context.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreBootstrap {
    #[serde(default)]
    pub snapshot: Option<RuntimeSnapshot>,
    pub retrying: HashMap<String, RetryRow>,
    pub throttles: HashMap<String, ThrottleWindow>,
    pub budgets: HashMap<String, BudgetSnapshot>,
    pub saved_contexts: HashMap<String, AgentContextSnapshot>,
    pub recent_events: Vec<RuntimeEvent>,
    #[serde(default)]
    pub movements: HashMap<String, Movement>,
    #[serde(default)]
    pub tasks: HashMap<String, Task>,
    #[serde(default)]
    pub reviewed_pull_request_heads: HashMap<String, ReviewedPullRequestHead>,
    #[serde(default)]
    pub run_history: Vec<PersistedRunRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContextEntry {
    pub at: DateTime<Utc>,
    pub kind: AgentEventKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContextSnapshot {
    pub issue_id: String,
    pub issue_identifier: String,
    pub updated_at: DateTime<Utc>,
    pub agent_name: String,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub status: Option<AttemptStatus>,
    pub error: Option<String>,
    pub usage: TokenUsage,
    pub transcript: Vec<AgentContextEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitSignal {
    pub component: String,
    pub reason: String,
    pub limited_at: DateTime<Utc>,
    pub retry_after_ms: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub status_code: Option<u16>,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThrottleWindow {
    pub component: String,
    pub until: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub component: String,
    pub captured_at: DateTime<Utc>,
    pub credits_remaining: Option<f64>,
    pub credits_total: Option<f64>,
    pub spent_usd: Option<f64>,
    pub soft_limit_usd: Option<f64>,
    pub hard_limit_usd: Option<f64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub raw: Option<Value>,
}

impl BudgetSnapshot {
    #[must_use]
    pub fn has_credit_headroom(&self) -> bool {
        self.credits_remaining
            .is_some_and(|remaining| remaining > 0.0)
            || self
                .hard_limit_usd
                .zip(self.spent_usd)
                .is_some_and(|(limit, spent)| limit > spent)
            || self
                .soft_limit_usd
                .zip(self.spent_usd)
                .is_some_and(|(limit, spent)| limit > spent)
            || self
                .raw
                .as_ref()
                .and_then(Self::raw_credit_headroom)
                .is_some_and(|remaining| remaining > 0.0)
    }

    #[must_use]
    pub fn has_weekly_credit_deficit(&self) -> bool {
        let Some(raw) = self.raw.as_ref() else {
            return false;
        };

        Self::raw_bool(raw, &["weekly_deficit"])
            .or_else(|| Self::raw_bool(raw, &["has_weekly_deficit"]))
            .or_else(|| Self::raw_bool(raw, &["weekly", "deficit"]))
            .or_else(|| Self::raw_bool(raw, &["weekly", "has_deficit"]))
            .unwrap_or(false)
            || Self::raw_number(raw, &["weekly_deficit"])
                .or_else(|| Self::raw_number(raw, &["weekly_credit_deficit"]))
                .or_else(|| Self::raw_number(raw, &["weekly", "deficit"]))
                .or_else(|| Self::raw_number(raw, &["weekly", "credit_deficit"]))
                .is_some_and(|deficit| deficit > 0.0)
            || Self::raw_number(raw, &["weekly_remaining"])
                .or_else(|| Self::raw_number(raw, &["weekly_credits_remaining"]))
                .or_else(|| Self::raw_number(raw, &["weekly", "remaining"]))
                .or_else(|| Self::raw_number(raw, &["weekly", "credits_remaining"]))
                .is_some_and(|remaining| remaining < 0.0)
    }

    fn raw_credit_headroom(raw: &Value) -> Option<f64> {
        Self::raw_number(raw, &["credits_remaining"])
            .or_else(|| Self::raw_number(raw, &["remaining_credits"]))
            .or_else(|| Self::raw_number(raw, &["credits_leftover"]))
            .or_else(|| Self::raw_number(raw, &["leftover_credits"]))
            .or_else(|| Self::raw_number(raw, &["credits", "remaining"]))
    }

    fn raw_number(raw: &Value, path: &[&str]) -> Option<f64> {
        let mut current = raw;
        for segment in path {
            current = current.get(*segment)?;
        }
        current.as_f64()
    }

    fn raw_bool(raw: &Value, path: &[&str]) -> Option<bool> {
        let mut current = raw;
        for segment in path {
            current = current.get(*segment)?;
        }
        current.as_bool()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repository: String,
    pub number: u64,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestReviewComment {
    pub path: String,
    pub line: u32,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRequest {
    pub repository: String,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCommitRequest {
    pub workspace_path: PathBuf,
    pub branch_name: String,
    pub base_branch: Option<String>,
    pub commit_message: String,
    pub remote_name: String,
    pub auth_token: Option<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCommitResult {
    pub branch_name: String,
    pub head_sha: String,
    pub changed_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedSnapshot {
    pub saved_at: Option<DateTime<Utc>>,
    pub visible_issues: Vec<VisibleIssueRow>,
    #[serde(default)]
    pub visible_triggers: Vec<VisibleTriggerRow>,
    #[serde(default)]
    pub approved_issue_keys: Vec<String>,
    pub budgets: Vec<BudgetSnapshot>,
    pub agent_catalogs: Vec<AgentModelCatalog>,
    #[serde(default)]
    pub tracker_connection: Option<TrackerConnectionStatus>,
}

#[async_trait]
pub trait NetworkCache: Send + Sync {
    async fn load(&self) -> Result<CachedSnapshot, Error>;
    async fn save(&self, snapshot: &CachedSnapshot) -> Result<(), Error>;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoadingState {
    pub fetching_issues: bool,
    pub fetching_budgets: bool,
    pub fetching_models: bool,
    pub reconciling: bool,
}

impl LoadingState {
    pub fn any_active(&self) -> bool {
        self.fetching_issues || self.fetching_budgets || self.fetching_models || self.reconciling
    }
}

pub fn new_movement_id() -> MovementId {
    format!("mov-{}", Uuid::new_v4())
}

pub fn sanitize_workspace_key(identifier: &str) -> String {
    identifier
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect()
}
