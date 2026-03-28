use async_graphql::{Context, Enum, Object, SimpleObject, Subscription};
use chrono::{DateTime, Utc};
use polyphony_core::RuntimeSnapshot;
use tokio::sync::watch;
use tokio_stream::Stream;

// ---------------------------------------------------------------------------
// Enums (mirror core enums for GraphQL without coupling derives)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Enum, PartialEq, Eq)]
pub(crate) enum GqlMovementStatus {
    Pending,
    Planning,
    InProgress,
    Review,
    Delivered,
    Failed,
    Cancelled,
}

impl From<polyphony_core::MovementStatus> for GqlMovementStatus {
    fn from(s: polyphony_core::MovementStatus) -> Self {
        match s {
            polyphony_core::MovementStatus::Pending => Self::Pending,
            polyphony_core::MovementStatus::Planning => Self::Planning,
            polyphony_core::MovementStatus::InProgress => Self::InProgress,
            polyphony_core::MovementStatus::Review => Self::Review,
            polyphony_core::MovementStatus::Delivered => Self::Delivered,
            polyphony_core::MovementStatus::Failed => Self::Failed,
            polyphony_core::MovementStatus::Cancelled => Self::Cancelled,
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
pub(crate) enum GqlTriggerKind {
    Issue,
    PullRequestReview,
    PullRequestComment,
    PullRequestConflict,
}

impl From<polyphony_core::VisibleTriggerKind> for GqlTriggerKind {
    fn from(k: polyphony_core::VisibleTriggerKind) -> Self {
        match k {
            polyphony_core::VisibleTriggerKind::Issue => Self::Issue,
            polyphony_core::VisibleTriggerKind::PullRequestReview => Self::PullRequestReview,
            polyphony_core::VisibleTriggerKind::PullRequestComment => Self::PullRequestComment,
            polyphony_core::VisibleTriggerKind::PullRequestConflict => Self::PullRequestConflict,
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
struct GqlTrigger {
    trigger_id: String,
    kind: GqlTriggerKind,
    source: String,
    identifier: String,
    title: String,
    status: String,
    priority: Option<i32>,
    labels: Vec<String>,
    description: Option<String>,
    url: Option<String>,
    author: Option<String>,
    has_workspace: bool,
    updated_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
}

impl From<&polyphony_core::VisibleTriggerRow> for GqlTrigger {
    fn from(r: &polyphony_core::VisibleTriggerRow) -> Self {
        Self {
            trigger_id: r.trigger_id.clone(),
            kind: r.kind.into(),
            source: r.source.clone(),
            identifier: r.identifier.clone(),
            title: r.title.clone(),
            status: r.status.clone(),
            priority: r.priority,
            labels: r.labels.clone(),
            description: r.description.clone(),
            url: r.url.clone(),
            author: r.author.clone(),
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
struct GqlMovement {
    id: String,
    kind: String,
    issue_identifier: Option<String>,
    title: String,
    status: GqlMovementStatus,
    task_count: i32,
    tasks_completed: i32,
    deliverable: Option<GqlDeliverable>,
    workspace_key: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<&polyphony_core::MovementRow> for GqlMovement {
    fn from(r: &polyphony_core::MovementRow) -> Self {
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
    movement_id: String,
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
            movement_id: r.movement_id.clone(),
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

impl From<&polyphony_core::RunningRow> for GqlRunningAgent {
    fn from(r: &polyphony_core::RunningRow) -> Self {
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
    movements: i32,
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
            movements: c.movements as i32,
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
    async fn triggers(&self, ctx: &Context<'_>) -> Vec<GqlTrigger> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow()
            .visible_triggers
            .iter()
            .map(GqlTrigger::from)
            .collect()
    }

    async fn movements(&self, ctx: &Context<'_>) -> Vec<GqlMovement> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow()
            .movements
            .iter()
            .map(GqlMovement::from)
            .collect()
    }

    async fn movement(&self, ctx: &Context<'_>, id: String) -> Option<GqlMovement> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        snap.borrow()
            .movements
            .iter()
            .find(|m| m.id == id)
            .map(GqlMovement::from)
    }

    async fn tasks(&self, ctx: &Context<'_>, movement_id: Option<String>) -> Vec<GqlTask> {
        let snap = ctx.data_unchecked::<watch::Receiver<RuntimeSnapshot>>();
        let snapshot = snap.borrow();
        snapshot
            .tasks
            .iter()
            .filter(|t| movement_id.as_ref().is_none_or(|mid| &t.movement_id == mid))
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
    trigger_count: i32,
    movement_count: i32,
    running_agent_count: i32,
}

impl From<&RuntimeSnapshot> for GqlSnapshotSummary {
    fn from(s: &RuntimeSnapshot) -> Self {
        Self {
            generated_at: s.generated_at,
            counts: GqlCounts::from(&s.counts),
            dispatch_mode: s.dispatch_mode.into(),
            trigger_count: s.visible_triggers.len() as i32,
            movement_count: s.movements.len() as i32,
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
