use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use {
    chrono::{DateTime, Utc},
    notify::{RecommendedWatcher, RecursiveMode, Watcher},
    polyphony_core::{
        AgentContextEntry, AgentContextSnapshot, AgentEvent, AgentEventKind, AgentModelCatalog,
        AgentRunResult, AgentRunSpec, AgentRuntime, AttemptStatus, BudgetSnapshot, CodexTotals,
        Error as CoreError, FeedbackAction, FeedbackLink, FeedbackNotification, Issue,
        IssueTracker, PersistedRunRecord, PullRequestCommenter, PullRequestManager,
        PullRequestRequest, RateLimitSignal, RetryRow, RunningRow, RuntimeEvent, RuntimeSnapshot,
        SnapshotCounts, StateStore, ThrottleWindow, TokenUsage, WorkspaceCommitRequest,
        WorkspaceCommitter, WorkspaceProvisioner, sanitize_workspace_key,
    },
    polyphony_feedback::FeedbackRegistry,
    polyphony_workflow::{
        HooksConfig, LoadedWorkflow, load_workflow, render_issue_template_with_strings,
        render_prompt,
    },
    polyphony_workspace::WorkspaceManager,
    serde_json::Value,
    thiserror::Error,
    tokio::{
        sync::{mpsc, watch},
        task::JoinHandle,
    },
    tracing::{error, info, warn},
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

pub struct RuntimeService {
    tracker: Arc<dyn IssueTracker>,
    agent: Arc<dyn AgentRuntime>,
    provisioner: Arc<dyn WorkspaceProvisioner>,
    committer: Option<Arc<dyn WorkspaceCommitter>>,
    pull_request_manager: Option<Arc<dyn PullRequestManager>>,
    pull_request_commenter: Option<Arc<dyn PullRequestCommenter>>,
    feedback: Option<Arc<FeedbackRegistry>>,
    store: Option<Arc<dyn StateStore>>,
    workflow_rx: watch::Receiver<LoadedWorkflow>,
    snapshot_tx: watch::Sender<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
    command_rx: mpsc::UnboundedReceiver<OrchestratorMessage>,
    external_command_rx: mpsc::UnboundedReceiver<RuntimeCommand>,
    state: RuntimeState,
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
    started_at: DateTime<Utc>,
    session_id: Option<String>,
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
    saved_contexts: HashMap<String, AgentContextSnapshot>,
    recent_events: VecDeque<RuntimeEvent>,
    ended_runtime_seconds: f64,
    totals: CodexTotals,
    rate_limits: Option<Value>,
    last_budget_poll_at: Option<DateTime<Utc>>,
    last_model_discovery_at: Option<DateTime<Utc>>,
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
                workflow_rx,
                snapshot_tx,
                command_tx: command_tx.clone(),
                command_rx,
                external_command_rx,
                state: RuntimeState {
                    running: HashMap::new(),
                    claim_states: HashMap::new(),
                    retrying: HashMap::new(),
                    completed: HashSet::new(),
                    throttles: HashMap::new(),
                    budgets: HashMap::new(),
                    agent_catalogs: HashMap::new(),
                    saved_contexts: HashMap::new(),
                    recent_events: VecDeque::with_capacity(128),
                    ended_runtime_seconds: 0.0,
                    totals: CodexTotals::default(),
                    rate_limits: None,
                    last_budget_poll_at: None,
                    last_model_discovery_at: None,
                },
            },
            RuntimeHandle {
                snapshot_rx,
                command_tx: external_command_tx,
            },
        )
    }

    pub async fn run(mut self) -> Result<(), Error> {
        if let Some(store) = &self.store {
            let bootstrap = store.bootstrap().await?;
            self.restore_bootstrap(bootstrap);
        }
        self.startup_cleanup().await;
        self.emit_snapshot().await?;
        let mut next_tick = Instant::now();

        loop {
            let next_retry = self.next_retry_deadline();
            let next_deadline = next_retry
                .map(|retry| retry.min(next_tick))
                .unwrap_or(next_tick);
            let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(next_deadline));
            tokio::pin!(sleep);

            tokio::select! {
                _ = &mut sleep => {
                    let now = Instant::now();
                    if now >= next_tick {
                        self.tick().await;
                        let interval = Duration::from_millis(self.workflow_rx.borrow().config.polling.interval_ms);
                        next_tick = Instant::now() + interval;
                    }
                    self.process_due_retries().await;
                }
                Some(message) = self.command_rx.recv() => {
                    self.handle_message(message).await?;
                }
                Some(command) = self.external_command_rx.recv() => match command {
                    RuntimeCommand::Refresh => {
                        next_tick = Instant::now();
                    }
                    RuntimeCommand::Shutdown => {
                        self.abort_all().await;
                        self.emit_snapshot().await?;
                        return Ok(());
                    }
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
            workflow.config.workspace_checkout_kind(),
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

    async fn tick(&mut self) {
        self.reconcile_running().await;
        self.poll_budgets().await;
        self.refresh_agent_catalogs().await;
        let workflow = self.workflow_rx.borrow().clone();
        if let Err(error) = workflow.config.validate() {
            self.push_event("workflow".into(), format!("validation failed: {error}"));
            error!(%error, "workflow validation failed");
            let _ = self.emit_snapshot().await;
            return;
        }
        if self.is_throttled(&self.tracker.component_key())
            || self.is_throttled(&self.agent.component_key())
        {
            self.push_event(
                "throttle".into(),
                "dispatch skipped while a component is throttled".into(),
            );
            let _ = self.emit_snapshot().await;
            return;
        }
        let query = workflow.config.tracker_query();
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                let _ = self.emit_snapshot().await;
                return;
            },
            Err(error) => {
                self.push_event("tracker".into(), format!("candidate fetch failed: {error}"));
                error!(%error, "candidate fetch failed");
                let _ = self.emit_snapshot().await;
                return;
            },
        };

        let mut issues = issues;
        issues.sort_by(dispatch_order);
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
                self.push_event("dispatch".into(), format!("dispatch failed: {error}"));
                error!(%error, "dispatch failed");
            }
        }
        let _ = self.emit_snapshot().await;
    }

    async fn reconcile_running(&mut self) {
        let workflow = self.workflow_rx.borrow().clone();
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
            self.fail_running(&issue_id, "stall timeout exceeded").await;
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
                "reconcile".into(),
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
        let max_turns = workflow.config.agent.max_turns;
        let prompt = append_saved_context(
            render_prompt(&workflow.definition, &issue, attempt)?,
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
                "handoff".into(),
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

        let handle = tokio::spawn(async move {
            let manager = WorkspaceManager::new(
                workflow.config.workspace.root.clone(),
                provisioner,
                workflow.config.workspace_checkout_kind(),
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
                max_turns,
                selected_agent_for_task,
                saved_context,
                command_tx.clone(),
            )
            .await;
            let outcome = match outcome {
                Ok(result) => result,
                Err(error) => AgentRunResult {
                    status: AttemptStatus::Failed,
                    turns_completed: 0,
                    error: Some(error.to_string()),
                    final_issue_state: None,
                },
            };
            let _ = command_tx.send(OrchestratorMessage::WorkerFinished {
                issue_id,
                issue_identifier: issue_identifier_for_task,
                attempt,
                started_at,
                outcome,
            });
        });

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
            started_at,
            session_id: None,
            last_event: Some("dispatch_started".into()),
            last_message: Some("worker launched".into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle,
        });
        self.push_event("dispatch".into(), format!("dispatched {issue_identifier}"));
        Ok(())
    }

    async fn process_due_retries(&mut self) {
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
        let workflow = self.workflow_rx.borrow().clone();
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
                    "agent".into(),
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
                    status: format!("{:?}", outcome.status),
                    attempt,
                    started_at,
                    finished_at: Some(Utc::now()),
                    details,
                })
                .await?;
        }

        let workflow = self.workflow_rx.borrow().clone();
        match outcome.status {
            AttemptStatus::Succeeded => {
                let workflow_status = outcome
                    .final_issue_state
                    .clone()
                    .unwrap_or_else(|| "Human Review".into());
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
                        "handoff".into(),
                        format!("{} handoff failed: {}", running.issue.identifier, error),
                    );
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
            "worker".into(),
            format!("{} {:?}", issue_identifier, outcome.status),
        );
        self.emit_snapshot().await?;
        Ok(())
    }

    async fn startup_cleanup(&mut self) {
        let workflow = self.workflow_rx.borrow().clone();
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
        let workflow = self.workflow_rx.borrow().clone();
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
                "reconcile".into(),
                format!("stopped {}", running.issue.identifier),
            );
        }
    }

    async fn fail_running(&mut self, issue_id: &str, reason: &str) {
        if let Some(running) = self.state.running.remove(issue_id) {
            running.handle.abort();
            let max_retry_backoff_ms = self.workflow_rx.borrow().config.agent.max_retry_backoff_ms;
            self.schedule_retry(
                issue_id.to_string(),
                running.issue.identifier.clone(),
                running.attempt.unwrap_or(0) + 1,
                Some(reason.to_string()),
                false,
                max_retry_backoff_ms,
            );
            self.push_event(
                "retry".into(),
                format!("{} {}", running.issue.identifier, reason),
            );
        }
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
            "retry".into(),
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

    fn push_event(&mut self, scope: String, message: String) {
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
            },
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
                    session_id: running.session_id.clone(),
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
            "throttle".into(),
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

        let workflow = self.workflow_rx.borrow().clone();
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
        let workflow = self.workflow_rx.borrow().clone();
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
                kind: format!("{:?}", event.kind),
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
        context.status = Some(format!("{:?}", outcome.status));
        context.error = outcome.error.clone();
        context.usage = running.tokens.clone();
        if let Some(error) = &outcome.error {
            context.transcript.push(AgentContextEntry {
                at: Utc::now(),
                kind: "outcome".into(),
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
                "handoff".into(),
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
            "handoff".into(),
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
                "feedback".into(),
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
    workflow_tx: watch::Sender<LoadedWorkflow>,
    runtime_command_tx: mpsc::UnboundedSender<RuntimeCommand>,
) -> Result<JoinHandle<Result<(), Error>>, Error> {
    Ok(tokio::spawn(async move {
        let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
            let _ = notify_tx.send(event);
        })?;
        watcher.watch(&workflow_path, RecursiveMode::NonRecursive)?;
        while let Some(event) = notify_rx.recv().await {
            match event {
                Ok(_) => match load_workflow(&workflow_path) {
                    Ok(workflow) => {
                        let _ = workflow_tx.send(workflow);
                        let _ = runtime_command_tx.send(RuntimeCommand::Refresh);
                        info!("workflow reloaded");
                    },
                    Err(error) => {
                        warn!(%error, "workflow reload failed; keeping last good config");
                    },
                },
                Err(error) => warn!(%error, "workflow watch event failed"),
            }
        }
        Ok(())
    }))
}

async fn run_worker_attempt(
    workspace_manager: &WorkspaceManager,
    hooks: &HooksConfig,
    agent: Arc<dyn AgentRuntime>,
    _tracker: Arc<dyn IssueTracker>,
    issue: Issue,
    attempt: Option<u32>,
    workspace_path: PathBuf,
    prompt: String,
    max_turns: u32,
    selected_agent: polyphony_core::AgentDefinition,
    saved_context: Option<AgentContextSnapshot>,
    command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
) -> Result<AgentRunResult, Error> {
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
    let result = agent
        .run(
            AgentRunSpec {
                issue,
                attempt,
                workspace_path: workspace_path.clone(),
                prompt,
                max_turns,
                agent: selected_agent,
                prior_context: saved_context,
            },
            event_tx,
        )
        .await;
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
        running: Vec::new(),
        retrying: Vec::new(),
        codex_totals: CodexTotals::default(),
        rate_limits: None,
        throttles: Vec::new(),
        budgets: Vec::new(),
        agent_catalogs: Vec::new(),
        saved_contexts: Vec::new(),
        recent_events: Vec::new(),
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
            "- [{}] {}: {}\n",
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
            fs,
            path::Path,
            sync::{Arc, Mutex},
        },
    };

    use {
        async_trait::async_trait,
        polyphony_core::{IssueAuthor, IssueComment, Workspace, WorkspaceRequest},
        tokio::sync::watch,
    };

    #[derive(Clone)]
    struct TestTracker {
        issues: Arc<Mutex<HashMap<String, Issue>>>,
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
            }
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
            Ok(AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: None,
            })
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
            "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nprovider:\n  kind: mock\n  command: mock\n---\nTest prompt\n",
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
            rx,
        )
        .0
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
            url: None,
            author: None,
            labels: vec!["test".into()],
            comments: Vec::new(),
            blocked_by: Vec::new(),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
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
            started_at: Utc::now(),
            session_id: None,
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
        assert_eq!(context.usage.total_tokens, 20);
        assert_eq!(context.transcript.len(), 1);
        assert!(
            context.transcript[0]
                .message
                .contains("Investigating failing test")
        );
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
                status: Some("Failed".into()),
                error: Some("rate limited".into()),
                usage: TokenUsage::default(),
                transcript: vec![AgentContextEntry {
                    at: Utc::now(),
                    kind: "Notification".into(),
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
                status: Some("Failed".into()),
                error: Some("tool timeout".into()),
                usage: TokenUsage::default(),
                transcript: vec![AgentContextEntry {
                    at: Utc::now(),
                    kind: "Notification".into(),
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
