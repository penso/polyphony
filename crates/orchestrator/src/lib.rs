use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime},
};

use chrono::{DateTime, Utc};
use polyphony_core::{
    AgentContextSnapshot, AgentEvent, AgentModelCatalog, AgentRunResult, AgentRuntime,
    BudgetSnapshot, CodexTotals, Error as CoreError, Issue, IssueTracker, LoadingState, Movement,
    MovementId, NetworkCache, PersistedRunRecord, PullRequestCommentTrigger, PullRequestCommenter,
    PullRequestConflictTrigger, PullRequestManager, PullRequestReviewTrigger, PullRequestTrigger,
    PullRequestTriggerSource, RateLimitSignal, RetryRow, ReviewTarget, ReviewedPullRequestHead,
    RuntimeEvent, RuntimeSnapshot, StateStore, Task, TaskId, ThrottleWindow, TokenUsage,
    TrackerConnectionStatus, UserInteractionRequest, VisibleIssueRow, VisibleTriggerRow,
    WorkspaceCommitter, WorkspaceProgressUpdate, WorkspaceProvisioner,
};
use polyphony_feedback::FeedbackRegistry;
use polyphony_workflow::LoadedWorkflow;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("workflow error: {0}")]
    Workflow(#[from] polyphony_workflow::Error),
    #[error("core error: {0}")]
    Core(#[from] polyphony_core::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),
    #[error("workspace error: {0}")]
    Workspace(#[from] polyphony_workspace::Error),
}

const DEFAULT_AUTOMATION_COMMIT_MESSAGE: &str = "fix({{ issue.identifier }}): {{ issue.title }}";
const DEFAULT_AUTOMATION_PR_TITLE: &str = "{{ issue.identifier }}: {{ issue.title }}";
const DEFAULT_AUTOMATION_PR_BODY: &str = include_str!("prompts/automation_pr_body.md");
const DEFAULT_AUTOMATION_REVIEW_PROMPT: &str = include_str!("prompts/automation_review.md");
const DEFAULT_PULL_REQUEST_REVIEW_PROMPT: &str = include_str!("prompts/pull_request_review.md");
const DEFAULT_PULL_REQUEST_COMMENT_REVIEW_PROMPT: &str =
    include_str!("prompts/pull_request_comment_review.md");
const DEFAULT_PLANNER_PROMPT: &str = include_str!("prompts/planner.md");

#[derive(Debug, Deserialize)]
struct GithubViewerIdentity {
    login: String,
}

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    Refresh,
    Shutdown,
    SetMode(polyphony_core::DispatchMode),
    ApproveIssueTrigger {
        issue_id: polyphony_core::IssueId,
        source: String,
    },
    CloseIssueTrigger {
        issue_id: polyphony_core::IssueId,
    },
    ResolveMovementDeliverable {
        movement_id: polyphony_core::MovementId,
        decision: polyphony_core::DeliverableDecision,
    },
    DispatchIssue {
        issue_id: polyphony_core::IssueId,
        agent_name: Option<String>,
        directives: Option<String>,
    },
    DispatchPullRequestTrigger {
        trigger_id: String,
        directives: Option<String>,
    },
    MergeDeliverable {
        movement_id: polyphony_core::MovementId,
    },
    RetryMovement {
        movement_id: polyphony_core::MovementId,
    },
    /// Mark a pipeline task as completed (manual override) and resume the pipeline.
    ResolveTask {
        movement_id: polyphony_core::MovementId,
        task_id: polyphony_core::TaskId,
    },
    /// Re-run a failed pipeline task (reset to Pending and dispatch again).
    RetryTask {
        movement_id: polyphony_core::MovementId,
        task_id: polyphony_core::TaskId,
    },
    /// Stop a running agent by issue ID (user-initiated).
    StopAgent {
        issue_id: polyphony_core::IssueId,
    },
}

#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    pub snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    pub command_tx: mpsc::UnboundedSender<RuntimeCommand>,
}

pub struct RuntimeComponents {
    pub tracker: Arc<dyn IssueTracker>,
    pub pull_request_trigger_source: Option<Arc<dyn PullRequestTriggerSource>>,
    pub agent: Arc<dyn AgentRuntime>,
    pub committer: Option<Arc<dyn WorkspaceCommitter>>,
    pub pull_request_manager: Option<Arc<dyn PullRequestManager>>,
    pub pull_request_commenter: Option<Arc<dyn PullRequestCommenter>>,
    pub feedback: Option<Arc<FeedbackRegistry>>,
}

pub type RuntimeComponentFactory =
    dyn Fn(&LoadedWorkflow) -> Result<RuntimeComponents, CoreError> + Send + Sync;

pub struct RuntimeService {
    tracker: Arc<dyn IssueTracker>,
    pull_request_trigger_source: Option<Arc<dyn PullRequestTriggerSource>>,
    agent: Arc<dyn AgentRuntime>,
    provisioner: Arc<dyn WorkspaceProvisioner>,
    committer: Option<Arc<dyn WorkspaceCommitter>>,
    pull_request_manager: Option<Arc<dyn PullRequestManager>>,
    pull_request_commenter: Option<Arc<dyn PullRequestCommenter>>,
    feedback: Option<Arc<FeedbackRegistry>>,
    store: Option<Arc<dyn StateStore>>,
    cache: Option<Arc<dyn NetworkCache>>,
    workflow_rx: watch::Receiver<LoadedWorkflow>,
    snapshot_tx: watch::Sender<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
    command_rx: mpsc::UnboundedReceiver<OrchestratorMessage>,
    external_command_rx: mpsc::UnboundedReceiver<RuntimeCommand>,
    pending_refresh: bool,
    pending_issue_approvals: Vec<(polyphony_core::IssueId, String)>,
    pending_issue_closures: Vec<polyphony_core::IssueId>,
    pending_deliverable_resolutions: Vec<(
        polyphony_core::MovementId,
        polyphony_core::DeliverableDecision,
    )>,
    pending_manual_dispatches: Vec<ManualDispatchRequest>,
    pending_manual_pull_request_trigger_dispatches: Vec<ManualPullRequestDispatchRequest>,
    pending_merge_deliverables: Vec<polyphony_core::MovementId>,
    pending_movement_retries: Vec<polyphony_core::MovementId>,
    pending_task_resolutions: Vec<(polyphony_core::MovementId, polyphony_core::TaskId)>,
    pending_task_retries: Vec<(polyphony_core::MovementId, polyphony_core::TaskId)>,
    pending_agent_stops: Vec<polyphony_core::IssueId>,
    state: RuntimeState,
    user_interactions: Arc<Mutex<HashMap<String, UserInteractionRequest>>>,
    reload_support: Option<WorkflowReloadSupport>,
}

#[derive(Debug)]
enum OrchestratorMessage {
    AgentEvent(AgentEvent),
    RateLimited(RateLimitSignal),
    WorkspaceProgress(WorkspaceProgressUpdate),
    WorkerFinished {
        issue_id: polyphony_core::IssueId,
        issue_identifier: String,
        attempt: Option<u32>,
        started_at: DateTime<Utc>,
        outcome: AgentRunResult,
    },
}

#[derive(Debug, Clone)]
struct ManualDispatchRequest {
    issue_id: polyphony_core::IssueId,
    agent_name: Option<String>,
    directives: Option<String>,
}

#[derive(Debug, Clone)]
struct ManualPullRequestDispatchRequest {
    trigger_id: String,
    directives: Option<String>,
}

#[derive(Debug)]
struct RunningTask {
    issue: Issue,
    agent_name: String,
    model: Option<String>,
    attempt: Option<u32>,
    workspace_path: PathBuf,
    stall_timeout_ms: i64,
    max_turns: u32,
    started_at: DateTime<Utc>,
    session_id: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    codex_app_server_pid: Option<String>,
    last_event: Option<String>,
    last_message: Option<String>,
    last_event_at: Option<DateTime<Utc>>,
    tokens: TokenUsage,
    last_reported_tokens: TokenUsage,
    turn_count: u32,
    rate_limits: Option<Value>,
    handle: JoinHandle<()>,
    /// Set when this running task is part of a pipeline.
    active_task_id: Option<TaskId>,
    /// Set when this running task is part of a pipeline.
    movement_id: Option<MovementId>,
    review_target: Option<ReviewTarget>,
    review_comment_marker: Option<String>,
    recent_log: VecDeque<String>,
}

#[derive(Debug, Clone)]
struct RetryEntry {
    row: RetryRow,
    due_at: Instant,
}

#[derive(Debug, Clone)]
struct DiscardedTriggerEntry {
    row: VisibleTriggerRow,
    discarded_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct ActiveThrottle {
    window: ThrottleWindow,
    due_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IssueClaimState {
    Running,
    RetryQueued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewTriggerSuppression {
    AwaitingApproval,
    Draft,
    AlreadyRunning,
    AlreadyReviewed,
    IgnoredAuthor { author: String },
    BotAuthor { author: String },
    IgnoredLabel { label: String },
    MissingLabels { labels: Vec<String> },
    Debounced { remaining_seconds: i64 },
}

#[derive(Debug)]
struct RuntimeState {
    running: HashMap<String, RunningTask>,
    claim_states: HashMap<String, IssueClaimState>,
    retrying: HashMap<String, RetryEntry>,
    completed: HashSet<String>,
    throttles: HashMap<String, ActiveThrottle>,
    budgets: HashMap<String, BudgetSnapshot>,
    agent_catalogs: HashMap<String, AgentModelCatalog>,
    visible_issues: Vec<VisibleIssueRow>,
    bootstrapped_visible_issues: Vec<VisibleIssueRow>,
    bootstrapped_visible_triggers: Vec<VisibleTriggerRow>,
    approved_issue_keys: HashSet<String>,
    issue_snapshot_loaded: bool,
    pull_request_snapshot_loaded: bool,
    visible_review_triggers: HashMap<String, PullRequestReviewTrigger>,
    visible_comment_triggers: HashMap<String, PullRequestCommentTrigger>,
    visible_conflict_triggers: HashMap<String, PullRequestConflictTrigger>,
    discarded_triggers: HashMap<String, DiscardedTriggerEntry>,
    saved_contexts: HashMap<String, AgentContextSnapshot>,
    recent_events: VecDeque<RuntimeEvent>,
    run_history: VecDeque<PersistedRunRecord>,
    ended_runtime_seconds: f64,
    totals: CodexTotals,
    rate_limits: Option<Value>,
    tracker_connection: Option<TrackerConnectionStatus>,
    last_tracker_poll_at: Option<DateTime<Utc>>,
    last_tracker_connection_poll_at: Option<DateTime<Utc>>,
    last_budget_poll_at: Option<DateTime<Utc>>,
    last_model_discovery_at: Option<DateTime<Utc>>,
    loading: LoadingState,
    from_cache: bool,
    cached_at: Option<DateTime<Utc>>,
    dispatch_mode: polyphony_core::DispatchMode,
    movements: HashMap<MovementId, Movement>,
    tasks: HashMap<MovementId, Vec<Task>>,
    workspace_setup_tasks_by_issue_identifier: HashMap<String, (MovementId, TaskId)>,
    workspace_setup_tasks_by_key: HashMap<String, (MovementId, TaskId)>,
    worktree_keys: HashSet<String>,
    /// Workspace keys from orphaned workspaces detected at startup, pending dispatch.
    orphan_dispatch_keys: HashSet<String>,
    reviewed_pull_request_heads: HashMap<String, ReviewedPullRequestHead>,
    pull_request_retry_triggers: HashMap<String, PullRequestTrigger>,
    review_trigger_suppressions: HashMap<String, ReviewTriggerSuppression>,
    /// Set to `true` after `restore_bootstrap` loads a persisted snapshot,
    /// so that `run()` preserves the restored dispatch mode instead of
    /// overwriting it with the config default.
    bootstrap_restored: bool,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            running: HashMap::new(),
            claim_states: HashMap::new(),
            retrying: HashMap::new(),
            completed: HashSet::new(),
            throttles: HashMap::new(),
            budgets: HashMap::new(),
            agent_catalogs: HashMap::new(),
            visible_issues: Vec::new(),
            bootstrapped_visible_issues: Vec::new(),
            bootstrapped_visible_triggers: Vec::new(),
            approved_issue_keys: HashSet::new(),
            issue_snapshot_loaded: false,
            pull_request_snapshot_loaded: false,
            visible_review_triggers: HashMap::new(),
            visible_comment_triggers: HashMap::new(),
            visible_conflict_triggers: HashMap::new(),
            discarded_triggers: HashMap::new(),
            saved_contexts: HashMap::new(),
            recent_events: VecDeque::with_capacity(128),
            run_history: VecDeque::with_capacity(256),
            ended_runtime_seconds: 0.0,
            totals: CodexTotals::default(),
            rate_limits: None,
            tracker_connection: None,
            last_tracker_poll_at: None,
            last_tracker_connection_poll_at: None,
            last_budget_poll_at: None,
            last_model_discovery_at: None,
            loading: LoadingState::default(),
            from_cache: false,
            cached_at: None,
            dispatch_mode: polyphony_core::DispatchMode::default(),
            movements: HashMap::new(),
            tasks: HashMap::new(),
            workspace_setup_tasks_by_issue_identifier: HashMap::new(),
            workspace_setup_tasks_by_key: HashMap::new(),
            worktree_keys: HashSet::new(),
            orphan_dispatch_keys: HashSet::new(),
            reviewed_pull_request_heads: HashMap::new(),
            pull_request_retry_triggers: HashMap::new(),
            review_trigger_suppressions: HashMap::new(),
            bootstrap_restored: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowFileFingerprint {
    Missing,
    Present {
        len: u64,
        modified: Option<SystemTime>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowInputsFingerprint {
    entries: Vec<(PathBuf, WorkflowFileFingerprint)>,
}

struct WorkflowReloadSupport {
    workflow_path: PathBuf,
    user_config_path: Option<PathBuf>,
    workflow_tx: watch::Sender<LoadedWorkflow>,
    component_factory: Arc<RuntimeComponentFactory>,
    last_seen_fingerprint: Option<WorkflowInputsFingerprint>,
    reload_error: Option<String>,
}

mod dispatch;
mod handoff;
mod helpers;
mod lifecycle;
mod pipeline;
mod prelude;
mod pull_request_reviews;
mod pull_request_triggers;
mod reload;
mod retry;
mod runtime;
mod snapshot;

#[cfg(test)]
mod tests;

pub use crate::handoff::spawn_workflow_watcher;
