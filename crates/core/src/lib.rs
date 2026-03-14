pub mod file_cache;

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
};

use {
    async_trait::async_trait,
    chrono::{DateTime, Utc},
    serde::{Deserialize, Serialize},
    serde_json::Value,
    thiserror::Error,
    tokio::sync::mpsc,
    uuid::Uuid,
};

pub type IssueId = String;
pub type MovementId = String;
pub type TaskId = String;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid issue: {0}")]
    InvalidIssue(&'static str),
    #[error("adapter error: {0}")]
    Adapter(String),
    #[error("state store error: {0}")]
    Store(String),
    #[error("rate limited: {0:?}")]
    RateLimited(Box<RateLimitSignal>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueAuthor {
    pub id: Option<String>,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub trust_level: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueComment {
    pub id: String,
    pub body: String,
    pub author: Option<IssueAuthor>,
    pub url: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub state: String,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    pub author: Option<IssueAuthor>,
    pub labels: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub blocked_by: Vec<BlockerRef>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Issue {
    pub fn normalized_state(&self) -> String {
        self.state.to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub path: PathBuf,
    pub workspace_key: String,
    pub created_now: bool,
    pub branch_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutKind {
    #[default]
    Directory,
    LinkedWorktree,
    DiscreteClone,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRequest {
    pub issue_identifier: String,
    pub workspace_root: PathBuf,
    pub workspace_path: PathBuf,
    pub workspace_key: String,
    pub branch_name: Option<String>,
    pub checkout_kind: CheckoutKind,
    pub sync_on_reuse: bool,
    pub source_repo_path: Option<PathBuf>,
    pub clone_url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentModel {
    pub id: String,
    pub display_name: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentModelCatalog {
    pub agent_name: String,
    pub provider_kind: String,
    pub fetched_at: DateTime<Utc>,
    pub selected_model: Option<String>,
    pub models: Vec<AgentModel>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentEventKind {
    SessionStarted,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
    Notification,
    UsageUpdated,
    RateLimitsUpdated,
    StartupFailed,
    OtherMessage,
    Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub issue_id: String,
    pub issue_identifier: String,
    pub agent_name: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub kind: AgentEventKind,
    pub at: DateTime<Utc>,
    pub message: Option<String>,
    pub usage: Option<TokenUsage>,
    pub rate_limits: Option<Value>,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrackerKind {
    #[default]
    None,
    Linear,
    Github,
    Mock,
}

impl fmt::Display for TrackerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Variants are single PascalCase words; lowercasing Debug matches serde rename_all.
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AttemptStatus {
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CancelledByReconciliation,
}

impl fmt::Display for AttemptStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MovementStatus {
    Pending,
    Planning,
    InProgress,
    Review,
    Delivered,
    Failed,
    Cancelled,
}

impl fmt::Display for MovementStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Planning => "planning",
            Self::InProgress => "in_progress",
            Self::Review => "review",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    Research,
    Coding,
    Testing,
    Documentation,
    Review,
}

impl fmt::Display for TaskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Research => "research",
            Self::Coding => "coding",
            Self::Testing => "testing",
            Self::Documentation => "documentation",
            Self::Review => "review",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableKind {
    GithubPullRequest,
    GitlabMergeRequest,
    Patch,
}

impl fmt::Display for DeliverableKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::GithubPullRequest => "github_pull_request",
            Self::GitlabMergeRequest => "gitlab_merge_request",
            Self::Patch => "patch",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableStatus {
    Pending,
    Open,
    Merged,
    Closed,
}

impl fmt::Display for DeliverableStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deliverable {
    pub kind: DeliverableKind,
    pub status: DeliverableStatus,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Movement {
    pub id: MovementId,
    pub issue_id: Option<IssueId>,
    pub issue_identifier: Option<String>,
    pub title: String,
    pub status: MovementStatus,
    pub workspace_key: Option<String>,
    pub workspace_path: Option<PathBuf>,
    pub deliverable: Option<Deliverable>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub movement_id: MovementId,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: TaskCategory,
    pub status: TaskStatus,
    pub ordinal: u32,
    pub parent_id: Option<TaskId>,
    pub agent_name: Option<String>,
    pub turns_completed: u32,
    pub tokens: TokenUsage,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlan {
    pub tasks: Vec<PlannedTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTask {
    pub title: String,
    pub category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

impl PlannedTask {
    pub fn to_task(&self, movement_id: &str, ordinal: u32) -> Task {
        let now = Utc::now();
        let category = match self.category.to_ascii_lowercase().as_str() {
            "research" => TaskCategory::Research,
            "coding" => TaskCategory::Coding,
            "testing" => TaskCategory::Testing,
            "documentation" => TaskCategory::Documentation,
            "review" => TaskCategory::Review,
            _ => TaskCategory::Coding,
        };
        Task {
            id: format!("task-{}", Uuid::new_v4()),
            movement_id: movement_id.to_string(),
            title: self.title.clone(),
            description: self.description.clone(),
            category,
            status: TaskStatus::Pending,
            ordinal,
            parent_id: None,
            agent_name: self.agent.clone(),
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovementRow {
    pub id: MovementId,
    pub issue_identifier: Option<String>,
    pub title: String,
    pub status: MovementStatus,
    pub task_count: usize,
    pub tasks_completed: usize,
    pub has_deliverable: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: TaskId,
    pub movement_id: MovementId,
    pub title: String,
    pub category: TaskCategory,
    pub status: TaskStatus,
    pub ordinal: u32,
    pub agent_name: Option<String>,
    pub turns_completed: u32,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub status: AttemptStatus,
    pub turns_completed: u32,
    pub error: Option<String>,
    pub final_issue_state: Option<String>,
}

impl AgentRunResult {
    pub fn succeeded(turns: u32) -> Self {
        Self {
            status: AttemptStatus::Succeeded,
            turns_completed: turns,
            error: None,
            final_issue_state: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: AttemptStatus::Failed,
            turns_completed: 0,
            error: Some(error.into()),
            final_issue_state: None,
        }
    }

    pub fn cancelled(error: impl Into<String>) -> Self {
        Self {
            status: AttemptStatus::CancelledByReconciliation,
            turns_completed: 0,
            error: Some(error.into()),
            final_issue_state: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    Mock,
    AppServer,
    LocalCli,
    OpenAiChat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentInteractionMode {
    #[default]
    OneShot,
    Interactive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromptMode {
    #[default]
    Env,
    Stdin,
    TmuxPaste,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDefinition {
    pub name: String,
    pub kind: String,
    pub transport: AgentTransport,
    pub command: Option<String>,
    pub fallback_agents: Vec<String>,
    pub model: Option<String>,
    pub models: Vec<String>,
    pub models_command: Option<String>,
    pub fetch_models: bool,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: i64,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
    pub use_tmux: bool,
    pub tmux_session_prefix: Option<String>,
    pub interaction_mode: AgentInteractionMode,
    pub prompt_mode: AgentPromptMode,
    pub idle_timeout_ms: u64,
    pub completion_sentinel: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AgentRunSpec {
    pub issue: Issue,
    pub attempt: Option<u32>,
    pub workspace_path: PathBuf,
    pub prompt: String,
    pub max_turns: u32,
    pub agent: AgentDefinition,
    pub prior_context: Option<AgentContextSnapshot>,
}

#[async_trait]
pub trait AgentSession: Send {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, Error>;

    async fn stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

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
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    pub generated_at: DateTime<Utc>,
    pub counts: SnapshotCounts,
    #[serde(default)]
    pub cadence: RuntimeCadence,
    #[serde(default)]
    pub visible_issues: Vec<VisibleIssueRow>,
    pub running: Vec<RunningRow>,
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
    pub from_cache: bool,
    #[serde(default)]
    pub cached_at: Option<DateTime<Utc>>,
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
pub struct PersistedRunRecord {
    pub issue_id: String,
    pub issue_identifier: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub status: AttemptStatus,
    pub attempt: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub details: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreBootstrap {
    pub retrying: HashMap<String, RetryRow>,
    pub throttles: HashMap<String, ThrottleWindow>,
    pub budgets: HashMap<String, BudgetSnapshot>,
    pub saved_contexts: HashMap<String, AgentContextSnapshot>,
    pub recent_events: Vec<RuntimeEvent>,
    #[serde(default)]
    pub movements: HashMap<String, Movement>,
    #[serde(default)]
    pub tasks: HashMap<String, Task>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repository: String,
    pub number: u64,
    pub url: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackInboundMode {
    None,
    Polling,
    Webhook,
    Websocket,
    Cli,
    Mcp,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedbackCapabilities {
    pub supports_outbound: bool,
    pub supports_links: bool,
    pub supports_interactive: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackChannelKind {
    Telegram,
    Webhook,
}

impl fmt::Display for FeedbackChannelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackChannelDescriptor {
    pub kind: FeedbackChannelKind,
    pub inbound_mode: FeedbackInboundMode,
    pub capabilities: FeedbackCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackAction {
    pub id: String,
    pub label: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackNotification {
    pub key: String,
    pub title: String,
    pub body: String,
    pub links: Vec<FeedbackLink>,
    pub actions: Vec<FeedbackAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueAssignment {
    pub issue_id: String,
    pub issue_identifier: String,
    pub pull_request: Option<PullRequestRef>,
}

#[async_trait]
pub trait IssueTracker: Send + Sync {
    fn component_key(&self) -> String;
    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, Error>;
    async fn fetch_issues_by_states(
        &self,
        project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, Error>;
    async fn fetch_issues_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>, Error>;
    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, Error>;
    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, Error> {
        Ok(None)
    }
    async fn ensure_issue_workflow_tracking(&self, _issue: &Issue) -> Result<(), Error> {
        Ok(())
    }
    async fn update_issue_workflow_status(
        &self,
        _issue: &Issue,
        _status: &str,
    ) -> Result<(), Error> {
        Ok(())
    }
}

#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn component_key(&self) -> String;

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, Error> {
        Ok(None)
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, Error>;
    async fn fetch_budgets(
        &self,
        _agents: &[AgentDefinition],
    ) -> Result<Vec<BudgetSnapshot>, Error> {
        Ok(Vec::new())
    }
    async fn discover_models(
        &self,
        _agents: &[AgentDefinition],
    ) -> Result<Vec<AgentModelCatalog>, Error> {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait AgentProviderRuntime: Send + Sync {
    fn runtime_key(&self) -> String;
    fn supports(&self, agent: &AgentDefinition) -> bool;

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, Error> {
        Ok(None)
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, Error>;
    async fn fetch_budget(
        &self,
        _agent: &AgentDefinition,
    ) -> Result<Option<BudgetSnapshot>, Error> {
        Ok(None)
    }
    async fn discover_models(
        &self,
        _agent: &AgentDefinition,
    ) -> Result<Option<AgentModelCatalog>, Error> {
        Ok(None)
    }
}

#[async_trait]
pub trait WorkspaceProvisioner: Send + Sync {
    fn component_key(&self) -> String;
    async fn ensure_workspace(&self, request: WorkspaceRequest) -> Result<Workspace, Error>;
    async fn cleanup_workspace(&self, request: WorkspaceRequest) -> Result<(), Error>;
}

#[async_trait]
pub trait PullRequestCommenter: Send + Sync {
    fn component_key(&self) -> String;
    async fn comment_on_pull_request(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
    ) -> Result<(), Error>;
}

#[async_trait]
pub trait PullRequestManager: Send + Sync {
    fn component_key(&self) -> String;
    async fn ensure_pull_request(
        &self,
        request: &PullRequestRequest,
    ) -> Result<PullRequestRef, Error>;
    async fn merge_pull_request(&self, pull_request: &PullRequestRef) -> Result<(), Error>;
}

#[async_trait]
pub trait WorkspaceCommitter: Send + Sync {
    fn component_key(&self) -> String;
    async fn commit_and_push(
        &self,
        request: &WorkspaceCommitRequest,
    ) -> Result<Option<WorkspaceCommitResult>, Error>;
}

#[async_trait]
pub trait FeedbackSink: Send + Sync {
    fn component_key(&self) -> String;
    fn descriptor(&self) -> FeedbackChannelDescriptor;
    async fn send(&self, notification: &FeedbackNotification) -> Result<(), Error>;
}

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn bootstrap(&self) -> Result<StoreBootstrap, Error>;
    async fn save_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), Error>;
    async fn record_run(&self, run: &PersistedRunRecord) -> Result<(), Error>;
    async fn record_budget(&self, snapshot: &BudgetSnapshot) -> Result<(), Error>;

    async fn save_movement(&self, _movement: &Movement) -> Result<(), Error> {
        Ok(())
    }
    async fn save_task(&self, _task: &Task) -> Result<(), Error> {
        Ok(())
    }
    async fn load_movements(&self) -> Result<Vec<Movement>, Error> {
        Ok(Vec::new())
    }
    async fn load_tasks_for_movement(&self, _movement_id: &str) -> Result<Vec<Task>, Error> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachedSnapshot {
    pub saved_at: Option<DateTime<Utc>>,
    pub visible_issues: Vec<VisibleIssueRow>,
    pub budgets: Vec<BudgetSnapshot>,
    pub agent_catalogs: Vec<AgentModelCatalog>,
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
