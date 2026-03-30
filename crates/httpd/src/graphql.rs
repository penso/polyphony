use async_graphql::{Context, Enum, Object, SimpleObject, Subscription};
use chrono::{DateTime, Utc};
use polyphony_core::RuntimeSnapshot;
use tokio::sync::watch;
use tokio_stream::Stream;

// ---------------------------------------------------------------------------
// Enums (mirror core enums for GraphQL without coupling derives)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlRunStatus {
    Pending,
    Planning,
    InProgress,
    Review,
    Delivered,
    Failed,
    Cancelled,
}

impl From<polyphony_core::RunStatus> for GqlRunStatus {
    fn from(s: polyphony_core::RunStatus) -> Self {
        match s {
            polyphony_core::RunStatus::Pending => Self::Pending,
            polyphony_core::RunStatus::Planning => Self::Planning,
            polyphony_core::RunStatus::InProgress => Self::InProgress,
            polyphony_core::RunStatus::Review => Self::Review,
            polyphony_core::RunStatus::Delivered => Self::Delivered,
            polyphony_core::RunStatus::Failed => Self::Failed,
            polyphony_core::RunStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlTaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl From<polyphony_core::TaskStatus> for GqlTaskStatus {
    fn from(s: polyphony_core::TaskStatus) -> Self {
        match s {
            polyphony_core::TaskStatus::Pending => Self::Pending,
            polyphony_core::TaskStatus::InProgress => Self::InProgress,
            polyphony_core::TaskStatus::Completed => Self::Completed,
            polyphony_core::TaskStatus::Failed => Self::Failed,
            polyphony_core::TaskStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlTaskCategory {
    Research,
    Coding,
    Testing,
    Documentation,
    Review,
    Feedback,
}

impl From<polyphony_core::TaskCategory> for GqlTaskCategory {
    fn from(c: polyphony_core::TaskCategory) -> Self {
        match c {
            polyphony_core::TaskCategory::Research => Self::Research,
            polyphony_core::TaskCategory::Coding => Self::Coding,
            polyphony_core::TaskCategory::Testing => Self::Testing,
            polyphony_core::TaskCategory::Documentation => Self::Documentation,
            polyphony_core::TaskCategory::Review => Self::Review,
            polyphony_core::TaskCategory::Feedback => Self::Feedback,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlDispatchMode {
    Manual,
    Automatic,
    Nightshift,
    Idle,
    Stop,
}

impl From<polyphony_core::DispatchMode> for GqlDispatchMode {
    fn from(m: polyphony_core::DispatchMode) -> Self {
        match m {
            polyphony_core::DispatchMode::Manual => Self::Manual,
            polyphony_core::DispatchMode::Automatic => Self::Automatic,
            polyphony_core::DispatchMode::Nightshift => Self::Nightshift,
            polyphony_core::DispatchMode::Idle => Self::Idle,
            polyphony_core::DispatchMode::Stop => Self::Stop,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlDeliverableStatus {
    Pending,
    Open,
    Merged,
    Closed,
    Reviewed,
}

impl From<polyphony_core::DeliverableStatus> for GqlDeliverableStatus {
    fn from(s: polyphony_core::DeliverableStatus) -> Self {
        match s {
            polyphony_core::DeliverableStatus::Pending => Self::Pending,
            polyphony_core::DeliverableStatus::Open => Self::Open,
            polyphony_core::DeliverableStatus::Merged => Self::Merged,
            polyphony_core::DeliverableStatus::Closed => Self::Closed,
            polyphony_core::DeliverableStatus::Reviewed => Self::Reviewed,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlDeliverableDecision {
    Waiting,
    Accepted,
    Rejected,
}

impl From<polyphony_core::DeliverableDecision> for GqlDeliverableDecision {
    fn from(d: polyphony_core::DeliverableDecision) -> Self {
        match d {
            polyphony_core::DeliverableDecision::Waiting => Self::Waiting,
            polyphony_core::DeliverableDecision::Accepted => Self::Accepted,
            polyphony_core::DeliverableDecision::Rejected => Self::Rejected,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlApprovalState {
    Approved,
    Waiting,
}

impl From<polyphony_core::DispatchApprovalState> for GqlApprovalState {
    fn from(s: polyphony_core::DispatchApprovalState) -> Self {
        match s {
            polyphony_core::DispatchApprovalState::Approved => Self::Approved,
            polyphony_core::DispatchApprovalState::Waiting => Self::Waiting,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlInboxItemKind {
    Issue,
    PullRequestReview,
    PullRequestComment,
    PullRequestConflict,
}

impl From<polyphony_core::InboxItemKind> for GqlInboxItemKind {
    fn from(k: polyphony_core::InboxItemKind) -> Self {
        match k {
            polyphony_core::InboxItemKind::Issue => Self::Issue,
            polyphony_core::InboxItemKind::PullRequestReview => Self::PullRequestReview,
            polyphony_core::InboxItemKind::PullRequestComment => Self::PullRequestComment,
            polyphony_core::InboxItemKind::PullRequestConflict => Self::PullRequestConflict,
        }
    }
}

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlEventScope {
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

impl From<polyphony_core::EventScope> for GqlEventScope {
    fn from(s: polyphony_core::EventScope) -> Self {
        match s {
            polyphony_core::EventScope::Workflow => Self::Workflow,
            polyphony_core::EventScope::Throttle => Self::Throttle,
            polyphony_core::EventScope::Dispatch => Self::Dispatch,
            polyphony_core::EventScope::Handoff => Self::Handoff,
            polyphony_core::EventScope::Agent => Self::Agent,
            polyphony_core::EventScope::Retry => Self::Retry,
            polyphony_core::EventScope::Worker => Self::Worker,
            polyphony_core::EventScope::Reconcile => Self::Reconcile,
            polyphony_core::EventScope::Tracker => Self::Tracker,
            polyphony_core::EventScope::Startup => Self::Startup,
            polyphony_core::EventScope::Feedback => Self::Feedback,
        }
    }
}

// ---------------------------------------------------------------------------
// GraphQL object types
// ---------------------------------------------------------------------------

#[derive(SimpleObject)]
struct GqlInboxItem {
    item_id: String,
    kind: GqlInboxItemKind,
    source: String,
    identifier: String,
    title: String,
    status: String,
    approval_state: GqlApprovalState,
    priority: Option<i32>,
    labels: Vec<String>,
    description: Option<String>,
    url: Option<String>,
    author: Option<String>,
    parent_id: Option<String>,
    has_workspace: bool,
    updated_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
}

impl From<&polyphony_core::InboxItemRow> for GqlInboxItem {
    fn from(r: &polyphony_core::InboxItemRow) -> Self {
        Self {
            item_id: r.item_id.clone(),
            kind: r.kind.into(),
            source: r.source.clone(),
            identifier: r.identifier.clone(),
            title: r.title.clone(),
            status: r.status.clone(),
            approval_state: r.approval_state.into(),
            priority: r.priority,
            labels: r.labels.clone(),
            description: r.description.clone(),
            url: r.url.clone(),
            author: r.author.clone(),
            parent_id: r.parent_id.clone(),
            has_workspace: r.has_workspace,
            updated_at: r.updated_at,
            created_at: r.created_at,
        }
    }
}

#[derive(SimpleObject)]
struct GqlDeliverable {
    kind: String,
    status: GqlDeliverableStatus,
    decision: GqlDeliverableDecision,
    url: Option<String>,
    title: Option<String>,
    description: Option<String>,
}

impl From<&polyphony_core::Deliverable> for GqlDeliverable {
    fn from(d: &polyphony_core::Deliverable) -> Self {
        Self {
            kind: d.kind.to_string(),
            status: d.status.into(),
            decision: d.decision.into(),
            url: d.url.clone(),
            title: d.title.clone(),
            description: d.description.clone(),
        }
    }
}

#[derive(SimpleObject)]
struct GqlRun {
    id: String,
    kind: String,
    issue_identifier: Option<String>,
    title: String,
    status: GqlRunStatus,
    task_count: i32,
    tasks_completed: i32,
    deliverable: Option<GqlDeliverable>,
    workspace_key: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<&polyphony_core::RunRow> for GqlRun {
    fn from(r: &polyphony_core::RunRow) -> Self {
        Self {
            id: r.id.clone(),
            kind: r.kind.to_string(),
            issue_identifier: r.issue_identifier.clone(),
            title: r.title.clone(),
            status: r.status.into(),
            task_count: r.task_count as i32,
            tasks_completed: r.tasks_completed as i32,
            deliverable: r.deliverable.as_ref().map(GqlDeliverable::from),
            workspace_key: r.workspace_key.clone(),
            created_at: r.created_at,
        }
    }
}

#[derive(SimpleObject)]
struct GqlTask {
    id: String,
    run_id: String,
    title: String,
    description: Option<String>,
    category: GqlTaskCategory,
    status: GqlTaskStatus,
    ordinal: i32,
    agent_name: Option<String>,
    turns_completed: i32,
    total_tokens: String,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    error: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<&polyphony_core::TaskRow> for GqlTask {
    fn from(r: &polyphony_core::TaskRow) -> Self {
        Self {
            id: r.id.clone(),
            run_id: r.run_id.clone(),
            title: r.title.clone(),
            description: r.description.clone(),
            category: r.category.into(),
            status: r.status.into(),
            ordinal: r.ordinal as i32,
            agent_name: r.agent_name.clone(),
            turns_completed: r.turns_completed as i32,
            total_tokens: r.total_tokens.to_string(),
            started_at: r.started_at,
            finished_at: r.finished_at,
            error: r.error.clone(),
            created_at: r.created_at,
        }
    }
}

#[derive(SimpleObject)]
struct GqlRunningAgent {
    issue_id: String,
    issue_identifier: String,
    agent_name: String,
    model: Option<String>,
    state: String,
    turn_count: i32,
    max_turns: i32,
    last_event: Option<String>,
    last_message: Option<String>,
    started_at: DateTime<Utc>,
    last_event_at: Option<DateTime<Utc>>,
}

impl From<&polyphony_core::RunningAgentRow> for GqlRunningAgent {
    fn from(r: &polyphony_core::RunningAgentRow) -> Self {
        Self {
            issue_id: r.issue_id.clone(),
            issue_identifier: r.issue_identifier.clone(),
            agent_name: r.agent_name.clone(),
            model: r.model.clone(),
            state: r.state.clone(),
            turn_count: r.turn_count as i32,
            max_turns: r.max_turns as i32,
            last_event: r.last_event.clone(),
            last_message: r.last_message.clone(),
            started_at: r.started_at,
            last_event_at: r.last_event_at,
        }
    }
}

#[derive(SimpleObject)]
struct GqlRuntimeEvent {
    at: DateTime<Utc>,
    scope: GqlEventScope,
    message: String,
}

impl From<&polyphony_core::RuntimeEvent> for GqlRuntimeEvent {
    fn from(e: &polyphony_core::RuntimeEvent) -> Self {
        Self {
            at: e.at,
            scope: e.scope.into(),
            message: e.message.clone(),
        }
    }
}

#[derive(SimpleObject)]
struct GqlCounts {
    running: i32,
    retrying: i32,
    runs: i32,
    tasks_pending: i32,
    tasks_in_progress: i32,
    tasks_completed: i32,
    worktrees: i32,
}

impl From<&polyphony_core::SnapshotCounts> for GqlCounts {
    fn from(c: &polyphony_core::SnapshotCounts) -> Self {
        Self {
            running: c.running as i32,
            retrying: c.retrying as i32,
            runs: c.runs as i32,
            tasks_pending: c.tasks_pending as i32,
            tasks_in_progress: c.tasks_in_progress as i32,
            tasks_completed: c.tasks_completed as i32,
            worktrees: c.worktrees as i32,
        }
    }
}

// ---------------------------------------------------------------------------
// Query root
// ---------------------------------------------------------------------------

pub(crate) struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn inbox(&self, ctx: &Context<'_>) -> Vec<GqlInboxItem> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow()
            .inbox_items
            .iter()
            .map(GqlInboxItem::from)
            .collect()
    }

    async fn runs(&self, ctx: &Context<'_>) -> Vec<GqlRun> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow().runs.iter().map(GqlRun::from).collect()
    }

    async fn run(&self, ctx: &Context<'_>, id: String) -> Option<GqlRun> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow()
            .runs
            .iter()
            .find(|m| m.id == id)
            .map(GqlRun::from)
    }

    async fn tasks(&self, ctx: &Context<'_>, run_id: Option<String>) -> Vec<GqlTask> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        let snapshot = snap.borrow();
        snapshot
            .tasks
            .iter()
            .filter(|t| run_id.as_ref().is_none_or(|mid| &t.run_id == mid))
            .map(GqlTask::from)
            .collect()
    }

    async fn running_agents(&self, ctx: &Context<'_>) -> Vec<GqlRunningAgent> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow()
            .running
            .iter()
            .map(GqlRunningAgent::from)
            .collect()
    }

    async fn recent_events(&self, ctx: &Context<'_>, limit: Option<i32>) -> Vec<GqlRuntimeEvent> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        let snapshot = snap.borrow();
        let limit = limit.unwrap_or(50).max(0) as usize;
        snapshot
            .recent_events
            .iter()
            .rev()
            .take(limit)
            .map(GqlRuntimeEvent::from)
            .collect()
    }

    async fn counts(&self, ctx: &Context<'_>) -> GqlCounts {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        GqlCounts::from(&snap.borrow().counts)
    }

    async fn dispatch_mode(&self, ctx: &Context<'_>) -> GqlDispatchMode {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow().dispatch_mode.into()
    }

    /// List directories matching a prefix path, for repo autocomplete.
    /// Returns directories that exist and contain a `.git` folder or are valid parent dirs.
    async fn list_directories(&self, prefix: String) -> Vec<String> {
        let path = std::path::Path::new(&prefix);

        // Determine parent dir and filename prefix for filtering
        let (parent, name_prefix) = if path.is_dir() {
            (path.to_path_buf(), String::new())
        } else {
            let parent = path.parent().unwrap_or(std::path::Path::new("/"));
            let name_prefix = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            (parent.to_path_buf(), name_prefix)
        };

        let Ok(entries) = std::fs::read_dir(&parent) else {
            return vec![];
        };

        let mut results: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && name.starts_with(name_prefix.as_str())
            })
            .map(|e| {
                let mut p = e.path().to_string_lossy().to_string();
                if !p.ends_with('/') {
                    p.push('/');
                }
                p
            })
            .take(20)
            .collect();
        results.sort();
        results
    }
}

// ---------------------------------------------------------------------------
// Mutation root
// ---------------------------------------------------------------------------

pub(crate) struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn set_mode(&self, ctx: &Context<'_>, mode: GqlDispatchMode) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        let core_mode = match mode {
            GqlDispatchMode::Manual => polyphony_core::DispatchMode::Manual,
            GqlDispatchMode::Automatic => polyphony_core::DispatchMode::Automatic,
            GqlDispatchMode::Nightshift => polyphony_core::DispatchMode::Nightshift,
            GqlDispatchMode::Idle => polyphony_core::DispatchMode::Idle,
            GqlDispatchMode::Stop => polyphony_core::DispatchMode::Stop,
        };
        tx.send(polyphony_orchestrator::RuntimeCommand::SetMode(core_mode))
            .is_ok()
    }

    async fn dispatch_issue(
        &self,
        ctx: &Context<'_>,
        issue_id: String,
        agent_name: Option<String>,
        directives: Option<String>,
    ) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::DispatchIssue {
            issue_id,
            agent_name,
            directives,
        })
        .is_ok()
    }

    async fn refresh(&self, ctx: &Context<'_>) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::Refresh)
            .is_ok()
    }

    async fn approve_inbox_item(&self, ctx: &Context<'_>, item_id: String, source: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::ApproveInboxItem { item_id, source })
            .is_ok()
    }

    async fn dispatch_pull_request_inbox_item(
        &self,
        ctx: &Context<'_>,
        item_id: String,
        directives: Option<String>,
    ) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(
            polyphony_orchestrator::RuntimeCommand::DispatchPullRequestInboxItem {
                item_id,
                directives,
            },
        )
        .is_ok()
    }

    async fn close_tracker_issue(&self, ctx: &Context<'_>, issue_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::CloseTrackerIssue { issue_id })
            .is_ok()
    }

    async fn resolve_run_deliverable(
        &self,
        ctx: &Context<'_>,
        run_id: String,
        decision: GqlDeliverableDecision,
    ) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        let core_decision = match decision {
            GqlDeliverableDecision::Waiting => polyphony_core::DeliverableDecision::Waiting,
            GqlDeliverableDecision::Accepted => polyphony_core::DeliverableDecision::Accepted,
            GqlDeliverableDecision::Rejected => polyphony_core::DeliverableDecision::Rejected,
        };
        tx.send(
            polyphony_orchestrator::RuntimeCommand::ResolveRunDeliverable {
                run_id,
                decision: core_decision,
            },
        )
        .is_ok()
    }

    async fn retry_run(&self, ctx: &Context<'_>, run_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::RetryRun { run_id })
            .is_ok()
    }

    async fn merge_deliverable(&self, ctx: &Context<'_>, run_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::MergeDeliverable { run_id })
            .is_ok()
    }

    async fn resolve_task(&self, ctx: &Context<'_>, run_id: String, task_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::ResolveTask { run_id, task_id })
            .is_ok()
    }

    async fn retry_task(&self, ctx: &Context<'_>, run_id: String, task_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::RetryTask { run_id, task_id })
            .is_ok()
    }

    async fn stop_agent(&self, ctx: &Context<'_>, issue_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::StopAgent { issue_id })
            .is_ok()
    }

    async fn create_issue(
        &self,
        ctx: &Context<'_>,
        title: String,
        description: String,
        repo_id: Option<String>,
    ) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::CreateIssue {
            title,
            description,
            repo_id,
        })
        .is_ok()
    }

    async fn inject_run_feedback(
        &self,
        ctx: &Context<'_>,
        run_id: String,
        prompt: String,
        agent_name: Option<String>,
    ) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::InjectRunFeedback {
            run_id,
            prompt,
            agent_name,
        })
        .is_ok()
    }

    async fn refresh_repo(&self, ctx: &Context<'_>, repo_id: String) -> bool {
        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        tx.send(polyphony_orchestrator::RuntimeCommand::RefreshRepo { repo_id })
            .is_ok()
    }

    /// Add a repository by local path or remote URL.
    async fn add_repo(
        &self,
        ctx: &Context<'_>,
        source: String,
        branch: Option<String>,
    ) -> async_graphql::Result<bool> {
        let registry_path = polyphony_core::default_repo_registry_path();
        let mut registry = polyphony_core::load_repo_registry(&registry_path)
            .map_err(|e| async_graphql::Error::new(format!("loading registry: {e}")))?;
        let registration = polyphony_core::build_repo_registration(&source, branch.as_deref())
            .map_err(|e| async_graphql::Error::new(format!("building registration: {e}")))?;

        if registry.contains(&registration.repo_id) {
            return Err(async_graphql::Error::new(format!(
                "repository '{}' is already registered",
                registration.repo_id
            )));
        }

        registry.add(registration.clone());
        polyphony_core::save_repo_registry(&registry_path, &registry)
            .map_err(|e| async_graphql::Error::new(format!("saving registry: {e}")))?;

        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        Ok(tx
            .send(polyphony_orchestrator::RuntimeCommand::AddRepo(
                registration.clone(),
            ))
            .is_ok())
    }

    /// Remove a repository by ID.
    async fn remove_repo(&self, ctx: &Context<'_>, repo_id: String) -> async_graphql::Result<bool> {
        let registry_path = polyphony_core::default_repo_registry_path();
        let mut registry = polyphony_core::load_repo_registry(&registry_path)
            .map_err(|e| async_graphql::Error::new(format!("loading registry: {e}")))?;

        registry.remove(&repo_id).ok_or_else(|| {
            async_graphql::Error::new(format!("repository '{repo_id}' not found"))
        })?;

        polyphony_core::save_repo_registry(&registry_path, &registry)
            .map_err(|e| async_graphql::Error::new(format!("saving registry: {e}")))?;

        let tx = ctx.data_unchecked::<tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>>();
        Ok(tx
            .send(polyphony_orchestrator::RuntimeCommand::RemoveRepo { repo_id })
            .is_ok())
    }
}

// ---------------------------------------------------------------------------
// Subscription root (WebSocket real-time)
// ---------------------------------------------------------------------------

pub(crate) struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    /// Emits the full snapshot whenever the runtime state changes.
    async fn snapshot_updated(&self, ctx: &Context<'_>) -> impl Stream<Item = GqlSnapshotSummary> {
        let rx = ctx
            .data_unchecked::<watch::Receiver<RuntimeSnapshot>>()
            .clone();
        async_stream::stream! {
            let mut rx = rx;
            while rx.changed().await.is_ok() {
                let summary = {
                    let snap = rx.borrow();
                    GqlSnapshotSummary::from(&*snap)
                };
                yield summary;
            }
        }
    }

    /// Emits new runtime events as they arrive.
    async fn events(&self, ctx: &Context<'_>) -> impl Stream<Item = GqlRuntimeEvent> {
        let rx = ctx
            .data_unchecked::<watch::Receiver<RuntimeSnapshot>>()
            .clone();
        async_stream::stream! {
            let mut rx = rx;
            let mut last_count = 0usize;
            while rx.changed().await.is_ok() {
                let new_events = {
                    let snap = rx.borrow();
                    let events = &snap.recent_events;
                    let batch: Vec<GqlRuntimeEvent> = if events.len() > last_count {
                        events[last_count..].iter().map(GqlRuntimeEvent::from).collect()
                    } else {
                        Vec::new()
                    };
                    last_count = events.len();
                    batch
                };
                for event in new_events {
                    yield event;
                }
            }
        }
    }
}

#[derive(SimpleObject)]
struct GqlSnapshotSummary {
    generated_at: DateTime<Utc>,
    counts: GqlCounts,
    dispatch_mode: GqlDispatchMode,
    inbox_count: i32,
    run_count: i32,
    running_agent_count: i32,
}

impl From<&RuntimeSnapshot> for GqlSnapshotSummary {
    fn from(s: &RuntimeSnapshot) -> Self {
        Self {
            generated_at: s.generated_at,
            counts: GqlCounts::from(&s.counts),
            dispatch_mode: s.dispatch_mode.into(),
            inbox_count: s.inbox_items.len() as i32,
            run_count: s.runs.len() as i32,
            running_agent_count: s.running.len() as i32,
        }
    }
}

// ---------------------------------------------------------------------------
// Schema builder
// ---------------------------------------------------------------------------

pub(crate) type PolyphonySchema = async_graphql::Schema<QueryRoot, MutationRoot, SubscriptionRoot>;

pub(crate) fn build_schema(
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: tokio::sync::mpsc::UnboundedSender<polyphony_orchestrator::RuntimeCommand>,
) -> PolyphonySchema {
    async_graphql::Schema::build(QueryRoot, MutationRoot, SubscriptionRoot)
        .data(snapshot_rx)
        .data(command_tx)
        .finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Build a test schema with real channels so mutations can send commands.
    fn test_schema() -> (
        PolyphonySchema,
        tokio::sync::mpsc::UnboundedReceiver<polyphony_orchestrator::RuntimeCommand>,
    ) {
        let snapshot: RuntimeSnapshot = serde_json::from_value(serde_json::json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "counts": { "running": 0, "retrying": 0, "runs": 0, "tasks_pending": 0, "tasks_in_progress": 0, "tasks_completed": 0, "worktrees": 0 },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": []
        }))
        .expect("minimal snapshot should deserialize");
        let (snap_tx, snap_rx) = watch::channel(snapshot);
        drop(snap_tx);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let schema = build_schema(snap_rx, cmd_tx);
        (schema, cmd_rx)
    }

    /// The exact mutation strings used in inbox.html JavaScript.
    /// If a query here fails to validate, the frontend mutation is broken.
    const JS_MUTATION_APPROVE: &str = r#"mutation($itemId: String!, $source: String!) { approveInboxItem(itemId: $itemId, source: $source) }"#;
    const JS_MUTATION_DISPATCH_ISSUE: &str = r#"mutation($id: String!, $a: String, $d: String) { dispatchIssue(issueId: $id, agentName: $a, directives: $d) }"#;
    const JS_MUTATION_DISPATCH_PR: &str = r#"mutation($id: String!, $d: String) { dispatchPullRequestInboxItem(itemId: $id, directives: $d) }"#;
    const JS_MUTATION_CLOSE: &str = r#"mutation($id: String!) { closeTrackerIssue(issueId: $id) }"#;
    const JS_MUTATION_REFRESH: &str = r#"mutation { refresh }"#;

    /// Execute a mutation and assert it succeeds (no GraphQL errors).
    async fn assert_mutation_ok(
        schema: &PolyphonySchema,
        query: &str,
        variables: serde_json::Value,
    ) {
        let vars: async_graphql::Variables = async_graphql::Variables::from_json(variables);
        let request = async_graphql::Request::new(query).variables(vars);
        let response = schema.execute(request).await;
        assert!(
            response.errors.is_empty(),
            "GraphQL errors for query {query:?}: {:?}",
            response.errors
        );
    }

    #[tokio::test]
    async fn js_mutation_approve_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_APPROVE,
            json!({"itemId": "test-1", "source": "github"}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_dispatch_issue_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_DISPATCH_ISSUE,
            json!({"id": "test-1", "a": "implementer", "d": "fix the bug"}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_dispatch_pr_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_DISPATCH_PR,
            json!({"id": "test-1", "d": null}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_close_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_CLOSE, json!({"id": "test-1"})).await;
    }

    #[tokio::test]
    async fn js_mutation_refresh_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_REFRESH, json!({})).await;
    }

    #[tokio::test]
    async fn approve_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_APPROVE,
            json!({"itemId": "issue-42", "source": "github"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::ApproveInboxItem { item_id, source } => {
                assert_eq!(item_id, "issue-42");
                assert_eq!(source, "github");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn close_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_CLOSE, json!({"id": "issue-42"})).await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::CloseTrackerIssue { issue_id } => {
                assert_eq!(issue_id, "issue-42");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_REFRESH, json!({})).await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::Refresh => {},
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_issue_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_DISPATCH_ISSUE,
            json!({"id": "issue-42", "a": "implementer", "d": "fix it"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::DispatchIssue {
                issue_id,
                agent_name,
                directives,
            } => {
                assert_eq!(issue_id, "issue-42");
                assert_eq!(agent_name.as_deref(), Some("implementer"));
                assert_eq!(directives.as_deref(), Some("fix it"));
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_pr_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_DISPATCH_PR,
            json!({"id": "pr-1", "d": null}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::DispatchPullRequestInboxItem {
                item_id,
                directives,
            } => {
                assert_eq!(item_id, "pr-1");
                assert!(directives.is_none());
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    // Runs page mutation strings
    const JS_MUTATION_RESOLVE: &str = r#"mutation($id: String!, $d: GqlDeliverableDecision!) { resolveRunDeliverable(runId: $id, decision: $d) }"#;
    const JS_MUTATION_RETRY: &str = r#"mutation($id: String!) { retryRun(runId: $id) }"#;
    const JS_MUTATION_MERGE: &str = r#"mutation($id: String!) { mergeDeliverable(runId: $id) }"#;
    const JS_MUTATION_INJECT_FEEDBACK: &str = r#"mutation($id: String!, $p: String!, $a: String) { injectRunFeedback(runId: $id, prompt: $p, agentName: $a) }"#;
    const JS_MUTATION_RESOLVE_TASK: &str = r#"mutation($runId: String!, $taskId: String!) { resolveTask(runId: $runId, taskId: $taskId) }"#;
    const JS_MUTATION_RETRY_TASK: &str = r#"mutation($runId: String!, $taskId: String!) { retryTask(runId: $runId, taskId: $taskId) }"#;
    const JS_MUTATION_STOP_AGENT: &str = r#"mutation($id: String!) { stopAgent(issueId: $id) }"#;
    const JS_MUTATION_REFRESH_REPO: &str = r#"mutation($id: String!) { refreshRepo(repoId: $id) }"#;

    #[tokio::test]
    async fn js_mutation_resolve_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_RESOLVE,
            json!({"id": "run-1", "d": "ACCEPTED"}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_retry_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_RETRY, json!({"id": "run-1"})).await;
    }

    #[tokio::test]
    async fn js_mutation_merge_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_MERGE, json!({"id": "run-1"})).await;
    }

    #[tokio::test]
    async fn js_mutation_inject_feedback_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_INJECT_FEEDBACK,
            json!({"id": "run-1", "p": "please add tests", "a": "reviewer"}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_resolve_task_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_RESOLVE_TASK,
            json!({"runId": "run-1", "taskId": "task-1"}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_retry_task_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_RETRY_TASK,
            json!({"runId": "run-1", "taskId": "task-1"}),
        )
        .await;
    }

    #[tokio::test]
    async fn js_mutation_stop_agent_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_STOP_AGENT, json!({"id": "issue-1"})).await;
    }

    #[tokio::test]
    async fn js_mutation_refresh_repo_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_REFRESH_REPO,
            json!({"id": "owner/repo"}),
        )
        .await;
    }

    #[tokio::test]
    async fn resolve_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_RESOLVE,
            json!({"id": "run-42", "d": "REJECTED"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::ResolveRunDeliverable { run_id, decision } => {
                assert_eq!(run_id, "run-42");
                assert_eq!(decision, polyphony_core::DeliverableDecision::Rejected);
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn retry_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_RETRY, json!({"id": "run-42"})).await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::RetryRun { run_id } => {
                assert_eq!(run_id, "run-42");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn merge_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_MERGE, json!({"id": "run-42"})).await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::MergeDeliverable { run_id } => {
                assert_eq!(run_id, "run-42");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn inject_feedback_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_INJECT_FEEDBACK,
            json!({"id": "run-42", "p": "please add a regression test", "a": "reviewer"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::InjectRunFeedback {
                run_id,
                prompt,
                agent_name,
            } => {
                assert_eq!(run_id, "run-42");
                assert_eq!(prompt, "please add a regression test");
                assert_eq!(agent_name.as_deref(), Some("reviewer"));
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_task_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_RESOLVE_TASK,
            json!({"runId": "run-42", "taskId": "task-9"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::ResolveTask { run_id, task_id } => {
                assert_eq!(run_id, "run-42");
                assert_eq!(task_id, "task-9");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn retry_task_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_RETRY_TASK,
            json!({"runId": "run-42", "taskId": "task-9"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::RetryTask { run_id, task_id } => {
                assert_eq!(run_id, "run-42");
                assert_eq!(task_id, "task-9");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_agent_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(&schema, JS_MUTATION_STOP_AGENT, json!({"id": "issue-42"})).await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::StopAgent { issue_id } => {
                assert_eq!(issue_id, "issue-42");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_repo_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_REFRESH_REPO,
            json!({"id": "owner/repo"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::RefreshRepo { repo_id } => {
                assert_eq!(repo_id, "owner/repo");
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    // Create issue mutation
    const JS_MUTATION_CREATE_ISSUE: &str = r#"mutation($t: String!, $d: String!, $r: String) { createIssue(title: $t, description: $d, repoId: $r) }"#;

    #[tokio::test]
    async fn js_mutation_create_issue_validates() {
        let (schema, _rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_CREATE_ISSUE,
            json!({"t": "Fix bug", "d": "The thing is broken", "r": null}),
        )
        .await;
    }

    #[tokio::test]
    async fn create_issue_sends_correct_command() {
        let (schema, mut rx) = test_schema();
        assert_mutation_ok(
            &schema,
            JS_MUTATION_CREATE_ISSUE,
            json!({"t": "New feature", "d": "Add dark mode", "r": "owner/repo"}),
        )
        .await;
        let cmd = rx.try_recv().expect("expected a command");
        match cmd {
            polyphony_orchestrator::RuntimeCommand::CreateIssue {
                title,
                description,
                repo_id,
            } => {
                assert_eq!(title, "New feature");
                assert_eq!(description, "Add dark mode");
                assert_eq!(repo_id.as_deref(), Some("owner/repo"));
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
