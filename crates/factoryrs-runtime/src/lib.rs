use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use factoryrs_core::{
    AgentEvent, AgentEventKind, AgentRunResult, AgentRunSpec, AgentRuntime, AttemptStatus,
    BudgetSnapshot, CheckoutKind, CodexTotals, Error as CoreError, Issue, IssueTracker,
    PersistedRunRecord, RateLimitSignal, RetryRow, RunningRow, RuntimeEvent, RuntimeSnapshot,
    SnapshotCounts, StateStore, ThrottleWindow, TokenUsage, TrackerQuery, Workspace,
    WorkspaceProvisioner, WorkspaceRequest, sanitize_workspace_key,
};
use factoryrs_workflow::{HooksConfig, LoadedWorkflow, load_workflow, render_prompt};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

#[derive(Debug, Error)]
pub enum Error {
    #[error("workflow error: {0}")]
    Workflow(#[from] factoryrs_workflow::Error),
    #[error("core error: {0}")]
    Core(#[from] factoryrs_core::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),
    #[error("hook failure: {0}")]
    Hook(String),
}

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
    attempt: Option<u32>,
    workspace_path: PathBuf,
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

#[derive(Debug)]
struct RuntimeState {
    running: HashMap<String, RunningTask>,
    claimed: HashSet<String>,
    retrying: HashMap<String, RetryEntry>,
    throttles: HashMap<String, ActiveThrottle>,
    budgets: HashMap<String, BudgetSnapshot>,
    recent_events: VecDeque<RuntimeEvent>,
    ended_runtime_seconds: f64,
    totals: CodexTotals,
    rate_limits: Option<Value>,
    last_budget_poll_at: Option<DateTime<Utc>>,
}

pub struct WorkspaceManager {
    root: PathBuf,
    provisioner: Arc<dyn WorkspaceProvisioner>,
    checkout_kind: CheckoutKind,
    source_repo_path: Option<PathBuf>,
    clone_url: Option<String>,
    default_branch: Option<String>,
}

impl WorkspaceManager {
    pub fn new(
        root: PathBuf,
        provisioner: Arc<dyn WorkspaceProvisioner>,
        checkout_kind: CheckoutKind,
        source_repo_path: Option<PathBuf>,
        clone_url: Option<String>,
        default_branch: Option<String>,
    ) -> Self {
        Self {
            root,
            provisioner,
            checkout_kind,
            source_repo_path,
            clone_url,
            default_branch,
        }
    }

    pub async fn ensure_workspace(
        &self,
        issue_identifier: &str,
        branch_name: Option<String>,
        hooks: &HooksConfig,
    ) -> Result<Workspace, Error> {
        tokio::fs::create_dir_all(&self.root).await?;
        let workspace_key = sanitize_workspace_key(issue_identifier);
        let workspace_path = self.root.join(&workspace_key);
        ensure_contained(&self.root, &workspace_path)?;
        let workspace = self
            .provisioner
            .ensure_workspace(WorkspaceRequest {
                issue_identifier: issue_identifier.to_string(),
                workspace_root: self.root.clone(),
                workspace_path: workspace_path.clone(),
                workspace_key,
                branch_name,
                checkout_kind: self.checkout_kind.clone(),
                source_repo_path: self.source_repo_path.clone(),
                clone_url: self.clone_url.clone(),
                default_branch: self.default_branch.clone(),
            })
            .await
            .map_err(Error::Core)?;
        if workspace.created_now {
            self.run_hook(
                "after_create",
                hooks.after_create.as_deref(),
                &workspace_path,
                hooks.timeout_ms,
            )
            .await?;
        }
        Ok(workspace)
    }

    pub async fn run_before_run(
        &self,
        hooks: &HooksConfig,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        self.run_hook(
            "before_run",
            hooks.before_run.as_deref(),
            workspace_path,
            hooks.timeout_ms,
        )
        .await
    }

    pub async fn run_after_run_best_effort(&self, hooks: &HooksConfig, workspace_path: &Path) {
        if let Err(error) = self
            .run_hook(
                "after_run",
                hooks.after_run.as_deref(),
                workspace_path,
                hooks.timeout_ms,
            )
            .await
        {
            warn!(%error, "after_run hook failed");
        }
    }

    pub async fn cleanup_workspace(
        &self,
        issue_identifier: &str,
        branch_name: Option<String>,
        hooks: &HooksConfig,
    ) -> Result<(), Error> {
        let workspace_key = sanitize_workspace_key(issue_identifier);
        let workspace_path = self.root.join(workspace_key.clone());
        if tokio::fs::metadata(&workspace_path).await.is_err() {
            return Ok(());
        }
        ensure_contained(&self.root, &workspace_path)?;
        if let Err(error) = self
            .run_hook(
                "before_remove",
                hooks.before_remove.as_deref(),
                &workspace_path,
                hooks.timeout_ms,
            )
            .await
        {
            warn!(%error, "before_remove hook failed");
        }
        self.provisioner
            .cleanup_workspace(WorkspaceRequest {
                issue_identifier: issue_identifier.to_string(),
                workspace_root: self.root.clone(),
                workspace_path,
                workspace_key,
                branch_name,
                checkout_kind: self.checkout_kind.clone(),
                source_repo_path: self.source_repo_path.clone(),
                clone_url: self.clone_url.clone(),
                default_branch: self.default_branch.clone(),
            })
            .await
            .map_err(Error::Core)?;
        Ok(())
    }

    async fn run_hook(
        &self,
        hook_name: &str,
        script: Option<&str>,
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<(), Error> {
        let Some(script) = script else {
            return Ok(());
        };
        let mut command = Command::new("bash");
        command.arg("-lc").arg(script).current_dir(cwd);
        let status = tokio::time::timeout(Duration::from_millis(timeout_ms), command.status())
            .await
            .map_err(|_| Error::Hook(format!("{hook_name} timed out")))??;
        if !status.success() {
            return Err(Error::Hook(format!(
                "{hook_name} exited with status {status}"
            )));
        }
        Ok(())
    }
}

impl RuntimeService {
    pub fn new(
        tracker: Arc<dyn IssueTracker>,
        agent: Arc<dyn AgentRuntime>,
        provisioner: Arc<dyn WorkspaceProvisioner>,
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
                store,
                workflow_rx,
                snapshot_tx,
                command_tx: command_tx.clone(),
                command_rx,
                external_command_rx,
                state: RuntimeState {
                    running: HashMap::new(),
                    claimed: HashSet::new(),
                    retrying: HashMap::new(),
                    throttles: HashMap::new(),
                    budgets: HashMap::new(),
                    recent_events: VecDeque::with_capacity(128),
                    ended_runtime_seconds: 0.0,
                    totals: CodexTotals::default(),
                    rate_limits: None,
                    last_budget_poll_at: None,
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

    fn build_workspace_manager(&self, workflow: &LoadedWorkflow) -> WorkspaceManager {
        WorkspaceManager::new(
            workflow.config.workspace.root.clone(),
            self.provisioner.clone(),
            parse_checkout_kind(&workflow.config.workspace.checkout_kind),
            workflow.config.workspace.source_repo_path.clone(),
            workflow.config.workspace.clone_url.clone(),
            workflow.config.workspace.default_branch.clone(),
        )
    }

    async fn tick(&mut self) {
        self.reconcile_running().await;
        self.poll_budgets().await;
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
        let query = TrackerQuery {
            project_slug: workflow.config.tracker.project_slug.clone(),
            repository: workflow.config.tracker.repository.clone(),
            active_states: workflow.config.tracker.active_states.clone(),
            terminal_states: workflow.config.tracker.terminal_states.clone(),
        };
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(signal);
                let _ = self.emit_snapshot().await;
                return;
            }
            Err(error) => {
                self.push_event("tracker".into(), format!("candidate fetch failed: {error}"));
                error!(%error, "candidate fetch failed");
                let _ = self.emit_snapshot().await;
                return;
            }
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
            if let Err(error) = self.dispatch_issue(workflow.clone(), issue, None).await {
                self.push_event("dispatch".into(), format!("dispatch failed: {error}"));
                error!(%error, "dispatch failed");
            }
        }
        let _ = self.emit_snapshot().await;
    }

    async fn reconcile_running(&mut self) {
        let workflow = self.workflow_rx.borrow().clone();
        let stall_timeout_ms = workflow.config.provider.stall_timeout_ms;
        if stall_timeout_ms > 0 {
            let stall_limit = Duration::from_millis(stall_timeout_ms as u64);
            let stale_ids = self
                .state
                .running
                .iter()
                .filter_map(|(issue_id, running)| {
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
        }

        let running_ids = self.state.running.keys().cloned().collect::<Vec<_>>();
        if running_ids.is_empty() {
            return;
        }
        if self.is_throttled(&self.tracker.component_key()) {
            return;
        }
        let states = match self.tracker.fetch_issue_states_by_ids(&running_ids).await {
            Ok(states) => states,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(signal);
                return;
            }
            Err(error) => {
                warn!(%error, "running state refresh failed");
                return;
            }
        };
        let active = normalize_states(&workflow.config.tracker.active_states);
        let terminal = normalize_states(&workflow.config.tracker.terminal_states);
        for update in states {
            let state = update.state.to_ascii_lowercase();
            if terminal.contains(&state) {
                self.stop_running(&update.id, true).await;
            } else if active.contains(&state) {
                if let Some(running) = self.state.running.get_mut(&update.id) {
                    running.issue.state = update.state;
                }
            } else {
                self.stop_running(&update.id, false).await;
            }
        }
    }

    async fn dispatch_issue(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
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
        let runtime_command = workflow.config.provider.command.clone();
        let max_turns = workflow.config.agent.max_turns;
        let prompt = render_prompt(&workflow.definition, &issue, attempt)?;
        let workspace_path = workspace.path.clone();
        let started_at = Utc::now();

        let handle = tokio::spawn(async move {
            let manager = WorkspaceManager::new(
                workflow.config.workspace.root.clone(),
                provisioner,
                parse_checkout_kind(&workflow.config.workspace.checkout_kind),
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
                runtime_command,
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

        self.state.claimed.insert(issue.id.clone());
        self.state.retrying.remove(&issue.id);
        self.state.running.insert(
            issue.id.clone(),
            RunningTask {
                issue,
                attempt,
                workspace_path: workspace.path,
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
            },
        );
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
        let query = TrackerQuery {
            project_slug: workflow.config.tracker.project_slug.clone(),
            repository: workflow.config.tracker.repository.clone(),
            active_states: workflow.config.tracker.active_states.clone(),
            terminal_states: workflow.config.tracker.terminal_states.clone(),
        };
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(signal);
                return;
            }
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
            }
        };
        let Some(issue) = issues
            .into_iter()
            .find(|issue| issue.id == retry.row.issue_id)
        else {
            self.state.claimed.remove(&issue_id);
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
            .dispatch_issue(workflow.clone(), issue, Some(retry.row.attempt))
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
                    if let Some(usage) = event.usage {
                        apply_usage_delta(&mut self.state.totals, running, usage);
                    }
                    if let Some(rate_limits) = event.rate_limits {
                        running.rate_limits = Some(rate_limits.clone());
                        self.state.rate_limits = Some(rate_limits);
                    }
                }
                self.push_event(
                    "agent".into(),
                    format!(
                        "{} {}",
                        event.issue_identifier,
                        event.message.unwrap_or_else(|| format!("{:?}", event.kind))
                    ),
                );
                self.emit_snapshot().await?;
            }
            OrchestratorMessage::RateLimited(signal) => {
                self.register_throttle(signal);
                self.emit_snapshot().await?;
            }
            OrchestratorMessage::WorkerFinished {
                issue_id,
                issue_identifier,
                attempt,
                started_at,
                outcome,
            } => {
                self.finish_running(issue_id, issue_identifier, attempt, started_at, outcome)
                    .await?;
            }
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
                self.schedule_retry(
                    issue_id.clone(),
                    issue_identifier.clone(),
                    1,
                    None,
                    true,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            }
            AttemptStatus::CancelledByReconciliation => {
                self.state.claimed.remove(&issue_id);
            }
            _ => {
                self.schedule_retry(
                    issue_id.clone(),
                    issue_identifier.clone(),
                    attempt.unwrap_or(0) + 1,
                    outcome.error.clone(),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            }
        }
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
                self.register_throttle(signal);
                return;
            }
            Err(error) => {
                warn!(%error, "startup terminal cleanup skipped");
                return;
            }
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
            self.state.claimed.remove(issue_id);
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
        if self.state.running.contains_key(&issue.id) || self.state.claimed.contains(&issue.id) {
            return false;
        }
        let active = normalize_states(&workflow.config.tracker.active_states);
        let terminal = normalize_states(&workflow.config.tracker.terminal_states);
        let state = issue.normalized_state();
        if !active.contains(&state) || terminal.contains(&state) {
            return false;
        }
        if state == "todo" {
            for blocker in &issue.blocked_by {
                let blocker_state = blocker
                    .state
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !blocker_state.is_empty() && !terminal.contains(&blocker_state) {
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
        if let Some(limit) = workflow
            .config
            .agent
            .max_concurrent_agents_by_state
            .get(&normalized)
        {
            let count = self
                .state
                .running
                .values()
                .filter(|entry| entry.issue.normalized_state() == normalized)
                .count();
            count < *limit
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
        self.state.claimed.insert(issue_id.clone());
        self.state.retrying.insert(
            issue_id.clone(),
            RetryEntry {
                row: RetryRow {
                    issue_id,
                    issue_identifier: issue_identifier.clone(),
                    attempt,
                    due_at: Utc::now() + chrono::Duration::milliseconds(delay_ms as i64),
                    error: error.clone(),
                },
                due_at: Instant::now() + Duration::from_millis(delay_ms),
            },
        );
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
            recent_events: self.state.recent_events.iter().cloned().collect(),
        }
    }

    fn restore_bootstrap(&mut self, bootstrap: factoryrs_core::StoreBootstrap) {
        self.state.recent_events = bootstrap.recent_events.into_iter().collect();
        self.state.budgets = bootstrap.budgets;
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
        self.state.throttles.insert(
            signal.component.clone(),
            ActiveThrottle {
                window: ThrottleWindow {
                    component: signal.component.clone(),
                    until,
                    reason: signal.reason.clone(),
                },
                due_at,
            },
        );
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
            }
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
            Ok(None) => {}
            Err(CoreError::RateLimited(signal)) => self.register_throttle(signal),
            Err(error) => warn!(%error, "tracker budget poll failed"),
        }

        match self.agent.fetch_budget().await {
            Ok(Some(snapshot)) => self.record_budget(snapshot).await,
            Ok(None) => {}
            Err(CoreError::RateLimited(signal)) => self.register_throttle(signal),
            Err(error) => warn!(%error, "agent budget poll failed"),
        }
    }

    async fn record_budget(&mut self, snapshot: BudgetSnapshot) {
        self.state
            .budgets
            .insert(snapshot.component.clone(), snapshot.clone());
        if let Some(store) = &self.store {
            if let Err(error) = store.record_budget(&snapshot).await {
                warn!(%error, "persisting budget snapshot failed");
            }
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
                    }
                    Err(error) => {
                        warn!(%error, "workflow reload failed; keeping last good config");
                    }
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
    runtime_command: String,
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
                runtime_command,
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
            let _ = command_tx.send(OrchestratorMessage::RateLimited(signal.clone()));
            warn!(issue_id = %issue_id, "worker attempt hit provider rate limit");
            Err(Error::Core(CoreError::RateLimited(signal)))
        }
        Err(error) => {
            warn!(issue_id = %issue_id, %error, "worker attempt failed");
            Err(Error::Core(error))
        }
    }
}

fn ensure_contained(root: &Path, workspace: &Path) -> Result<(), Error> {
    let root = absolute_path(root);
    let workspace = absolute_path(workspace);
    if !workspace.starts_with(&root) {
        return Err(Error::Hook(format!(
            "workspace path escapes root: {}",
            workspace.display()
        )));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn normalize_states(states: &[String]) -> HashSet<String> {
    states
        .iter()
        .map(|state| state.to_ascii_lowercase())
        .collect()
}

fn parse_checkout_kind(value: &str) -> CheckoutKind {
    match value {
        "linked_worktree" => CheckoutKind::LinkedWorktree,
        "discrete_clone" => CheckoutKind::DiscreteClone,
        _ => CheckoutKind::Directory,
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
        recent_events: Vec::new(),
    }
}
