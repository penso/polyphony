use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use {
    chrono::{DateTime, Utc},
    notify::{RecommendedWatcher, RecursiveMode, Watcher},
    polyphony_core::{
        AgentContextEntry, AgentContextSnapshot, AgentEvent, AgentEventKind, AgentModelCatalog,
        AgentRunResult, AgentRunSpec, AgentRuntime, AttemptStatus, BudgetSnapshot, CachedSnapshot,
        CodexTotals, Error as CoreError, EventScope, FeedbackAction, FeedbackLink,
        FeedbackNotification, Issue, IssueTracker, LoadingState, NetworkCache, PersistedRunRecord,
        PullRequestCommenter, PullRequestManager, PullRequestRequest, RateLimitSignal, RetryRow,
        RunningRow, RuntimeCadence, RuntimeEvent, RuntimeSnapshot, SnapshotCounts, StateStore,
        ThrottleWindow, TokenUsage, VisibleIssueRow, WorkspaceCommitRequest, WorkspaceCommitter,
        WorkspaceProvisioner, sanitize_workspace_key,
    },
    polyphony_feedback::FeedbackRegistry,
    polyphony_workflow::{
        HooksConfig, LoadedWorkflow, load_workflow_with_user_config,
        render_issue_template_with_strings, render_turn_prompt, render_turn_template,
    },
    polyphony_workspace::WorkspaceManager,
    serde_json::Value,
    thiserror::Error,
    tokio::{
        sync::{mpsc, watch},
        task::JoinHandle,
    },
    tracing::{Instrument, debug, error, info, info_span, warn},
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
const DEFAULT_AUTOMATION_PR_BODY: &str = "Automated handoff for {{ issue.identifier }}.\n\nIssue: {{ issue.url }}\nBase branch: {{ base_branch }}\nHead branch: {{ head_branch }}\nCommit: {{ commit_sha }}";
const DEFAULT_AUTOMATION_REVIEW_PROMPT: &str = "Review the current branch against {{ base_branch }}.\nInspect the repository state and write a concise markdown review to `.polyphony/review.md`.\nInclude these sections:\n- Summary\n- Risks\n- Recommended human checks\nDo not modify tracked source files other than `.polyphony/review.md`.";

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    Refresh,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    pub snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    pub command_tx: mpsc::UnboundedSender<RuntimeCommand>,
}

pub struct RuntimeComponents {
    pub tracker: Arc<dyn IssueTracker>,
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
    state: RuntimeState,
    reload_support: Option<WorkflowReloadSupport>,
}

#[derive(Debug)]
enum OrchestratorMessage {
    AgentEvent(AgentEvent),
    RateLimited(RateLimitSignal),
    WorkerFinished {
        issue_id: String,
        issue_identifier: String,
        attempt: Option<u32>,
        started_at: DateTime<Utc>,
        outcome: AgentRunResult,
    },
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
}

#[derive(Debug, Clone)]
struct RetryEntry {
    row: RetryRow,
    due_at: Instant,
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
    saved_contexts: HashMap<String, AgentContextSnapshot>,
    recent_events: VecDeque<RuntimeEvent>,
    ended_runtime_seconds: f64,
    totals: CodexTotals,
    rate_limits: Option<Value>,
    last_tracker_poll_at: Option<DateTime<Utc>>,
    last_budget_poll_at: Option<DateTime<Utc>>,
    last_model_discovery_at: Option<DateTime<Utc>>,
    loading: LoadingState,
    from_cache: bool,
    cached_at: Option<DateTime<Utc>>,
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
            saved_contexts: HashMap::new(),
            recent_events: VecDeque::with_capacity(128),
            ended_runtime_seconds: 0.0,
            totals: CodexTotals::default(),
            rate_limits: None,
            last_tracker_poll_at: None,
            last_budget_poll_at: None,
            last_model_discovery_at: None,
            loading: LoadingState::default(),
            from_cache: false,
            cached_at: None,
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

struct WorkflowReloadSupport {
    workflow_path: PathBuf,
    user_config_path: Option<PathBuf>,
    workflow_tx: watch::Sender<LoadedWorkflow>,
    component_factory: Arc<RuntimeComponentFactory>,
    last_seen_fingerprint: Option<WorkflowFileFingerprint>,
    reload_error: Option<String>,
}

impl RuntimeService {
    pub fn new(
        tracker: Arc<dyn IssueTracker>,
        agent: Arc<dyn AgentRuntime>,
        provisioner: Arc<dyn WorkspaceProvisioner>,
        committer: Option<Arc<dyn WorkspaceCommitter>>,
        pull_request_manager: Option<Arc<dyn PullRequestManager>>,
        pull_request_commenter: Option<Arc<dyn PullRequestCommenter>>,
        feedback: Option<Arc<FeedbackRegistry>>,
        store: Option<Arc<dyn StateStore>>,
        cache: Option<Arc<dyn NetworkCache>>,
        workflow_rx: watch::Receiver<LoadedWorkflow>,
    ) -> (Self, RuntimeHandle) {
        let (snapshot_tx, snapshot_rx) = watch::channel(empty_snapshot());
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (external_command_tx, external_command_rx) = mpsc::unbounded_channel();
        (
            Self {
                tracker,
                agent,
                provisioner,
                committer,
                pull_request_manager,
                pull_request_commenter,
                feedback,
                store,
                cache,
                workflow_rx,
                snapshot_tx,
                command_tx: command_tx.clone(),
                command_rx,
                external_command_rx,
                pending_refresh: false,
                reload_support: None,
                state: RuntimeState::default(),
            },
            RuntimeHandle {
                snapshot_rx,
                command_tx: external_command_tx,
            },
        )
    }

    pub fn with_workflow_reload(
        mut self,
        workflow_path: PathBuf,
        user_config_path: Option<PathBuf>,
        workflow_tx: watch::Sender<LoadedWorkflow>,
        component_factory: Arc<RuntimeComponentFactory>,
    ) -> Self {
        self.reload_support = Some(WorkflowReloadSupport {
            last_seen_fingerprint: workflow_file_fingerprint(&workflow_path).ok(),
            workflow_path,
            user_config_path,
            workflow_tx,
            component_factory,
            reload_error: None,
        });
        self
    }

    pub async fn run(mut self) -> Result<(), Error> {
        if let Some(store) = &self.store {
            let bootstrap = store.bootstrap().await?;
            self.restore_bootstrap(bootstrap);
        }
        if let Some(cache) = &self.cache {
            if let Ok(cached) = cache.load().await {
                self.restore_cache(cached);
            }
        }
        self.emit_snapshot().await?;
        if !self.drain_commands() {
            self.startup_cleanup().await;
            self.emit_snapshot().await?;
        }
        let mut next_tick = Instant::now();

        loop {
            // Handle any Refresh commands absorbed by drain_commands() during tick()
            if self.pending_refresh {
                self.pending_refresh = false;
                info!("manual refresh requested");
                self.state.throttles.clear();
                self.reload_workflow_from_disk(true, "manual_refresh").await;
                next_tick = Instant::now();
                let _ = self.emit_snapshot().await;
            }

            let next_retry = self.next_retry_deadline();
            let next_deadline = next_retry
                .map(|retry| retry.min(next_tick))
                .unwrap_or(next_tick);
            let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(next_deadline));
            tokio::pin!(sleep);

            tokio::select! {
                biased;

                Some(command) = self.external_command_rx.recv() => {
                    match command {
                        RuntimeCommand::Refresh => {
                            info!("manual refresh requested");
                            self.state.throttles.clear();
                            self.reload_workflow_from_disk(true, "manual_refresh").await;
                            next_tick = Instant::now();
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::Shutdown => {
                            self.abort_all().await;
                            self.emit_snapshot().await?;
                            return Ok(());
                        }
                    }
                }
                Some(message) = self.command_rx.recv() => {
                    self.handle_message(message).await?;
                }
                _ = &mut sleep => {
                    let now = Instant::now();
                    if now >= next_tick {
                        let shutdown = self.tick().await;
                        if shutdown {
                            self.abort_all().await;
                            let _ = self.emit_snapshot().await;
                            return Ok(());
                        }
                        let interval = Duration::from_millis(self.workflow_rx.borrow().config.polling.interval_ms);
                        next_tick = Instant::now() + interval;
                    }
                    self.process_due_retries().await;
                }
            }
        }
    }

    fn claim_issue(&mut self, issue_id: impl Into<String>, state: IssueClaimState) {
        self.state.claim_states.insert(issue_id.into(), state);
    }

    fn release_issue(&mut self, issue_id: &str) {
        self.state.claim_states.remove(issue_id);
    }

    fn is_claimed(&self, issue_id: &str) -> bool {
        self.state.claim_states.contains_key(issue_id)
    }

    fn build_workspace_manager(&self, workflow: &LoadedWorkflow) -> WorkspaceManager {
        WorkspaceManager::new(
            workflow.config.workspace.root.clone(),
            self.provisioner.clone(),
            workflow.config.workspace.checkout_kind,
            workflow.config.workspace.sync_on_reuse,
            workflow.config.workspace.transient_paths.clone(),
            workflow.config.workspace.source_repo_path.clone(),
            workflow.config.workspace.clone_url.clone(),
            workflow.config.workspace.default_branch.clone(),
        )
    }

    fn select_dispatch_agent(
        &mut self,
        issue: &Issue,
        candidate_agents: &[polyphony_core::AgentDefinition],
        saved_context: Option<&AgentContextSnapshot>,
        prefer_alternate_agent: bool,
    ) -> Result<polyphony_core::AgentDefinition, Error> {
        if candidate_agents.is_empty() {
            return Err(Error::Core(CoreError::Adapter(format!(
                "no agent candidates configured for issue `{}`",
                issue.identifier
            ))));
        }

        let ordered_candidates = rotate_agent_candidates(
            candidate_agents,
            saved_context.map(|context| context.agent_name.as_str()),
            prefer_alternate_agent,
        );
        ordered_candidates
            .into_iter()
            .find(|agent| !self.is_throttled(&format!("agent:{}", agent.name)))
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "all candidate agents are throttled for issue `{}`",
                    issue.identifier
                )))
            })
    }

    /// Drain pending external commands. Returns `true` if shutdown was requested.
    /// Refresh commands set `pending_refresh` so the caller can act on them.
    fn drain_commands(&mut self) -> bool {
        loop {
            match self.external_command_rx.try_recv() {
                Ok(RuntimeCommand::Shutdown) => return true,
                Ok(RuntimeCommand::Refresh) => {
                    self.pending_refresh = true;
                }
                Err(_) => return false,
            }
        }
    }

    async fn tick(&mut self) -> bool {
        self.reload_workflow_from_disk(false, "poll_tick").await;

        if self.drain_commands() {
            return true;
        }

        debug!("tick: reconciling running sessions");
        self.state.loading.reconciling = true;
        let _ = self.emit_snapshot().await;
        self.reconcile_running().await;
        self.state.loading.reconciling = false;

        if self.drain_commands() {
            return true;
        }

        debug!("tick: polling budgets");
        self.state.loading.fetching_budgets = true;
        self.poll_budgets().await;
        self.state.loading.fetching_budgets = false;

        if self.drain_commands() {
            return true;
        }

        debug!("tick: refreshing agent catalogs");
        self.state.loading.fetching_models = true;
        self.refresh_agent_catalogs().await;
        self.state.loading.fetching_models = false;

        if self.drain_commands() {
            return true;
        }

        if self.workflow_reload_error().is_some() {
            let _ = self.emit_snapshot().await;
            return false;
        }
        let workflow = self.workflow();
        if let Err(error) = workflow.config.validate() {
            self.push_event(EventScope::Workflow, format!("validation failed: {error}"));
            error!(%error, "workflow validation failed");
            let _ = self.emit_snapshot().await;
            return false;
        }
        if self.is_throttled(&self.tracker.component_key())
            || self.is_throttled(&self.agent.component_key())
        {
            self.push_event(
                EventScope::Throttle,
                "dispatch skipped while a component is throttled".into(),
            );
            info!("tick: skipped — tracker or agent is throttled");
            let _ = self.emit_snapshot().await;
            return false;
        }
        let query = workflow.config.tracker_query();
        self.state.last_tracker_poll_at = Some(Utc::now());
        self.state.loading.fetching_issues = true;
        info!("tick: fetching issues from tracker");
        let _ = self.emit_snapshot().await;
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.state.loading.fetching_issues = false;
                warn!("tick: tracker returned rate-limited, re-throttling");
                self.register_throttle(*signal);
                let _ = self.emit_snapshot().await;
                return false;
            },
            Err(error) => {
                self.state.loading.fetching_issues = false;
                self.push_event(EventScope::Tracker, format!("candidate fetch failed: {error}"));
                error!(%error, "candidate fetch failed");
                let _ = self.emit_snapshot().await;
                return false;
            },
        };
        self.state.loading.fetching_issues = false;
        self.state.from_cache = false;
        self.state.cached_at = None;
        info!(count = issues.len(), "tick: fetched issues from tracker");

        let mut issues = issues;
        issues.sort_by(dispatch_order);
        self.state.visible_issues = issues.iter().map(summarize_issue).collect();
        self.save_cache().await;
        if !workflow.config.has_dispatch_agents() {
            let _ = self.emit_snapshot().await;
            return false;
        }
        for issue in issues {
            if !self.should_dispatch(&workflow, &issue) {
                continue;
            }
            if !self.has_available_slot(&workflow, &issue.state) {
                break;
            }
            if let Err(error) = self
                .dispatch_issue(workflow.clone(), issue, None, false)
                .await
            {
                self.push_event(EventScope::Dispatch, format!("dispatch failed: {error}"));
                error!(%error, "dispatch failed");
            }
        }
        let _ = self.emit_snapshot().await;
        false
    }

    async fn reconcile_running(&mut self) {
        let workflow = self.workflow();
        let stale_ids = self
            .state
            .running
            .iter()
            .filter_map(|(issue_id, running)| {
                if running.stall_timeout_ms <= 0 {
                    return None;
                }
                let stall_limit = Duration::from_millis(running.stall_timeout_ms as u64);
                let last = running.last_event_at.unwrap_or(running.started_at);
                let elapsed = Utc::now()
                    .signed_duration_since(last)
                    .to_std()
                    .unwrap_or_default();
                if elapsed > stall_limit {
                    Some(issue_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for issue_id in stale_ids {
            self.fail_running(&issue_id, AttemptStatus::Stalled, "stall_timeout")
                .await;
        }

        let running_ids = self.state.running.keys().cloned().collect::<Vec<_>>();
        if running_ids.is_empty() {
            return;
        }
        if self.is_throttled(&self.tracker.component_key()) {
            return;
        }
        let issues = match self.tracker.fetch_issues_by_ids(&running_ids).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                return;
            },
            Err(error) => {
                warn!(%error, "running state refresh failed");
                return;
            },
        };
        let refreshed_ids = issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect::<HashSet<_>>();
        for issue in issues {
            if workflow.config.is_terminal_state(&issue.state) {
                self.stop_running(&issue.id, true).await;
            } else if workflow.config.is_active_state(&issue.state) {
                if let Some(running) = self.state.running.get_mut(&issue.id) {
                    running.issue = issue;
                }
            } else {
                self.stop_running(&issue.id, false).await;
            }
        }
        for missing_issue_id in running_ids
            .into_iter()
            .filter(|issue_id| !refreshed_ids.contains(issue_id))
        {
            self.stop_running(&missing_issue_id, false).await;
            self.push_event(
                EventScope::Reconcile,
                format!("released missing issue {}", missing_issue_id),
            );
        }
    }

    async fn dispatch_issue(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        prefer_alternate_agent: bool,
    ) -> Result<(), Error> {
        let saved_context = self.state.saved_contexts.get(&issue.id).cloned();
        let candidate_agents = workflow.config.candidate_agents_for_issue(&issue)?;
        let selected_agent = self.select_dispatch_agent(
            &issue,
            &candidate_agents,
            saved_context.as_ref(),
            prefer_alternate_agent,
        )?;
        let workspace_manager = self.build_workspace_manager(&workflow);
        let workspace = workspace_manager
            .ensure_workspace(
                &issue.identifier,
                issue.branch_name.clone().or_else(|| {
                    Some(format!(
                        "task/{}",
                        sanitize_workspace_key(&issue.identifier)
                    ))
                }),
                &workflow.config.hooks,
            )
            .await?;
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let issue_identifier_for_task = issue_identifier.clone();
        let issue_for_task = issue.clone();
        let command_tx = self.command_tx.clone();
        let agent = self.agent.clone();
        let tracker = self.tracker.clone();
        let provisioner = self.provisioner.clone();
        let hooks = workflow.config.hooks.clone();
        let active_states = workflow.config.tracker.active_states.clone();
        let max_turns = workflow.config.agent.max_turns;
        let prompt = append_saved_context(
            render_turn_prompt(&workflow.definition, &issue, attempt, 1, max_turns)?,
            saved_context.as_ref(),
            attempt.is_some()
                || saved_context
                    .as_ref()
                    .is_some_and(|context| context.agent_name != selected_agent.name),
        );
        let workspace_path = workspace.path.clone();
        let started_at = Utc::now();
        let selected_agent_for_task = selected_agent.clone();
        if saved_context
            .as_ref()
            .is_some_and(|context| context.agent_name != selected_agent.name)
        {
            self.push_event(
                EventScope::Handoff,
                format!(
                    "{} switched from {} to {}",
                    issue.identifier,
                    saved_context
                        .as_ref()
                        .map(|context| context.agent_name.as_str())
                        .unwrap_or("unknown"),
                    selected_agent.name
                ),
            );
        }
        if let Err(error) = self.tracker.ensure_issue_workflow_tracking(&issue).await {
            warn!(%error, issue_identifier = %issue.identifier, "issue workflow tracking setup failed");
        }
        if let Err(error) = self
            .tracker
            .update_issue_workflow_status(&issue, "In Progress")
            .await
        {
            warn!(%error, issue_identifier = %issue.identifier, "issue workflow status sync failed");
        }
        info!(
            issue_identifier = %issue.identifier,
            agent = %selected_agent.name,
            attempt = attempt.unwrap_or(0),
            workspace_path = %workspace.path.display(),
            "dispatching issue to agent"
        );

        let worker_span = info_span!(
            "issue_worker",
            issue_identifier = %issue_identifier_for_task,
            agent = %selected_agent_for_task.name,
            attempt = attempt.unwrap_or(0)
        );
        let handle = tokio::spawn(
            async move {
                let manager = WorkspaceManager::new(
                    workflow.config.workspace.root.clone(),
                    provisioner,
                    workflow.config.workspace.checkout_kind,
                    workflow.config.workspace.sync_on_reuse,
                    workflow.config.workspace.transient_paths.clone(),
                    workflow.config.workspace.source_repo_path.clone(),
                    workflow.config.workspace.clone_url.clone(),
                    workflow.config.workspace.default_branch.clone(),
                );
                let outcome = run_worker_attempt(
                    &manager,
                    &hooks,
                    agent,
                    tracker,
                    issue_for_task,
                    attempt,
                    workspace_path.clone(),
                    prompt,
                    active_states,
                    max_turns,
                    workflow.config.agent.continuation_prompt.clone(),
                    selected_agent_for_task,
                    saved_context,
                    command_tx.clone(),
                )
                .await;
                let outcome = match outcome {
                    Ok(result) => result,
                    Err(error) => agent_run_result_from_error(&error),
                };
                let _ = command_tx.send(OrchestratorMessage::WorkerFinished {
                    issue_id,
                    issue_identifier: issue_identifier_for_task,
                    attempt,
                    started_at,
                    outcome,
                });
            }
            .instrument(worker_span),
        );

        self.claim_issue(issue.id.clone(), IssueClaimState::Running);
        self.state.retrying.remove(&issue.id);
        self.state.running.insert(issue.id.clone(), RunningTask {
            issue,
            agent_name: selected_agent.name.clone(),
            model: selected_agent
                .model
                .clone()
                .or_else(|| {
                    self.state
                        .agent_catalogs
                        .get(&selected_agent.name)
                        .and_then(|catalog| catalog.selected_model.clone())
                })
                .or_else(|| selected_agent.models.first().cloned()),
            attempt,
            workspace_path: workspace.path,
            stall_timeout_ms: selected_agent.stall_timeout_ms,
            max_turns,
            started_at,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("dispatch_started".into()),
            last_message: Some("worker launched".into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle,
        });
        self.push_event(EventScope::Dispatch, format!("dispatched {issue_identifier}"));
        Ok(())
    }

    async fn process_due_retries(&mut self) {
        if self.workflow_reload_error().is_some() {
            return;
        }
        if self.workflow().config.validate().is_err() {
            return;
        }
        let due_ids = self
            .state
            .retrying
            .iter()
            .filter_map(|(issue_id, entry)| {
                if entry.due_at <= Instant::now() {
                    Some(issue_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for issue_id in due_ids {
            self.handle_retry(issue_id).await;
        }
        let _ = self.emit_snapshot().await;
    }

    async fn handle_retry(&mut self, issue_id: String) {
        let Some(retry) = self.state.retrying.remove(&issue_id) else {
            return;
        };
        let workflow = self.workflow();
        if !workflow.config.has_dispatch_agents() {
            return;
        }
        let query = workflow.config.tracker_query();
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                return;
            },
            Err(error) => {
                self.schedule_retry(
                    issue_id,
                    retry.row.issue_identifier,
                    retry.row.attempt + 1,
                    Some(format!("retry poll failed: {error}")),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
                return;
            },
        };
        let Some(issue) = issues
            .into_iter()
            .find(|issue| issue.id == retry.row.issue_id)
        else {
            self.release_issue(&issue_id);
            return;
        };
        if !self.has_available_slot(&workflow, &issue.state) {
            self.schedule_retry(
                issue.id.clone(),
                issue.identifier.clone(),
                retry.row.attempt + 1,
                Some("no available orchestrator slots".into()),
                false,
                workflow.config.agent.max_retry_backoff_ms,
            );
            return;
        }
        if let Err(error) = self
            .dispatch_issue(
                workflow.clone(),
                issue,
                Some(retry.row.attempt),
                retry.row.error.is_some(),
            )
            .await
        {
            self.schedule_retry(
                retry.row.issue_id,
                retry.row.issue_identifier,
                retry.row.attempt + 1,
                Some(error.to_string()),
                false,
                workflow.config.agent.max_retry_backoff_ms,
            );
        }
    }

    async fn handle_message(&mut self, message: OrchestratorMessage) -> Result<(), Error> {
        match message {
            OrchestratorMessage::AgentEvent(event) => {
                let mut running_model = None;
                if let Some(running) = self.state.running.get_mut(&event.issue_id) {
                    running.session_id = event
                        .session_id
                        .clone()
                        .or_else(|| running.session_id.clone());
                    running.thread_id = event
                        .thread_id
                        .clone()
                        .or_else(|| running.thread_id.clone());
                    running.turn_id = event.turn_id.clone().or_else(|| running.turn_id.clone());
                    running.codex_app_server_pid = event
                        .codex_app_server_pid
                        .clone()
                        .or_else(|| running.codex_app_server_pid.clone());
                    running.last_event = Some(format!("{:?}", event.kind));
                    running.last_message = event.message.clone();
                    running.last_event_at = Some(event.at);
                    if matches!(event.kind, AgentEventKind::TurnStarted) {
                        running.turn_count += 1;
                    }
                    if let Some(usage) = event.usage.clone() {
                        apply_usage_delta(&mut self.state.totals, running, usage);
                    }
                    if let Some(rate_limits) = event.rate_limits.clone() {
                        running.rate_limits = Some(rate_limits.clone());
                        self.state.rate_limits = Some(rate_limits);
                    }
                    running_model = running.model.clone();
                }
                self.update_saved_context_from_event(&event, running_model);
                self.push_event(
                    EventScope::Agent,
                    format!(
                        "{} {}",
                        event.issue_identifier,
                        event.message.unwrap_or_else(|| format!("{:?}", event.kind))
                    ),
                );
                self.emit_snapshot().await?;
            },
            OrchestratorMessage::RateLimited(signal) => {
                self.register_throttle(signal);
                self.emit_snapshot().await?;
            },
            OrchestratorMessage::WorkerFinished {
                issue_id,
                issue_identifier,
                attempt,
                started_at,
                outcome,
            } => {
                self.finish_running(issue_id, issue_identifier, attempt, started_at, outcome)
                    .await?;
            },
        }
        Ok(())
    }

    async fn finish_running(
        &mut self,
        issue_id: String,
        issue_identifier: String,
        attempt: Option<u32>,
        started_at: DateTime<Utc>,
        outcome: AgentRunResult,
    ) -> Result<(), Error> {
        let Some(running) = self.state.running.remove(&issue_id) else {
            return Ok(());
        };
        self.state.ended_runtime_seconds += Utc::now()
            .signed_duration_since(started_at)
            .to_std()
            .unwrap_or_default()
            .as_secs_f64();
        self.state.totals.seconds_running = self.state.ended_runtime_seconds;

        if let Some(store) = &self.store {
            let mut details = std::collections::BTreeMap::new();
            if let Some(error) = outcome.error.clone() {
                details.insert("error".into(), Value::String(error));
            }
            if let Some(context) = self.state.saved_contexts.get(&issue_id) {
                details.insert(
                    "saved_context".into(),
                    serde_json::to_value(context).unwrap_or(Value::Null),
                );
            }
            store
                .record_run(&PersistedRunRecord {
                    issue_id: issue_id.clone(),
                    issue_identifier: issue_identifier.clone(),
                    session_id: running.session_id.clone(),
                    thread_id: running.thread_id.clone(),
                    turn_id: running.turn_id.clone(),
                    codex_app_server_pid: running.codex_app_server_pid.clone(),
                    status: outcome.status,
                    attempt,
                    started_at,
                    finished_at: Some(Utc::now()),
                    details,
                })
                .await?;
        }

        let workflow = self.workflow();
        match outcome.status {
            AttemptStatus::Succeeded => {
                let workflow_status = outcome
                    .final_issue_state
                    .clone()
                    .unwrap_or_else(|| "Human Review".into());
                if !workflow.config.is_active_state(&workflow_status) {
                    if let Err(error) = self
                        .tracker
                        .update_issue_workflow_status(&running.issue, &workflow_status)
                        .await
                    {
                        warn!(%error, issue_identifier = %running.issue.identifier, "issue workflow status sync failed");
                    }
                    if let Err(error) = self.run_success_handoff(&workflow, &running).await {
                        warn!(%error, issue_identifier = %running.issue.identifier, "post-run handoff failed");
                        self.push_event(
                            EventScope::Handoff,
                            format!("{} handoff failed: {}", running.issue.identifier, error),
                        );
                    }
                }
                self.state.completed.insert(issue_id.clone());
                self.schedule_retry(
                    issue_id.clone(),
                    issue_identifier.clone(),
                    1,
                    None,
                    true,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            },
            AttemptStatus::CancelledByReconciliation => {
                self.release_issue(&issue_id);
            },
            _ => {
                self.schedule_retry(
                    issue_id.clone(),
                    issue_identifier.clone(),
                    attempt.unwrap_or(0) + 1,
                    outcome.error.clone(),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            },
        }
        self.finalize_saved_context(&issue_id, &issue_identifier, &running, &outcome);
        self.push_event(
            EventScope::Worker,
            format!("{} {:?}", issue_identifier, outcome.status),
        );
        self.emit_snapshot().await?;
        Ok(())
    }

    async fn startup_cleanup(&mut self) {
        let workflow = self.workflow();
        let terminal = workflow.config.tracker.terminal_states.clone();
        let issues = match self
            .tracker
            .fetch_issues_by_states(workflow.config.tracker.project_slug.as_deref(), &terminal)
            .await
        {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                return;
            },
            Err(error) => {
                warn!(%error, "startup terminal cleanup skipped");
                return;
            },
        };
        let manager = self.build_workspace_manager(&workflow);
        for issue in issues {
            if let Err(error) = manager
                .cleanup_workspace(
                    &issue.identifier,
                    issue.branch_name.clone(),
                    &workflow.config.hooks,
                )
                .await
            {
                warn!(%error, issue_identifier = %issue.identifier, "terminal cleanup failed");
            }
        }
    }

    async fn stop_running(&mut self, issue_id: &str, cleanup_workspace: bool) {
        let workflow = self.workflow();
        if let Some(running) = self.state.running.remove(issue_id) {
            running.handle.abort();
            self.release_issue(issue_id);
            if cleanup_workspace {
                let manager = self.build_workspace_manager(&workflow);
                if let Err(error) = manager
                    .cleanup_workspace(
                        &running.issue.identifier,
                        running.issue.branch_name.clone(),
                        &workflow.config.hooks,
                    )
                    .await
                {
                    warn!(%error, issue_identifier = %running.issue.identifier, "cleanup failed");
                }
            }
            self.push_event(
                EventScope::Reconcile,
                format!("stopped {}", running.issue.identifier),
            );
        }
    }

    async fn fail_running(&mut self, issue_id: &str, status: AttemptStatus, reason: &str) {
        let Some((issue_identifier, attempt, started_at, turns_completed)) =
            self.state.running.get(issue_id).map(|running| {
                running.handle.abort();
                (
                    running.issue.identifier.clone(),
                    running.attempt,
                    running.started_at,
                    running.turn_count,
                )
            })
        else {
            return;
        };
        let _ = self
            .finish_running(
                issue_id.to_string(),
                issue_identifier,
                attempt,
                started_at,
                AgentRunResult {
                    status,
                    turns_completed,
                    error: Some(reason.to_string()),
                    final_issue_state: None,
                },
            )
            .await;
    }

    async fn abort_all(&mut self) {
        let running_ids = self.state.running.keys().cloned().collect::<Vec<_>>();
        for issue_id in running_ids {
            self.stop_running(&issue_id, false).await;
        }
    }

    fn should_dispatch(&self, workflow: &LoadedWorkflow, issue: &Issue) -> bool {
        if issue.id.is_empty()
            || issue.identifier.is_empty()
            || issue.title.is_empty()
            || issue.state.is_empty()
        {
            return false;
        }
        if self.state.running.contains_key(&issue.id) || self.is_claimed(&issue.id) {
            return false;
        }
        let state = issue.normalized_state();
        if !workflow.config.is_active_state(&issue.state)
            || workflow.config.is_terminal_state(&issue.state)
        {
            return false;
        }
        if state == "todo" {
            for blocker in &issue.blocked_by {
                let blocker_state = blocker
                    .state
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !blocker_state.is_empty() && !workflow.config.is_terminal_state(&blocker_state) {
                    return false;
                }
            }
        }
        true
    }

    fn has_available_slot(&self, workflow: &LoadedWorkflow, state: &str) -> bool {
        if self.state.running.len() >= workflow.config.agent.max_concurrent_agents {
            return false;
        }
        let normalized = state.to_ascii_lowercase();
        if let Some(limit) = workflow.config.state_concurrency_limit(state) {
            let count = self
                .state
                .running
                .values()
                .filter(|entry| entry.issue.normalized_state() == normalized)
                .count();
            count < limit
        } else {
            true
        }
    }

    fn schedule_retry(
        &mut self,
        issue_id: String,
        issue_identifier: String,
        attempt: u32,
        error: Option<String>,
        continuation: bool,
        max_retry_backoff_ms: u64,
    ) {
        let delay_ms = if continuation {
            1_000
        } else {
            let exponent = attempt.saturating_sub(1).min(10);
            let delay = 10_000u64.saturating_mul(2u64.saturating_pow(exponent));
            delay.min(max_retry_backoff_ms)
        };
        self.claim_issue(issue_id.clone(), IssueClaimState::RetryQueued);
        self.state.retrying.insert(issue_id.clone(), RetryEntry {
            row: RetryRow {
                issue_id,
                issue_identifier: issue_identifier.clone(),
                attempt,
                due_at: Utc::now() + chrono::Duration::milliseconds(delay_ms as i64),
                error: error.clone(),
            },
            due_at: Instant::now() + Duration::from_millis(delay_ms),
        });
        self.push_event(
            EventScope::Retry,
            format!(
                "{} retry attempt={} delay_ms={} {}",
                issue_identifier,
                attempt,
                delay_ms,
                error.unwrap_or_default()
            ),
        );
    }

    fn next_retry_deadline(&self) -> Option<Instant> {
        self.state.retrying.values().map(|entry| entry.due_at).min()
    }

    fn push_event(&mut self, scope: EventScope, message: String) {
        self.state.recent_events.push_front(RuntimeEvent {
            at: Utc::now(),
            scope,
            message,
        });
        while self.state.recent_events.len() > 100 {
            self.state.recent_events.pop_back();
        }
    }

    async fn emit_snapshot(&mut self) -> Result<(), Error> {
        let snapshot = self.snapshot();
        let _ = self.snapshot_tx.send(snapshot.clone());
        if let Some(store) = &self.store {
            store.save_snapshot(&snapshot).await?;
        }
        Ok(())
    }

    fn snapshot(&self) -> RuntimeSnapshot {
        let live_seconds: f64 = self
            .state
            .running
            .values()
            .map(|running| {
                Utc::now()
                    .signed_duration_since(running.started_at)
                    .to_std()
                    .unwrap_or_default()
                    .as_secs_f64()
            })
            .sum();
        let mut totals = self.state.totals.clone();
        totals.seconds_running = self.state.ended_runtime_seconds + live_seconds;
        RuntimeSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts {
                running: self.state.running.len(),
                retrying: self.state.retrying.len(),
                ..Default::default()
            },
            cadence: RuntimeCadence {
                tracker_poll_interval_ms: self.workflow_rx.borrow().config.polling.interval_ms,
                budget_poll_interval_ms: 60_000,
                model_discovery_interval_ms: 300_000,
                last_tracker_poll_at: self.state.last_tracker_poll_at,
                last_budget_poll_at: self.state.last_budget_poll_at,
                last_model_discovery_at: self.state.last_model_discovery_at,
            },
            visible_issues: self.state.visible_issues.clone(),
            running: self
                .state
                .running
                .values()
                .map(|running| RunningRow {
                    issue_id: running.issue.id.clone(),
                    issue_identifier: running.issue.identifier.clone(),
                    agent_name: running.agent_name.clone(),
                    model: running.model.clone(),
                    state: running.issue.state.clone(),
                    max_turns: running.max_turns,
                    session_id: running.session_id.clone(),
                    thread_id: running.thread_id.clone(),
                    turn_id: running.turn_id.clone(),
                    codex_app_server_pid: running.codex_app_server_pid.clone(),
                    turn_count: running.turn_count,
                    last_event: running.last_event.clone(),
                    last_message: running.last_message.clone(),
                    started_at: running.started_at,
                    last_event_at: running.last_event_at,
                    tokens: running.tokens.clone(),
                    workspace_path: running.workspace_path.clone(),
                    attempt: running.attempt,
                })
                .collect(),
            retrying: self
                .state
                .retrying
                .values()
                .map(|entry| entry.row.clone())
                .collect(),
            codex_totals: totals,
            rate_limits: self.state.rate_limits.clone(),
            throttles: self
                .state
                .throttles
                .values()
                .map(|entry| entry.window.clone())
                .collect(),
            budgets: self.state.budgets.values().cloned().collect(),
            agent_catalogs: self.state.agent_catalogs.values().cloned().collect(),
            saved_contexts: self.state.saved_contexts.values().cloned().collect(),
            recent_events: self.state.recent_events.iter().cloned().collect(),
            movements: Vec::new(),
            tasks: Vec::new(),
            loading: self.state.loading.clone(),
            from_cache: self.state.from_cache,
            cached_at: self.state.cached_at,
        }
    }

    fn restore_cache(&mut self, cached: CachedSnapshot) {
        if self.state.visible_issues.is_empty() {
            self.state.visible_issues = cached.visible_issues;
        }
        if self.state.budgets.is_empty() {
            for budget in cached.budgets {
                self.state.budgets.insert(budget.component.clone(), budget);
            }
        }
        if self.state.agent_catalogs.is_empty() {
            for catalog in cached.agent_catalogs {
                self.state
                    .agent_catalogs
                    .insert(catalog.agent_name.clone(), catalog);
            }
        }
        self.state.from_cache = true;
        self.state.cached_at = cached.saved_at;
    }

    async fn save_cache(&self) {
        if let Some(cache) = &self.cache {
            let cached = CachedSnapshot {
                saved_at: Some(Utc::now()),
                visible_issues: self.state.visible_issues.clone(),
                budgets: self.state.budgets.values().cloned().collect(),
                agent_catalogs: self.state.agent_catalogs.values().cloned().collect(),
            };
            if let Err(e) = cache.save(&cached).await {
                warn!(%e, "cache save failed");
            }
        }
    }

    fn restore_bootstrap(&mut self, bootstrap: polyphony_core::StoreBootstrap) {
        self.state.recent_events = bootstrap.recent_events.into_iter().collect();
        self.state.budgets = bootstrap.budgets;
        self.state.saved_contexts = bootstrap.saved_contexts;
        self.state.throttles = bootstrap
            .throttles
            .into_iter()
            .map(|(component, window)| {
                let due_at = window
                    .until
                    .signed_duration_since(Utc::now())
                    .to_std()
                    .map(|delta| Instant::now() + delta)
                    .unwrap_or_else(|_| Instant::now());
                (component, ActiveThrottle { window, due_at })
            })
            .collect();
        for (issue_id, row) in bootstrap.retrying {
            let due_at = row
                .due_at
                .signed_duration_since(Utc::now())
                .to_std()
                .map(|delta| Instant::now() + delta)
                .unwrap_or_else(|_| Instant::now());
            self.claim_issue(issue_id.clone(), IssueClaimState::RetryQueued);
            self.state
                .retrying
                .insert(issue_id, RetryEntry { row, due_at });
        }
    }

    fn register_throttle(&mut self, signal: RateLimitSignal) {
        let until = signal
            .reset_at
            .or_else(|| {
                signal
                    .retry_after_ms
                    .map(|ms| Utc::now() + chrono::Duration::milliseconds(ms as i64))
            })
            .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(60));
        let due_at = until
            .signed_duration_since(Utc::now())
            .to_std()
            .map(|delta| Instant::now() + delta)
            .unwrap_or_else(|_| Instant::now() + Duration::from_secs(1));
        self.state
            .throttles
            .insert(signal.component.clone(), ActiveThrottle {
                window: ThrottleWindow {
                    component: signal.component.clone(),
                    until,
                    reason: signal.reason.clone(),
                },
                due_at,
            });
        self.push_event(
            EventScope::Throttle,
            format!(
                "{} limited until {} ({})",
                signal.component, until, signal.reason
            ),
        );
    }

    fn is_throttled(&mut self, component: &str) -> bool {
        match self.state.throttles.get(component) {
            Some(throttle) if throttle.due_at > Instant::now() => true,
            Some(_) => {
                self.state.throttles.remove(component);
                false
            },
            None => false,
        }
    }

    async fn poll_budgets(&mut self) {
        let due = self
            .state
            .last_budget_poll_at
            .map(|at| Utc::now().signed_duration_since(at).num_seconds() >= 60)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.state.last_budget_poll_at = Some(Utc::now());

        match self.tracker.fetch_budget().await {
            Ok(Some(snapshot)) => self.record_budget(snapshot).await,
            Ok(None) => {},
            Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
            Err(error) => warn!(%error, "tracker budget poll failed"),
        }

        let workflow = self.workflow();
        match self
            .agent
            .fetch_budgets(&workflow.config.all_agents())
            .await
        {
            Ok(snapshots) => {
                for snapshot in snapshots {
                    self.record_budget(snapshot).await;
                }
            },
            Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
            Err(error) => warn!(%error, "agent budget poll failed"),
        }
    }

    async fn refresh_agent_catalogs(&mut self) {
        let due = self
            .state
            .last_model_discovery_at
            .map(|at| Utc::now().signed_duration_since(at).num_seconds() >= 300)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.state.last_model_discovery_at = Some(Utc::now());
        let workflow = self.workflow();
        match self
            .agent
            .discover_models(&workflow.config.all_agents())
            .await
        {
            Ok(catalogs) => {
                self.state.agent_catalogs = catalogs
                    .into_iter()
                    .map(|catalog| (catalog.agent_name.clone(), catalog))
                    .collect();
                for running in self.state.running.values_mut() {
                    if let Some(selected_model) = self
                        .state
                        .agent_catalogs
                        .get(&running.agent_name)
                        .and_then(|catalog| catalog.selected_model.clone())
                    {
                        running.model = Some(selected_model);
                    }
                }
            },
            Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
            Err(error) => warn!(%error, "agent model discovery failed"),
        }
    }

    async fn record_budget(&mut self, snapshot: BudgetSnapshot) {
        self.state
            .budgets
            .insert(snapshot.component.clone(), snapshot.clone());
        if let Some(store) = &self.store
            && let Err(error) = store.record_budget(&snapshot).await
        {
            warn!(%error, "persisting budget snapshot failed");
        }
    }

    fn workflow(&self) -> LoadedWorkflow {
        self.workflow_rx.borrow().clone()
    }

    fn workflow_reload_error(&self) -> Option<&str> {
        self.reload_support
            .as_ref()
            .and_then(|support| support.reload_error.as_deref())
    }

    async fn reload_workflow_from_disk(&mut self, force: bool, reason: &str) {
        let Some(reload_support) = self.reload_support.as_ref() else {
            return;
        };
        let workflow_path = reload_support.workflow_path.clone();
        let user_config_path = reload_support.user_config_path.clone();
        let workflow_tx = reload_support.workflow_tx.clone();
        let component_factory = reload_support.component_factory.clone();
        let last_seen_fingerprint = reload_support.last_seen_fingerprint;

        let fingerprint = match workflow_file_fingerprint(&workflow_path) {
            Ok(fingerprint) => Some(fingerprint),
            Err(error) => {
                self.note_workflow_reload_failure(
                    None,
                    format!("workflow fingerprint failed: {error}"),
                    reason,
                );
                return;
            },
        };
        if !force && fingerprint == last_seen_fingerprint {
            return;
        }

        match load_workflow_with_user_config(&workflow_path, user_config_path.as_deref()) {
            Ok(workflow) => {
                let current_workflow = self.workflow();
                let recovered = self.clear_workflow_reload_error(fingerprint);
                if workflow.definition == current_workflow.definition
                    && workflow.config == current_workflow.config
                {
                    if recovered {
                        self.push_event(
                            EventScope::Workflow,
                            format!("workflow reload recovered via {reason}"),
                        );
                        info!(%reason, "workflow reload recovered without config changes");
                    }
                    return;
                }

                match component_factory(&workflow) {
                    Ok(components) => {
                        self.apply_reloaded_components(&current_workflow, &workflow, components);
                        let _ = workflow_tx.send(workflow);
                        self.push_event(
                            EventScope::Workflow,
                            format!("workflow reloaded via {reason}"),
                        );
                        info!(%reason, path = %workflow_path.display(), "workflow reloaded");
                    },
                    Err(error) => {
                        self.note_workflow_reload_failure(
                            fingerprint,
                            format!("component rebuild failed: {error}"),
                            reason,
                        );
                    },
                }
            },
            Err(error) => {
                self.note_workflow_reload_failure(fingerprint, error.to_string(), reason);
            },
        }
    }

    fn clear_workflow_reload_error(
        &mut self,
        fingerprint: Option<WorkflowFileFingerprint>,
    ) -> bool {
        let Some(reload_support) = self.reload_support.as_mut() else {
            return false;
        };
        if let Some(fingerprint) = fingerprint {
            reload_support.last_seen_fingerprint = Some(fingerprint);
        }
        reload_support.reload_error.take().is_some()
    }

    fn note_workflow_reload_failure(
        &mut self,
        fingerprint: Option<WorkflowFileFingerprint>,
        error: String,
        reason: &str,
    ) {
        let changed = {
            let Some(reload_support) = self.reload_support.as_mut() else {
                return;
            };
            if let Some(fingerprint) = fingerprint {
                reload_support.last_seen_fingerprint = Some(fingerprint);
            }
            let changed = reload_support.reload_error.as_deref() != Some(error.as_str());
            reload_support.reload_error = Some(error.clone());
            changed
        };
        if changed {
            self.push_event(
                EventScope::Workflow,
                format!("workflow reload failed via {reason}: {error}"),
            );
        }
        warn!(%error, %reason, "workflow reload failed; keeping last good config");
    }

    fn apply_reloaded_components(
        &mut self,
        current_workflow: &LoadedWorkflow,
        new_workflow: &LoadedWorkflow,
        components: RuntimeComponents,
    ) {
        let old_tracker_key = self.tracker.component_key();
        let new_tracker_key = components.tracker.component_key();
        let old_agent_runtime_key = self.agent.component_key();
        let new_agent_runtime_key = components.agent.component_key();
        let old_agent_names = current_workflow
            .config
            .all_agents()
            .into_iter()
            .map(|agent| agent.name)
            .collect::<HashSet<_>>();
        let new_agent_names = new_workflow
            .config
            .all_agents()
            .into_iter()
            .map(|agent| agent.name)
            .collect::<HashSet<_>>();
        let removed_agents = old_agent_names
            .difference(&new_agent_names)
            .cloned()
            .collect::<Vec<_>>();
        let affected_agent_budgets = old_agent_names
            .union(&new_agent_names)
            .map(|name| format!("agent:{name}"))
            .collect::<HashSet<_>>();

        self.tracker = components.tracker;
        self.agent = components.agent;
        self.committer = components.committer;
        self.pull_request_manager = components.pull_request_manager;
        self.pull_request_commenter = components.pull_request_commenter;
        self.feedback = components.feedback;

        if old_tracker_key != new_tracker_key {
            self.state.throttles.remove(&old_tracker_key);
            self.state.budgets.remove(&old_tracker_key);
        }
        if old_agent_runtime_key != new_agent_runtime_key {
            self.state.throttles.remove(&old_agent_runtime_key);
        }
        for agent_name in removed_agents {
            self.state.throttles.remove(&format!("agent:{agent_name}"));
            self.state.agent_catalogs.remove(&agent_name);
        }
        self.state
            .budgets
            .retain(|component, _| !affected_agent_budgets.contains(component));
        self.state.agent_catalogs.clear();
        self.state.last_tracker_poll_at = None;
        self.state.last_budget_poll_at = None;
        self.state.last_model_discovery_at = None;
    }

    fn update_saved_context_from_event(&mut self, event: &AgentEvent, model: Option<String>) {
        let context = self
            .state
            .saved_contexts
            .entry(event.issue_id.clone())
            .or_insert_with(|| AgentContextSnapshot {
                issue_id: event.issue_id.clone(),
                issue_identifier: event.issue_identifier.clone(),
                updated_at: event.at,
                agent_name: event.agent_name.clone(),
                model: model.clone(),
                session_id: event.session_id.clone(),
                thread_id: event.thread_id.clone(),
                turn_id: event.turn_id.clone(),
                codex_app_server_pid: event.codex_app_server_pid.clone(),
                status: None,
                error: None,
                usage: event.usage.clone().unwrap_or_default(),
                transcript: Vec::new(),
            });
        context.updated_at = event.at;
        context.agent_name = event.agent_name.clone();
        context.model = model.or_else(|| context.model.clone());
        context.session_id = event
            .session_id
            .clone()
            .or_else(|| context.session_id.clone());
        context.thread_id = event
            .thread_id
            .clone()
            .or_else(|| context.thread_id.clone());
        context.turn_id = event.turn_id.clone().or_else(|| context.turn_id.clone());
        context.codex_app_server_pid = event
            .codex_app_server_pid
            .clone()
            .or_else(|| context.codex_app_server_pid.clone());
        if let Some(usage) = &event.usage {
            context.usage = usage.clone();
        }
        if let Some(message) = event
            .message
            .as_ref()
            .filter(|message| !message.trim().is_empty())
        {
            context.transcript.push(AgentContextEntry {
                at: event.at,
                kind: event.kind,
                message: message.clone(),
            });
            while context.transcript.len() > 40 {
                context.transcript.remove(0);
            }
        }
    }

    fn finalize_saved_context(
        &mut self,
        issue_id: &str,
        issue_identifier: &str,
        running: &RunningTask,
        outcome: &AgentRunResult,
    ) {
        let context = self
            .state
            .saved_contexts
            .entry(issue_id.to_string())
            .or_insert_with(|| AgentContextSnapshot {
                issue_id: issue_id.to_string(),
                issue_identifier: issue_identifier.to_string(),
                updated_at: Utc::now(),
                agent_name: running.agent_name.clone(),
                model: running.model.clone(),
                session_id: running.session_id.clone(),
                thread_id: running.thread_id.clone(),
                turn_id: running.turn_id.clone(),
                codex_app_server_pid: running.codex_app_server_pid.clone(),
                status: None,
                error: None,
                usage: running.tokens.clone(),
                transcript: Vec::new(),
            });
        context.updated_at = Utc::now();
        context.issue_identifier = issue_identifier.to_string();
        context.agent_name = running.agent_name.clone();
        context.model = running.model.clone();
        context.session_id = running.session_id.clone();
        context.thread_id = running.thread_id.clone();
        context.turn_id = running.turn_id.clone();
        context.codex_app_server_pid = running.codex_app_server_pid.clone();
        context.status = Some(outcome.status);
        context.error = outcome.error.clone();
        context.usage = running.tokens.clone();
        if let Some(error) = &outcome.error {
            context.transcript.push(AgentContextEntry {
                at: Utc::now(),
                kind: AgentEventKind::Outcome,
                message: format!("run ended with error: {error}"),
            });
        }
        while context.transcript.len() > 40 {
            context.transcript.remove(0);
        }
    }

    async fn run_success_handoff(
        &mut self,
        workflow: &LoadedWorkflow,
        running: &RunningTask,
    ) -> Result<(), Error> {
        if !workflow.config.automation.enabled {
            return Ok(());
        }
        let committer = self
            .committer
            .as_ref()
            .ok_or_else(|| CoreError::Adapter("workspace committer is not configured".into()))?;
        let pull_request_manager = self
            .pull_request_manager
            .as_ref()
            .ok_or_else(|| CoreError::Adapter("pull request manager is not configured".into()))?;
        let repository = workflow
            .config
            .tracker
            .repository
            .clone()
            .ok_or_else(|| CoreError::Adapter("tracker.repository is required".into()))?;
        let base_branch = workflow
            .config
            .workspace
            .default_branch
            .clone()
            .unwrap_or_else(|| "main".into());
        let branch_name = running.issue.branch_name.clone().unwrap_or_else(|| {
            format!("task/{}", sanitize_workspace_key(&running.issue.identifier))
        });
        let commit_message = render_issue_template_with_strings(
            workflow
                .config
                .automation
                .commit_message
                .as_deref()
                .unwrap_or(DEFAULT_AUTOMATION_COMMIT_MESSAGE),
            &running.issue,
            running.attempt,
            &[
                ("base_branch", base_branch.clone()),
                ("head_branch", branch_name.clone()),
            ],
        )?;
        let commit_result = committer
            .commit_and_push(&WorkspaceCommitRequest {
                workspace_path: running.workspace_path.clone(),
                branch_name: branch_name.clone(),
                commit_message,
                remote_name: workflow.config.automation.git.remote_name.clone(),
                auth_token: workflow.config.tracker.api_key.clone(),
                author_name: workflow.config.automation.git.author.name.clone(),
                author_email: workflow.config.automation.git.author.email.clone(),
            })
            .await?;
        let Some(commit_result) = commit_result else {
            self.push_event(
                EventScope::Handoff,
                format!(
                    "{} handoff skipped because the workspace is clean",
                    running.issue.identifier
                ),
            );
            return Ok(());
        };

        let pr_title = render_issue_template_with_strings(
            workflow
                .config
                .automation
                .pr_title
                .as_deref()
                .unwrap_or(DEFAULT_AUTOMATION_PR_TITLE),
            &running.issue,
            running.attempt,
            &[
                ("base_branch", base_branch.clone()),
                ("head_branch", branch_name.clone()),
                ("commit_sha", commit_result.head_sha.clone()),
            ],
        )?;
        let pr_body = render_issue_template_with_strings(
            workflow
                .config
                .automation
                .pr_body
                .as_deref()
                .unwrap_or(DEFAULT_AUTOMATION_PR_BODY),
            &running.issue,
            running.attempt,
            &[
                ("base_branch", base_branch.clone()),
                ("head_branch", branch_name.clone()),
                ("commit_sha", commit_result.head_sha.clone()),
            ],
        )?;
        let pull_request = pull_request_manager
            .ensure_pull_request(&PullRequestRequest {
                repository,
                head_branch: branch_name.clone(),
                base_branch: base_branch.clone(),
                title: pr_title,
                body: pr_body,
                draft: workflow.config.automation.draft_pull_requests,
            })
            .await?;

        if let Some(review_body) = self
            .run_review_pass(workflow, running, &pull_request)
            .await?
            && let Some(commenter) = &self.pull_request_commenter
        {
            commenter
                .comment_on_pull_request(&pull_request, &review_body)
                .await?;
        }
        self.send_handoff_feedback(workflow, running, &pull_request, &commit_result)
            .await;
        self.push_event(
            EventScope::Handoff,
            format!(
                "{} opened PR #{} on {}",
                running.issue.identifier, pull_request.number, commit_result.branch_name
            ),
        );
        Ok(())
    }

    async fn run_review_pass(
        &self,
        workflow: &LoadedWorkflow,
        running: &RunningTask,
        pull_request: &polyphony_core::PullRequestRef,
    ) -> Result<Option<String>, Error> {
        let review_agent = workflow
            .config
            .review_agent()?
            .or_else(|| {
                workflow
                    .config
                    .all_agents()
                    .into_iter()
                    .find(|agent| agent.name == running.agent_name)
            })
            .ok_or_else(|| CoreError::Adapter("review agent is not available".into()))?;
        let review_path = running.workspace_path.join(".polyphony").join("review.md");
        if let Some(parent) = review_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if tokio::fs::metadata(&review_path).await.is_ok() {
            let _ = tokio::fs::remove_file(&review_path).await;
        }
        let base_branch = workflow
            .config
            .workspace
            .default_branch
            .clone()
            .unwrap_or_else(|| "main".into());
        let prompt = render_issue_template_with_strings(
            workflow
                .config
                .automation
                .review_prompt
                .as_deref()
                .unwrap_or(DEFAULT_AUTOMATION_REVIEW_PROMPT),
            &running.issue,
            running.attempt,
            &[
                ("base_branch", base_branch),
                (
                    "head_branch",
                    running.issue.branch_name.clone().unwrap_or_default(),
                ),
                (
                    "pull_request_url",
                    pull_request.url.clone().unwrap_or_default(),
                ),
            ],
        )?;
        let manager = self.build_workspace_manager(workflow);
        manager
            .run_before_run(&workflow.config.hooks, &running.workspace_path)
            .await?;
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let drain = tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
        let result = self
            .agent
            .run(
                AgentRunSpec {
                    issue: running.issue.clone(),
                    attempt: None,
                    workspace_path: running.workspace_path.clone(),
                    prompt,
                    max_turns: workflow.config.agent.max_turns,
                    agent: review_agent,
                    prior_context: None,
                },
                event_tx,
            )
            .await;
        drain.abort();
        manager
            .run_after_run_best_effort(&workflow.config.hooks, &running.workspace_path)
            .await;
        match result {
            Ok(result) if matches!(result.status, AttemptStatus::Succeeded) => {
                let review = tokio::fs::read_to_string(&review_path).await.ok();
                let _ = tokio::fs::remove_file(&review_path).await;
                Ok(review.and_then(|body| {
                    let trimmed = body.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                }))
            },
            Ok(result) => {
                warn!(
                    issue_identifier = %running.issue.identifier,
                    status = ?result.status,
                    "review pass did not succeed"
                );
                Ok(None)
            },
            Err(error) => {
                warn!(issue_identifier = %running.issue.identifier, %error, "review pass failed");
                Ok(None)
            },
        }
    }

    async fn send_handoff_feedback(
        &mut self,
        workflow: &LoadedWorkflow,
        running: &RunningTask,
        pull_request: &polyphony_core::PullRequestRef,
        commit_result: &polyphony_core::WorkspaceCommitResult,
    ) {
        let Some(feedback) = &self.feedback else {
            return;
        };
        if feedback.is_empty() {
            return;
        }
        let mut links = Vec::new();
        if let Some(url) = &pull_request.url {
            links.push(FeedbackLink {
                label: "Review PR".into(),
                url: url.clone(),
            });
        }
        if let Some(url) = &running.issue.url {
            links.push(FeedbackLink {
                label: "Issue".into(),
                url: url.clone(),
            });
        }
        let notification = FeedbackNotification {
            key: format!("handoff:{}", running.issue.id),
            title: format!("{} ready for review", running.issue.identifier),
            body: format!(
                "{}\n\nBranch: {}\nCommit: {}\nChanged files: {}\nWorkspace: {}",
                running.issue.title,
                commit_result.branch_name,
                commit_result.head_sha,
                commit_result.changed_files,
                running.workspace_path.display()
            ),
            links,
            actions: workflow
                .config
                .feedback
                .action_base_url
                .as_ref()
                .map(|base| {
                    vec![FeedbackAction {
                        id: "review".into(),
                        label: "Open Review".into(),
                        url: pull_request.url.clone().or_else(|| Some(base.clone())),
                    }]
                })
                .unwrap_or_default(),
        };
        for (component, error) in feedback.send_all(&notification).await {
            warn!(%component, %error, "feedback sink failed");
            self.push_event(
                EventScope::Feedback,
                format!(
                    "{} sink {} failed: {}",
                    running.issue.identifier, component, error
                ),
            );
        }
    }
}

pub fn spawn_workflow_watcher(
    workflow_path: PathBuf,
    repo_config_path: Option<PathBuf>,
    runtime_command_tx: mpsc::UnboundedSender<RuntimeCommand>,
) -> Result<JoinHandle<Result<(), Error>>, Error> {
    Ok(tokio::spawn(async move {
        let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
            let _ = notify_tx.send(event);
        })?;
        watcher.watch(&workflow_path, RecursiveMode::NonRecursive)?;
        if let Some(repo_config_path) = repo_config_path.as_ref()
            && repo_config_path.exists()
        {
            watcher.watch(repo_config_path, RecursiveMode::NonRecursive)?;
        }
        while let Some(event) = notify_rx.recv().await {
            match event {
                Ok(_) => {
                    let _ = runtime_command_tx.send(RuntimeCommand::Refresh);
                    info!(path = %workflow_path.display(), "workflow change detected");
                },
                Err(error) => warn!(%error, "workflow watch event failed"),
            }
        }
        Ok(())
    }))
}

fn workflow_file_fingerprint(path: &Path) -> Result<WorkflowFileFingerprint, std::io::Error> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(WorkflowFileFingerprint::Present {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(WorkflowFileFingerprint::Missing)
        },
        Err(error) => Err(error),
    }
}

async fn run_worker_attempt(
    workspace_manager: &WorkspaceManager,
    hooks: &HooksConfig,
    agent: Arc<dyn AgentRuntime>,
    tracker: Arc<dyn IssueTracker>,
    issue: Issue,
    attempt: Option<u32>,
    workspace_path: PathBuf,
    prompt: String,
    active_states: Vec<String>,
    max_turns: u32,
    continuation_prompt_template: Option<String>,
    selected_agent: polyphony_core::AgentDefinition,
    saved_context: Option<AgentContextSnapshot>,
    command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
) -> Result<AgentRunResult, Error> {
    info!(
        issue_identifier = %issue.identifier,
        agent = %selected_agent.name,
        attempt = attempt.unwrap_or(0),
        max_turns,
        "starting worker attempt"
    );
    workspace_manager
        .run_before_run(hooks, &workspace_path)
        .await?;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let issue_id = issue.id.clone();
    let forward_command_tx = command_tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let _ = forward_command_tx.send(OrchestratorMessage::AgentEvent(event));
        }
    });
    let run_spec = AgentRunSpec {
        issue: issue.clone(),
        attempt,
        workspace_path: workspace_path.clone(),
        prompt: prompt.clone(),
        max_turns,
        agent: selected_agent,
        prior_context: saved_context,
    };
    let result = if let Some(mut session) = agent
        .start_session(run_spec.clone(), event_tx.clone())
        .await?
    {
        info!(
            issue_identifier = %run_spec.issue.identifier,
            agent = %run_spec.agent.name,
            "using live agent session"
        );
        let mut current_issue = issue;
        let mut current_prompt = prompt;
        let mut total_turns = 0;
        let mut turn_number = 1;
        let run_result = loop {
            info!(
                issue_identifier = %current_issue.identifier,
                turn_number,
                "starting live agent turn"
            );
            let turn_result = session.run_turn(current_prompt).await?;
            total_turns += turn_result.turns_completed;
            if !matches!(turn_result.status, AttemptStatus::Succeeded) {
                info!(
                    issue_identifier = %current_issue.identifier,
                    turn_number,
                    status = ?turn_result.status,
                    "live agent turn ended without success"
                );
                break Ok(AgentRunResult {
                    status: turn_result.status,
                    turns_completed: total_turns,
                    error: turn_result.error,
                    final_issue_state: turn_result.final_issue_state,
                });
            }

            let state_updates = tracker
                .fetch_issue_states_by_ids(&[current_issue.id.clone()])
                .await?;
            if let Some(updated_issue) = state_updates
                .into_iter()
                .find(|updated_issue| updated_issue.id == current_issue.id)
            {
                current_issue.state = updated_issue.state;
                current_issue.updated_at = updated_issue.updated_at;
            }
            debug!(
                issue_identifier = %current_issue.identifier,
                turn_number,
                state = %current_issue.state,
                "refreshed issue state after live turn"
            );

            if turn_number >= max_turns || !is_active_state(&active_states, &current_issue.state) {
                info!(
                    issue_identifier = %current_issue.identifier,
                    turn_number,
                    total_turns,
                    state = %current_issue.state,
                    "stopping live agent session"
                );
                break Ok(AgentRunResult {
                    status: AttemptStatus::Succeeded,
                    turns_completed: total_turns,
                    error: None,
                    final_issue_state: Some(current_issue.state.clone()),
                });
            }

            turn_number += 1;
            info!(
                issue_identifier = %current_issue.identifier,
                turn_number,
                state = %current_issue.state,
                "continuing live agent session"
            );
            current_prompt = build_continuation_prompt(
                &current_issue,
                attempt,
                turn_number,
                max_turns,
                continuation_prompt_template.as_deref(),
            )
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        };
        let stop_result = session.stop().await;
        match (run_result, stop_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(result), Ok(())) => Ok(result),
        }
    } else {
        info!(
            issue_identifier = %run_spec.issue.identifier,
            agent = %run_spec.agent.name,
            "provider does not support live sessions, falling back to single run"
        );
        agent.run(run_spec, event_tx).await
    };
    forwarder.abort();
    workspace_manager
        .run_after_run_best_effort(hooks, &workspace_path)
        .await;
    match result {
        Ok(result) => Ok(result),
        Err(CoreError::RateLimited(signal)) => {
            let _ = command_tx.send(OrchestratorMessage::RateLimited(signal.as_ref().clone()));
            warn!(issue_id = %issue_id, "worker attempt hit provider rate limit");
            Err(Error::Core(CoreError::RateLimited(signal)))
        },
        Err(error) => {
            warn!(issue_id = %issue_id, %error, "worker attempt failed");
            Err(Error::Core(error))
        },
    }
}

fn is_active_state(active_states: &[String], state: &str) -> bool {
    active_states
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(state))
}

fn build_continuation_prompt(
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
    template: Option<&str>,
) -> Result<String, polyphony_workflow::Error> {
    let source = template.unwrap_or(
        "Continue working on issue {{ issue.identifier }}: {{ issue.title }}.\n\
You are continuing the same live agent thread in the current workspace.\n\
Do not restart from scratch or repeat the original prompt.\n\
Current tracker state: {{ issue.state }}.\n\
This is continuation turn {{ turn_number }} of {{ max_turns }}.\n\
If the work is complete or blocked, say so explicitly. Otherwise continue with the next concrete steps.",
    );
    render_turn_template(source, issue, attempt, turn_number, max_turns)
}

fn agent_run_result_from_error(error: &Error) -> AgentRunResult {
    AgentRunResult {
        status: attempt_status_from_error(error),
        turns_completed: 0,
        error: Some(normalized_worker_error_message(error)),
        final_issue_state: None,
    }
}

fn attempt_status_from_error(error: &Error) -> AttemptStatus {
    match error {
        Error::Core(CoreError::Adapter(message))
            if matches!(message.as_str(), "response_timeout" | "turn_timeout") =>
        {
            AttemptStatus::TimedOut
        },
        _ => AttemptStatus::Failed,
    }
}

fn normalized_worker_error_message(error: &Error) -> String {
    match error {
        Error::Core(CoreError::Adapter(message)) => message.clone(),
        Error::Core(CoreError::RateLimited(signal)) => {
            format!("rate_limited: {}", signal.reason)
        },
        _ => error.to_string(),
    }
}

fn apply_usage_delta(totals: &mut CodexTotals, running: &mut RunningTask, usage: TokenUsage) {
    let delta_input = usage
        .input_tokens
        .saturating_sub(running.last_reported_tokens.input_tokens);
    let delta_output = usage
        .output_tokens
        .saturating_sub(running.last_reported_tokens.output_tokens);
    let delta_total = usage
        .total_tokens
        .saturating_sub(running.last_reported_tokens.total_tokens);
    totals.input_tokens += delta_input;
    totals.output_tokens += delta_output;
    totals.total_tokens += delta_total;
    running.tokens = usage.clone();
    running.last_reported_tokens = usage;
}

fn dispatch_order(left: &Issue, right: &Issue) -> std::cmp::Ordering {
    left.priority
        .unwrap_or(i32::MAX)
        .cmp(&right.priority.unwrap_or(i32::MAX))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.identifier.cmp(&right.identifier))
}

fn empty_snapshot() -> RuntimeSnapshot {
    RuntimeSnapshot {
        generated_at: Utc::now(),
        counts: SnapshotCounts::default(),
        cadence: RuntimeCadence::default(),
        visible_issues: Vec::new(),
        running: Vec::new(),
        retrying: Vec::new(),
        codex_totals: CodexTotals::default(),
        rate_limits: None,
        throttles: Vec::new(),
        budgets: Vec::new(),
        agent_catalogs: Vec::new(),
        saved_contexts: Vec::new(),
        recent_events: Vec::new(),
        movements: Vec::new(),
        tasks: Vec::new(),
        loading: LoadingState::default(),
        from_cache: false,
        cached_at: None,
    }
}

fn summarize_issue(issue: &Issue) -> VisibleIssueRow {
    VisibleIssueRow {
        issue_id: issue.id.clone(),
        issue_identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        state: issue.state.clone(),
        priority: issue.priority,
        labels: issue.labels.clone(),
        description: issue.description.clone(),
        url: issue.url.clone(),
        author: issue
            .author
            .as_ref()
            .and_then(|a| a.username.clone().or(a.display_name.clone())),
        updated_at: issue.updated_at,
        created_at: issue.created_at,
    }
}

fn append_saved_context(
    prompt: String,
    saved_context: Option<&AgentContextSnapshot>,
    include: bool,
) -> String {
    if !include {
        return prompt;
    }
    let Some(saved_context) = saved_context else {
        return prompt;
    };
    let mut result = prompt;
    result.push_str("\n\n## Saved Polyphony Context\n");
    result.push_str(&format!(
        "Last agent: {}{}\n",
        saved_context.agent_name,
        saved_context
            .model
            .as_ref()
            .map(|model| format!(" ({model})"))
            .unwrap_or_default()
    ));
    if let Some(status) = &saved_context.status {
        result.push_str(&format!("Last status: {status}\n"));
    }
    if let Some(error) = &saved_context.error {
        result.push_str(&format!("Last error: {error}\n"));
    }
    result.push_str("Recent transcript:\n");
    for entry in saved_context.transcript.iter().rev().take(12).rev() {
        result.push_str(&format!(
            "- [{:?}] {}: {}\n",
            entry.kind,
            entry.at.to_rfc3339(),
            entry.message
        ));
    }
    result
}

fn rotate_agent_candidates(
    candidate_agents: &[polyphony_core::AgentDefinition],
    previous_agent_name: Option<&str>,
    prefer_alternate_agent: bool,
) -> Vec<polyphony_core::AgentDefinition> {
    if !prefer_alternate_agent {
        return candidate_agents.to_vec();
    }
    let Some(previous_agent_name) = previous_agent_name else {
        return candidate_agents.to_vec();
    };
    let Some(previous_index) = candidate_agents
        .iter()
        .position(|agent| agent.name == previous_agent_name)
    else {
        return candidate_agents.to_vec();
    };
    if candidate_agents.len() <= 1 {
        return candidate_agents.to_vec();
    }

    candidate_agents[previous_index + 1..]
        .iter()
        .chain(candidate_agents[..=previous_index].iter())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::{
            collections::VecDeque,
            fs,
            path::Path,
            sync::{Arc, Mutex},
        },
    };

    use {
        async_trait::async_trait,
        polyphony_core::{
            AgentSession, IssueAuthor, IssueComment, IssueStateUpdate, Workspace, WorkspaceRequest,
        },
        polyphony_workflow::load_workflow,
        tokio::sync::watch,
    };

    #[derive(Clone)]
    struct TestTracker {
        issues: Arc<Mutex<HashMap<String, Issue>>>,
        workflow_updates: Arc<Mutex<Vec<String>>>,
    }

    impl TestTracker {
        fn new(issues: Vec<Issue>) -> Self {
            Self {
                issues: Arc::new(Mutex::new(
                    issues
                        .into_iter()
                        .map(|issue| (issue.id.clone(), issue))
                        .collect(),
                )),
                workflow_updates: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn recorded_workflow_updates(&self) -> Vec<String> {
            self.workflow_updates.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl IssueTracker for TestTracker {
        fn component_key(&self) -> String {
            "tracker:test".into()
        }

        async fn fetch_candidate_issues(
            &self,
            _query: &polyphony_core::TrackerQuery,
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            Ok(self.issues.lock().unwrap().values().cloned().collect())
        }

        async fn fetch_issues_by_states(
            &self,
            _project_slug: Option<&str>,
            states: &[String],
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            let normalized = states
                .iter()
                .map(|state| state.to_ascii_lowercase())
                .collect::<Vec<_>>();
            Ok(self
                .issues
                .lock()
                .unwrap()
                .values()
                .filter(|issue| normalized.contains(&issue.state.to_ascii_lowercase()))
                .cloned()
                .collect())
        }

        async fn fetch_issues_by_ids(
            &self,
            issue_ids: &[String],
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            let issues = self.issues.lock().unwrap();
            Ok(issue_ids
                .iter()
                .filter_map(|issue_id| issues.get(issue_id))
                .cloned()
                .collect())
        }

        async fn fetch_issue_states_by_ids(
            &self,
            issue_ids: &[String],
        ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
            let issues = self.issues.lock().unwrap();
            Ok(issue_ids
                .iter()
                .filter_map(|issue_id| issues.get(issue_id))
                .map(|issue| polyphony_core::IssueStateUpdate {
                    id: issue.id.clone(),
                    identifier: issue.identifier.clone(),
                    state: issue.state.clone(),
                    updated_at: issue.updated_at,
                })
                .collect())
        }

        async fn update_issue_workflow_status(
            &self,
            _issue: &Issue,
            status: &str,
        ) -> Result<(), polyphony_core::Error> {
            self.workflow_updates
                .lock()
                .unwrap()
                .push(status.to_string());
            Ok(())
        }
    }

    struct NoopAgent;

    #[async_trait]
    impl AgentRuntime for NoopAgent {
        fn component_key(&self) -> String {
            "provider:test".into()
        }

        async fn run(
            &self,
            _spec: AgentRunSpec,
            _event_tx: mpsc::UnboundedSender<AgentEvent>,
        ) -> Result<AgentRunResult, polyphony_core::Error> {
            Ok(AgentRunResult::succeeded(1))
        }
    }

    #[derive(Clone)]
    struct NamedTracker {
        component: String,
        issues: Arc<Mutex<HashMap<String, Issue>>>,
    }

    impl NamedTracker {
        fn new(component: impl Into<String>, issues: Vec<Issue>) -> Self {
            Self {
                component: component.into(),
                issues: Arc::new(Mutex::new(
                    issues
                        .into_iter()
                        .map(|issue| (issue.id.clone(), issue))
                        .collect(),
                )),
            }
        }
    }

    #[async_trait]
    impl IssueTracker for NamedTracker {
        fn component_key(&self) -> String {
            self.component.clone()
        }

        async fn fetch_candidate_issues(
            &self,
            _query: &polyphony_core::TrackerQuery,
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            Ok(self.issues.lock().unwrap().values().cloned().collect())
        }

        async fn fetch_issues_by_states(
            &self,
            _project_slug: Option<&str>,
            states: &[String],
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            let normalized = states
                .iter()
                .map(|state| state.to_ascii_lowercase())
                .collect::<Vec<_>>();
            Ok(self
                .issues
                .lock()
                .unwrap()
                .values()
                .filter(|issue| normalized.contains(&issue.state.to_ascii_lowercase()))
                .cloned()
                .collect())
        }

        async fn fetch_issues_by_ids(
            &self,
            issue_ids: &[String],
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            let issues = self.issues.lock().unwrap();
            Ok(issue_ids
                .iter()
                .filter_map(|issue_id| issues.get(issue_id))
                .cloned()
                .collect())
        }

        async fn fetch_issue_states_by_ids(
            &self,
            issue_ids: &[String],
        ) -> Result<Vec<IssueStateUpdate>, polyphony_core::Error> {
            let issues = self.issues.lock().unwrap();
            Ok(issue_ids
                .iter()
                .filter_map(|issue_id| issues.get(issue_id))
                .map(|issue| IssueStateUpdate {
                    id: issue.id.clone(),
                    identifier: issue.identifier.clone(),
                    state: issue.state.clone(),
                    updated_at: issue.updated_at,
                })
                .collect())
        }
    }

    #[derive(Clone)]
    struct NamedAgent {
        component: String,
    }

    impl NamedAgent {
        fn new(component: impl Into<String>) -> Self {
            Self {
                component: component.into(),
            }
        }
    }

    #[async_trait]
    impl AgentRuntime for NamedAgent {
        fn component_key(&self) -> String {
            self.component.clone()
        }

        async fn run(
            &self,
            _spec: AgentRunSpec,
            _event_tx: mpsc::UnboundedSender<AgentEvent>,
        ) -> Result<AgentRunResult, polyphony_core::Error> {
            Ok(AgentRunResult::succeeded(1))
        }
    }

    #[derive(Clone, Default)]
    struct RecordingSessionAgent {
        prompts: Arc<Mutex<Vec<String>>>,
        session_starts: Arc<Mutex<u32>>,
        stops: Arc<Mutex<u32>>,
    }

    impl RecordingSessionAgent {
        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }

        fn session_starts(&self) -> u32 {
            *self.session_starts.lock().unwrap()
        }

        fn stops(&self) -> u32 {
            *self.stops.lock().unwrap()
        }
    }

    struct RecordingSession {
        prompts: Arc<Mutex<Vec<String>>>,
        stops: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl AgentSession for RecordingSession {
        async fn run_turn(
            &mut self,
            prompt: String,
        ) -> Result<AgentRunResult, polyphony_core::Error> {
            self.prompts.lock().unwrap().push(prompt);
            Ok(AgentRunResult::succeeded(1))
        }

        async fn stop(&mut self) -> Result<(), polyphony_core::Error> {
            *self.stops.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[async_trait]
    impl AgentRuntime for RecordingSessionAgent {
        fn component_key(&self) -> String {
            "provider:session-test".into()
        }

        async fn start_session(
            &self,
            _spec: AgentRunSpec,
            _event_tx: mpsc::UnboundedSender<AgentEvent>,
        ) -> Result<Option<Box<dyn AgentSession>>, polyphony_core::Error> {
            *self.session_starts.lock().unwrap() += 1;
            Ok(Some(Box::new(RecordingSession {
                prompts: self.prompts.clone(),
                stops: self.stops.clone(),
            })))
        }

        async fn run(
            &self,
            _spec: AgentRunSpec,
            _event_tx: mpsc::UnboundedSender<AgentEvent>,
        ) -> Result<AgentRunResult, polyphony_core::Error> {
            Err(polyphony_core::Error::Adapter(
                "run() should not be used when live sessions are available".into(),
            ))
        }
    }

    struct SequencedStateTracker {
        issue: Issue,
        states: Arc<Mutex<VecDeque<String>>>,
    }

    impl SequencedStateTracker {
        fn new(issue: Issue, states: Vec<&str>) -> Self {
            Self {
                issue,
                states: Arc::new(Mutex::new(states.into_iter().map(str::to_string).collect())),
            }
        }
    }

    #[async_trait]
    impl IssueTracker for SequencedStateTracker {
        fn component_key(&self) -> String {
            "tracker:sequence".into()
        }

        async fn fetch_candidate_issues(
            &self,
            _query: &polyphony_core::TrackerQuery,
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            Ok(vec![self.issue.clone()])
        }

        async fn fetch_issues_by_states(
            &self,
            _project_slug: Option<&str>,
            _states: &[String],
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            Ok(vec![self.issue.clone()])
        }

        async fn fetch_issues_by_ids(
            &self,
            issue_ids: &[String],
        ) -> Result<Vec<Issue>, polyphony_core::Error> {
            if issue_ids.iter().any(|issue_id| issue_id == &self.issue.id) {
                Ok(vec![self.issue.clone()])
            } else {
                Ok(Vec::new())
            }
        }

        async fn fetch_issue_states_by_ids(
            &self,
            issue_ids: &[String],
        ) -> Result<Vec<IssueStateUpdate>, polyphony_core::Error> {
            if !issue_ids.iter().any(|issue_id| issue_id == &self.issue.id) {
                return Ok(Vec::new());
            }
            let state = self
                .states
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| self.issue.state.clone());
            Ok(vec![IssueStateUpdate {
                id: self.issue.id.clone(),
                identifier: self.issue.identifier.clone(),
                state,
                updated_at: self.issue.updated_at,
            }])
        }
    }

    #[derive(Clone, Default)]
    struct RecordingProvisioner {
        cleaned: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingProvisioner {
        fn cleaned_issue_identifiers(&self) -> Vec<String> {
            self.cleaned.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl WorkspaceProvisioner for RecordingProvisioner {
        fn component_key(&self) -> String {
            "workspace:test".into()
        }

        async fn ensure_workspace(
            &self,
            request: WorkspaceRequest,
        ) -> Result<Workspace, polyphony_core::Error> {
            Ok(Workspace {
                path: request.workspace_path,
                workspace_key: request.workspace_key,
                created_now: false,
                branch_name: request.branch_name,
            })
        }

        async fn cleanup_workspace(
            &self,
            request: WorkspaceRequest,
        ) -> Result<(), polyphony_core::Error> {
            self.cleaned
                .lock()
                .unwrap()
                .push(request.issue_identifier.clone());
            Ok(())
        }
    }

    fn test_workflow(workspace_root: &Path) -> LoadedWorkflow {
        test_workflow_with_front_matter(
            workspace_root,
            "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
        )
    }

    fn test_workflow_with_front_matter(workspace_root: &Path, raw: &str) -> LoadedWorkflow {
        let workflow_path = workspace_root.join("WORKFLOW.md");
        fs::create_dir_all(workspace_root).unwrap();
        let raw = raw.replace("__ROOT__", &workspace_root.display().to_string());
        fs::write(&workflow_path, raw).unwrap();
        load_workflow(&workflow_path).unwrap()
    }

    fn test_service(
        tracker: TestTracker,
        provisioner: RecordingProvisioner,
        workspace_root: &Path,
    ) -> RuntimeService {
        let workflow = test_workflow(workspace_root);
        let (_tx, rx) = watch::channel(workflow);
        RuntimeService::new(
            Arc::new(tracker),
            Arc::new(NoopAgent),
            Arc::new(provisioner),
            None,
            None,
            None,
            None,
            None,
            None,
            rx,
        )
        .0
    }

    fn test_service_with_reload(
        workflow: LoadedWorkflow,
        tracker: Arc<dyn IssueTracker>,
        agent: Arc<dyn AgentRuntime>,
        provisioner: RecordingProvisioner,
        component_factory: Arc<RuntimeComponentFactory>,
    ) -> RuntimeService {
        let (tx, rx) = watch::channel(workflow.clone());
        RuntimeService::new(
            tracker,
            agent,
            Arc::new(provisioner),
            None,
            None,
            None,
            None,
            None,
            None,
            rx,
        )
        .0
        .with_workflow_reload(workflow.path.clone(), None, tx, component_factory)
    }

    fn sample_issue(issue_id: &str, identifier: &str, state: &str, title: &str) -> Issue {
        Issue {
            id: issue_id.to_string(),
            identifier: identifier.to_string(),
            title: title.to_string(),
            description: Some(format!("Description for {title}")),
            priority: Some(1),
            state: state.to_string(),
            branch_name: Some(format!("task/{}", identifier.to_ascii_lowercase())),
            labels: vec!["test".into()],
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
            ..Issue::default()
        }
    }

    fn make_running_task(issue: Issue, workspace_path: PathBuf) -> RunningTask {
        RunningTask {
            issue,
            agent_name: "mock".into(),
            model: None,
            attempt: None,
            workspace_path,
            stall_timeout_ms: 300_000,
            max_turns: 5,
            started_at: Utc::now(),
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: None,
            last_message: None,
            last_event_at: None,
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle: tokio::spawn(async {
                let _: () = std::future::pending().await;
            }),
        }
    }

    fn unique_workspace_root(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "polyphony-orchestrator-{test_name}-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[tokio::test]
    async fn reconcile_running_releases_missing_issue() {
        let workspace_root = unique_workspace_root("missing");
        let provisioner = RecordingProvisioner::default();
        let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
        let issue = sample_issue("issue-1", "FAC-1", "Todo", "Old");
        let workspace_path = workspace_root.join("FAC-1");
        service.state.running.insert(
            issue.id.clone(),
            make_running_task(issue.clone(), workspace_path),
        );
        service.claim_issue(issue.id.clone(), IssueClaimState::Running);

        service.reconcile_running().await;

        assert!(!service.state.running.contains_key(&issue.id));
        assert!(!service.is_claimed(&issue.id));
    }

    #[tokio::test]
    async fn tick_tracks_visible_issues_when_no_agents_are_configured() {
        let workspace_root = unique_workspace_root("visible-issues");
        let workflow = test_workflow_with_front_matter(
            &workspace_root,
            "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  by_state: {}\n  by_label: {}\n  profiles: {}\n---\nTest prompt\n",
        );
        let (_tx, rx) = watch::channel(workflow);
        let tracker = TestTracker::new(vec![
            sample_issue("issue-1", "FAC-1", "Todo", "First"),
            sample_issue("issue-2", "FAC-2", "In Progress", "Second"),
        ]);
        let provisioner = RecordingProvisioner::default();
        let mut service = RuntimeService::new(
            Arc::new(tracker),
            Arc::new(NoopAgent),
            Arc::new(provisioner),
            None,
            None,
            None,
            None,
            None,
            None,
            rx,
        )
        .0;

        service.tick().await;

        let snapshot = service.snapshot();
        let visible = snapshot
            .visible_issues
            .iter()
            .map(|issue| issue.issue_identifier.as_str())
            .collect::<Vec<_>>();

        assert_eq!(visible, vec!["FAC-1", "FAC-2"]);
        assert!(snapshot.running.is_empty());
    }

    #[tokio::test]
    async fn reconcile_running_cleans_workspace_for_terminal_issue() {
        let workspace_root = unique_workspace_root("terminal");
        let provisioner = RecordingProvisioner::default();
        let tracker_issue = sample_issue("issue-2", "FAC-2", "Done", "Closed");
        let mut service = test_service(
            TestTracker::new(vec![tracker_issue.clone()]),
            provisioner.clone(),
            &workspace_root,
        );
        let running_issue = sample_issue("issue-2", "FAC-2", "Todo", "Open");
        let workspace_path = workspace_root.join("FAC-2");
        fs::create_dir_all(&workspace_path).unwrap();
        service.state.running.insert(
            running_issue.id.clone(),
            make_running_task(running_issue.clone(), workspace_path),
        );
        service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

        service.reconcile_running().await;

        assert!(!service.state.running.contains_key(&running_issue.id));
        assert_eq!(provisioner.cleaned_issue_identifiers(), vec![
            running_issue.identifier
        ]);
    }

    #[tokio::test]
    async fn reconcile_running_replaces_full_issue_snapshot() {
        let workspace_root = unique_workspace_root("refresh");
        let provisioner = RecordingProvisioner::default();
        let mut refreshed_issue = sample_issue("issue-3", "FAC-3", "Todo", "Updated title");
        refreshed_issue.author = Some(IssueAuthor {
            id: Some("author-1".into()),
            username: Some("outsider".into()),
            display_name: Some("Outsider".into()),
            role: Some("none".into()),
            trust_level: Some("outsider".into()),
            url: None,
        });
        refreshed_issue.comments.push(IssueComment {
            id: "comment-1".into(),
            body: "New follow-up context".into(),
            author: refreshed_issue.author.clone(),
            url: None,
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        });
        let mut service = test_service(
            TestTracker::new(vec![refreshed_issue.clone()]),
            provisioner,
            &workspace_root,
        );
        let stale_issue = sample_issue("issue-3", "FAC-3", "Todo", "Old title");
        let workspace_path = workspace_root.join("FAC-3");
        service.state.running.insert(
            stale_issue.id.clone(),
            make_running_task(stale_issue.clone(), workspace_path),
        );
        service.claim_issue(stale_issue.id.clone(), IssueClaimState::Running);

        service.reconcile_running().await;

        let running = service.state.running.get(&stale_issue.id).unwrap();
        assert_eq!(running.issue.title, "Updated title");
        assert_eq!(running.issue.comments.len(), 1);
        assert_eq!(
            running
                .issue
                .author
                .as_ref()
                .and_then(|author| author.trust_level.as_deref()),
            Some("outsider")
        );
    }

    #[tokio::test]
    async fn finish_running_success_marks_completed_and_queues_retry() {
        let workspace_root = unique_workspace_root("finish");
        let provisioner = RecordingProvisioner::default();
        let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
        let issue = sample_issue("issue-4", "FAC-4", "Todo", "Work");
        let workspace_path = workspace_root.join("FAC-4");
        service.state.running.insert(
            issue.id.clone(),
            make_running_task(issue.clone(), workspace_path),
        );
        service.claim_issue(issue.id.clone(), IssueClaimState::Running);

        service
            .finish_running(
                issue.id.clone(),
                issue.identifier.clone(),
                None,
                Utc::now(),
                AgentRunResult {
                    status: AttemptStatus::Succeeded,
                    turns_completed: 1,
                    error: None,
                    final_issue_state: Some("Human Review".into()),
                },
            )
            .await
            .unwrap();

        assert!(service.state.completed.contains(&issue.id));
        assert!(service.state.retrying.contains_key(&issue.id));
        assert_eq!(
            service.state.claim_states.get(&issue.id),
            Some(&IssueClaimState::RetryQueued)
        );
    }

    #[tokio::test]
    async fn finish_running_with_active_final_state_skips_workflow_transition() {
        let workspace_root = unique_workspace_root("finish-active");
        let provisioner = RecordingProvisioner::default();
        let tracker = TestTracker::new(Vec::new());
        let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
        let issue = sample_issue("issue-4b", "FAC-4B", "Todo", "Work");
        let workspace_path = workspace_root.join("FAC-4B");
        service.state.running.insert(
            issue.id.clone(),
            make_running_task(issue.clone(), workspace_path),
        );
        service.claim_issue(issue.id.clone(), IssueClaimState::Running);

        service
            .finish_running(
                issue.id.clone(),
                issue.identifier.clone(),
                None,
                Utc::now(),
                AgentRunResult {
                    status: AttemptStatus::Succeeded,
                    turns_completed: 2,
                    error: None,
                    final_issue_state: Some("Todo".into()),
                },
            )
            .await
            .unwrap();

        assert!(tracker.recorded_workflow_updates().is_empty());
        assert!(service.state.retrying.contains_key(&issue.id));
    }

    #[test]
    fn worker_timeout_errors_map_to_timed_out_attempts() {
        let result =
            agent_run_result_from_error(&Error::Core(CoreError::Adapter("turn_timeout".into())));
        assert!(matches!(result.status, AttemptStatus::TimedOut));
        assert_eq!(result.error.as_deref(), Some("turn_timeout"));

        let startup_timeout = agent_run_result_from_error(&Error::Core(CoreError::Adapter(
            "response_timeout".into(),
        )));
        assert!(matches!(startup_timeout.status, AttemptStatus::TimedOut));
        assert_eq!(startup_timeout.error.as_deref(), Some("response_timeout"));
    }

    #[tokio::test]
    async fn fail_running_preserves_stalled_status() {
        let workspace_root = unique_workspace_root("finish-stalled");
        let provisioner = RecordingProvisioner::default();
        let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
        let issue = sample_issue("issue-4c", "FAC-4C", "Todo", "Stalled");
        let workspace_path = workspace_root.join("FAC-4C");
        service.state.running.insert(
            issue.id.clone(),
            make_running_task(issue.clone(), workspace_path),
        );
        service.claim_issue(issue.id.clone(), IssueClaimState::Running);

        service
            .fail_running(&issue.id, AttemptStatus::Stalled, "stall_timeout")
            .await;

        assert!(!service.state.running.contains_key(&issue.id));
        let retry = service.state.retrying.get(&issue.id).unwrap();
        assert_eq!(retry.row.error.as_deref(), Some("stall_timeout"));
        let context = service.state.saved_contexts.get(&issue.id).unwrap();
        assert_eq!(context.status, Some(AttemptStatus::Stalled));
        assert_eq!(context.error.as_deref(), Some("stall_timeout"));
    }

    #[tokio::test]
    async fn run_worker_attempt_reuses_live_session_and_continues_while_issue_active() {
        let workspace_root = unique_workspace_root("worker-turns");
        let provisioner = Arc::new(RecordingProvisioner::default());
        let workspace_manager = WorkspaceManager::new(
            workspace_root.clone(),
            provisioner,
            polyphony_core::CheckoutKind::Directory,
            true,
            Vec::new(),
            None,
            None,
            None,
        );
        let issue = sample_issue("issue-turns", "FAC-TURNS", "Todo", "Loop");
        let tracker = Arc::new(SequencedStateTracker::new(issue.clone(), vec![
            "Todo",
            "Human Review",
        ]));
        let agent = Arc::new(RecordingSessionAgent::default());
        let hooks = HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 1_000,
        };
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        let result = run_worker_attempt(
            &workspace_manager,
            &hooks,
            agent.clone(),
            tracker,
            issue,
            Some(2),
            workspace_root.join("FAC-TURNS"),
            "Initial prompt".into(),
            vec!["Todo".into(), "In Progress".into()],
            4,
            Some(
                "Continue {{ issue.identifier }} in state {{ issue.state }}.\n\
Turn {{ turn_number }} of {{ max_turns }}. Continuation={{ is_continuation }}."
                    .into(),
            ),
            polyphony_core::AgentDefinition {
                name: "codex".into(),
                kind: "codex".into(),
                transport: polyphony_core::AgentTransport::AppServer,
                ..polyphony_core::AgentDefinition::default()
            },
            None,
            command_tx,
        )
        .await
        .unwrap();

        while command_rx.try_recv().is_ok() {}

        assert!(matches!(result.status, AttemptStatus::Succeeded));
        assert_eq!(result.turns_completed, 2);
        assert_eq!(result.final_issue_state.as_deref(), Some("Human Review"));
        assert_eq!(agent.session_starts(), 1);
        assert_eq!(agent.stops(), 1);
        let prompts = agent.prompts();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0], "Initial prompt");
        assert_eq!(
            prompts[1],
            "Continue FAC-TURNS in state Todo.\nTurn 2 of 4. Continuation=true."
        );
    }

    #[tokio::test]
    async fn saved_context_updates_from_streamed_agent_events() {
        let workspace_root = unique_workspace_root("context-events");
        let provisioner = RecordingProvisioner::default();
        let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
        let issue = sample_issue("issue-5", "FAC-5", "Todo", "Context");
        let workspace_path = workspace_root.join("FAC-5");
        let mut running = make_running_task(issue.clone(), workspace_path);
        running.model = Some("kimi-2.5".into());
        service.state.running.insert(issue.id.clone(), running);

        service
            .handle_message(OrchestratorMessage::AgentEvent(AgentEvent {
                issue_id: issue.id.clone(),
                issue_identifier: issue.identifier.clone(),
                agent_name: "kimi".into(),
                session_id: Some("sess-1".into()),
                thread_id: Some("thread-1".into()),
                turn_id: Some("turn-3".into()),
                codex_app_server_pid: Some("4242".into()),
                kind: AgentEventKind::Notification,
                at: Utc::now(),
                message: Some("Investigating failing test".into()),
                usage: Some(TokenUsage {
                    input_tokens: 12,
                    output_tokens: 8,
                    total_tokens: 20,
                }),
                rate_limits: None,
                raw: None,
            }))
            .await
            .unwrap();

        let context = service.state.saved_contexts.get(&issue.id).unwrap();
        assert_eq!(context.agent_name, "kimi");
        assert_eq!(context.model.as_deref(), Some("kimi-2.5"));
        assert_eq!(context.session_id.as_deref(), Some("sess-1"));
        assert_eq!(context.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(context.turn_id.as_deref(), Some("turn-3"));
        assert_eq!(context.codex_app_server_pid.as_deref(), Some("4242"));
        assert_eq!(context.usage.total_tokens, 20);
        assert_eq!(context.transcript.len(), 1);
        assert!(
            context.transcript[0]
                .message
                .contains("Investigating failing test")
        );
        let snapshot = service.snapshot();
        let running = &snapshot.running[0];
        assert_eq!(running.session_id.as_deref(), Some("sess-1"));
        assert_eq!(running.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(running.turn_id.as_deref(), Some("turn-3"));
        assert_eq!(running.codex_app_server_pid.as_deref(), Some("4242"));
    }

    #[tokio::test]
    async fn retry_dispatch_rotates_to_fallback_agent_using_saved_context() {
        let workspace_root = unique_workspace_root("fallback");
        let workflow = test_workflow_with_front_matter(
            &workspace_root,
            "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: codex\n  profiles:\n    codex:\n      kind: codex\n      transport: app_server\n      command: codex app-server\n      fallbacks:\n        - kimi\n        - claude\n    kimi:\n      kind: kimi\n      api_key: test-kimi\n      model: kimi-2.5\n    claude:\n      kind: claude\n      transport: local_cli\n      command: claude\n---\nTest prompt\n",
        );
        let (_tx, rx) = watch::channel(workflow.clone());
        let tracker = TestTracker::new(vec![sample_issue("issue-6", "FAC-6", "Todo", "Retry")]);
        let provisioner = RecordingProvisioner::default();
        let mut service = RuntimeService::new(
            Arc::new(tracker),
            Arc::new(NoopAgent),
            Arc::new(provisioner),
            None,
            None,
            None,
            None,
            None,
            None,
            rx,
        )
        .0;
        let issue = sample_issue("issue-6", "FAC-6", "Todo", "Retry");
        service
            .state
            .saved_contexts
            .insert(issue.id.clone(), AgentContextSnapshot {
                issue_id: issue.id.clone(),
                issue_identifier: issue.identifier.clone(),
                updated_at: Utc::now(),
                agent_name: "codex".into(),
                model: Some("gpt-5-codex".into()),
                session_id: Some("session-1".into()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                status: Some(AttemptStatus::Failed),
                error: Some("rate limited".into()),
                usage: TokenUsage::default(),
                transcript: vec![AgentContextEntry {
                    at: Utc::now(),
                    kind: AgentEventKind::Notification,
                    message: "Partial work already completed".into(),
                }],
            });

        service
            .dispatch_issue(workflow, issue.clone(), Some(2), true)
            .await
            .unwrap();

        let running = service.state.running.get(&issue.id).unwrap();
        assert_eq!(running.agent_name, "kimi");
        running.handle.abort();
    }

    #[tokio::test]
    async fn tick_defensively_reloads_workflow_and_rebuilds_components() {
        let workspace_root = unique_workspace_root("workflow-reload");
        let workflow = test_workflow_with_front_matter(
            &workspace_root,
            "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nInitial prompt\n",
        );
        let component_factory: Arc<RuntimeComponentFactory> = Arc::new(|workflow| {
            Ok(RuntimeComponents {
                tracker: Arc::new(NamedTracker::new(
                    format!("tracker:{}", workflow.config.tracker.kind),
                    Vec::new(),
                )),
                agent: Arc::new(NamedAgent::new(format!(
                    "agent:{}",
                    workflow.config.tracker.kind
                ))),
                committer: None,
                pull_request_manager: None,
                pull_request_commenter: None,
                feedback: None,
            })
        });
        let mut service = test_service_with_reload(
            workflow.clone(),
            Arc::new(NamedTracker::new("tracker:mock", Vec::new())),
            Arc::new(NamedAgent::new("agent:mock")),
            RecordingProvisioner::default(),
            component_factory,
        );

        fs::write(
            &workflow.path,
            "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 250\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nReloaded prompt\n"
                .replace("__ROOT__", &workspace_root.display().to_string()),
        )
        .unwrap();

        service.tick().await;

        assert_eq!(service.tracker.component_key(), "tracker:none");
        assert_eq!(service.agent.component_key(), "agent:none");
        assert_eq!(service.workflow().config.polling.interval_ms, 250);
        assert_eq!(
            service.workflow().definition.prompt_template,
            "Reloaded prompt"
        );
    }

    #[tokio::test]
    async fn invalid_reloaded_workflow_blocks_dispatch_until_fixed() {
        let workspace_root = unique_workspace_root("workflow-invalid");
        let workflow = test_workflow_with_front_matter(
            &workspace_root,
            "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nPrompt\n",
        );
        let issue = sample_issue("issue-reload", "FAC-RELOAD", "Todo", "Blocked");
        let issue_for_factory = issue.clone();
        let component_factory: Arc<RuntimeComponentFactory> = Arc::new(move |workflow| {
            Ok(RuntimeComponents {
                tracker: Arc::new(NamedTracker::new(
                    format!("tracker:{}", workflow.config.tracker.kind),
                    vec![issue_for_factory.clone()],
                )),
                agent: Arc::new(NamedAgent::new(format!(
                    "agent:{}",
                    workflow.config.tracker.kind
                ))),
                committer: None,
                pull_request_manager: None,
                pull_request_commenter: None,
                feedback: None,
            })
        });
        let mut service = test_service_with_reload(
            workflow.clone(),
            Arc::new(NamedTracker::new("tracker:none", vec![issue.clone()])),
            Arc::new(NamedAgent::new("agent:none")),
            RecordingProvisioner::default(),
            component_factory,
        );

        fs::write(&workflow.path, "---\ntracker:\n  kind: [\n").unwrap();

        service.tick().await;

        assert!(service.workflow_reload_error().is_some());
        assert!(service.state.running.is_empty());
        assert_eq!(service.workflow().definition.prompt_template, "Prompt");
    }

    #[test]
    fn append_saved_context_includes_recent_transcript() {
        let prompt = append_saved_context(
            "Base prompt".into(),
            Some(&AgentContextSnapshot {
                issue_id: "issue-7".into(),
                issue_identifier: "FAC-7".into(),
                updated_at: Utc::now(),
                agent_name: "claude".into(),
                model: Some("claude-sonnet".into()),
                session_id: Some("session-2".into()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                status: Some(AttemptStatus::Failed),
                error: Some("tool timeout".into()),
                usage: TokenUsage::default(),
                transcript: vec![AgentContextEntry {
                    at: Utc::now(),
                    kind: AgentEventKind::Notification,
                    message: "Implemented parser, tests still failing".into(),
                }],
            }),
            true,
        );

        assert!(prompt.contains("## Saved Polyphony Context"));
        assert!(prompt.contains("Last agent: claude (claude-sonnet)"));
        assert!(prompt.contains("Last error: tool timeout"));
        assert!(prompt.contains("Implemented parser, tests still failing"));
    }
}
