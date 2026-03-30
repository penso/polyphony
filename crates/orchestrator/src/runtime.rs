use std::{collections::HashMap, sync::Mutex};

use polyphony_core::{
    UserInteractionReporter, UserInteractionRequest, WorkspaceProgressReporter,
    WorkspaceProgressUpdate,
};

use crate::{prelude::*, *};

const SLOW_TRACKER_FETCH_WARN_THRESHOLD: Duration = Duration::from_millis(750);

struct RuntimeInteractionReporter {
    snapshot_tx: watch::Sender<RuntimeSnapshot>,
    snapshot_rx: Mutex<watch::Receiver<RuntimeSnapshot>>,
    user_interactions: Arc<Mutex<HashMap<String, UserInteractionRequest>>>,
}

struct RuntimeWorkspaceProgressReporter {
    snapshot_tx: watch::Sender<RuntimeSnapshot>,
    snapshot_rx: Mutex<watch::Receiver<RuntimeSnapshot>>,
    command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
}

impl RuntimeWorkspaceProgressReporter {
    fn new(
        snapshot_tx: watch::Sender<RuntimeSnapshot>,
        snapshot_rx: watch::Receiver<RuntimeSnapshot>,
        command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
    ) -> Self {
        Self {
            snapshot_tx,
            snapshot_rx: Mutex::new(snapshot_rx),
            command_tx,
        }
    }

    fn publish_snapshot(&self, update: &WorkspaceProgressUpdate) {
        let mut snapshot = self
            .snapshot_rx
            .lock()
            .ok()
            .map(|receiver| receiver.borrow().clone())
            .unwrap_or_else(empty_snapshot);
        apply_workspace_progress_to_snapshot(&mut snapshot, update);
        let _ = self.snapshot_tx.send(snapshot);
    }
}

impl RuntimeInteractionReporter {
    fn new(
        snapshot_tx: watch::Sender<RuntimeSnapshot>,
        snapshot_rx: watch::Receiver<RuntimeSnapshot>,
        user_interactions: Arc<Mutex<HashMap<String, UserInteractionRequest>>>,
    ) -> Self {
        Self {
            snapshot_tx,
            snapshot_rx: Mutex::new(snapshot_rx),
            user_interactions,
        }
    }

    fn publish_snapshot(&self) {
        let mut snapshot = self
            .snapshot_rx
            .lock()
            .ok()
            .map(|receiver| receiver.borrow().clone())
            .unwrap_or_else(empty_snapshot);
        snapshot.pending_user_interactions = self.pending_user_interactions();
        let _ = self.snapshot_tx.send(snapshot);
    }

    fn pending_user_interactions(&self) -> Vec<UserInteractionRequest> {
        let mut interactions = self
            .user_interactions
            .lock()
            .ok()
            .map(|interactions| interactions.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        interactions.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        interactions
    }
}

impl UserInteractionReporter for RuntimeInteractionReporter {
    fn begin(&self, interaction: UserInteractionRequest) {
        if let Ok(mut interactions) = self.user_interactions.lock() {
            interactions.insert(interaction.id.clone(), interaction);
        }
        self.publish_snapshot();
    }

    fn end(&self, interaction_id: &str) {
        if let Ok(mut interactions) = self.user_interactions.lock() {
            interactions.remove(interaction_id);
        }
        self.publish_snapshot();
    }
}

impl WorkspaceProgressReporter for RuntimeWorkspaceProgressReporter {
    fn log(&self, update: WorkspaceProgressUpdate) {
        self.publish_snapshot(&update);
        let _ = self
            .command_tx
            .send(OrchestratorMessage::WorkspaceProgress(update));
    }
}

fn apply_workspace_progress_to_snapshot(
    snapshot: &mut RuntimeSnapshot,
    update: &WorkspaceProgressUpdate,
) {
    let run_id = snapshot
        .runs
        .iter()
        .find(|run| {
            run.workspace_key.as_deref() == Some(update.workspace_key.as_str())
                || run.issue_identifier.as_deref() == Some(update.issue_identifier.as_str())
        })
        .map(|run| run.id.clone());
    let Some(run_id) = run_id else {
        return;
    };
    let Some(task) = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.run_id == run_id && task.ordinal == 0)
    else {
        return;
    };
    if activity_log_ends_with_message(&task.activity_log, &update.message) {
        return;
    }
    let line = format!("[{}] {}", update.at.format("%H:%M:%S"), update.message);
    task.activity_log.push(line);
    const TASK_ACTIVITY_LOG_LIMIT: usize = 64;
    if task.activity_log.len() > TASK_ACTIVITY_LOG_LIMIT {
        let excess = task.activity_log.len() - TASK_ACTIVITY_LOG_LIMIT;
        task.activity_log.drain(0..excess);
    }
    task.updated_at = update.at;
}

fn activity_log_ends_with_message(activity_log: &[String], message: &str) -> bool {
    activity_log.last().is_some_and(|line| {
        line.strip_prefix('[')
            .and_then(|line| line.split_once("] "))
            .map_or(line == message, |(_, suffix)| suffix == message)
    })
}

impl RuntimeService {
    pub fn new(
        tracker: Arc<dyn IssueTracker>,
        pull_request_event_source: Option<Arc<dyn PullRequestEventSource>>,
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
        let user_interactions = Arc::new(Mutex::new(HashMap::new()));
        let interaction_reporter: Arc<dyn UserInteractionReporter> =
            Arc::new(RuntimeInteractionReporter::new(
                snapshot_tx.clone(),
                snapshot_rx.clone(),
                user_interactions.clone(),
            ));
        let progress_reporter: Arc<dyn WorkspaceProgressReporter> =
            Arc::new(RuntimeWorkspaceProgressReporter::new(
                snapshot_tx.clone(),
                snapshot_rx.clone(),
                command_tx.clone(),
            ));
        provisioner.set_interaction_reporter(Some(interaction_reporter.clone()));
        provisioner.set_progress_reporter(Some(progress_reporter));
        if let Some(ref committer) = committer {
            committer.set_interaction_reporter(Some(interaction_reporter));
        }
        let initial_dispatch_mode = workflow_rx.borrow().config.startup_dispatch_mode();
        let state = RuntimeState {
            dispatch_mode: initial_dispatch_mode,
            ..RuntimeState::default()
        };
        (
            Self {
                tracker,
                pull_request_event_source,
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
                pending_inbox_approvals: Vec::new(),
                pending_issue_closures: Vec::new(),
                pending_deliverable_resolutions: Vec::new(),
                pending_manual_dispatches: Vec::new(),
                pending_manual_pull_request_inbox_dispatches: Vec::new(),
                pending_merge_deliverables: Vec::new(),
                pending_run_retries: Vec::new(),
                pending_task_resolutions: Vec::new(),
                pending_task_retries: Vec::new(),
                pending_agent_stops: Vec::new(),
                pending_create_issues: Vec::new(),
                pending_feedback_injections: Vec::new(),
                reload_support: None,
                state,
                user_interactions,
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
            last_seen_fingerprint: workflow_inputs_fingerprint(
                &workflow_path,
                user_config_path.as_deref(),
            )
            .ok(),
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
        if let Some(cache) = &self.cache
            && let Ok(cached) = cache.load().await
        {
            self.restore_cache(cached);
        }
        // Preserve the dispatch mode restored from the persisted snapshot so that
        // operator mode changes survive daemon restarts.  Only fall back to the
        // config default when no snapshot was restored (i.e. first run).
        if !self.state.bootstrap_restored {
            self.state.dispatch_mode = self.workflow_rx.borrow().config.startup_dispatch_mode();
        }
        self.normalize_restored_in_progress_runs().await?;
        self.refresh_tracker_connection(true).await;
        self.emit_snapshot().await?;
        // startup_cleanup is deferred to the first tick so the select loop
        // starts immediately and can process Refresh/Shutdown commands.
        let mut startup_cleanup_done = false;
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
                        RuntimeCommand::SetMode(mode) => {
                            info!(?mode, "dispatch mode changed (event loop)");
                            self.push_event(
                                EventScope::Dispatch,
                                format!("dispatch mode set to {mode}"),
                            );
                            self.state.dispatch_mode = mode;
                            if mode == polyphony_core::DispatchMode::Stop {
                                self.abort_all().await;
                            }
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::ApproveInboxItem { item_id, source } => {
                            info!(%item_id, %source, "inbox item approval queued (event loop)");
                            self.pending_inbox_approvals.push((item_id, source));
                            self.process_pending_inbox_approvals().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::CloseTrackerIssue { issue_id } => {
                            info!(%issue_id, "issue closure queued (event loop)");
                            self.pending_issue_closures.push(issue_id);
                            self.process_pending_issue_closures().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::ResolveRunDeliverable {
                            run_id,
                            decision,
                        } => {
                            self.pending_deliverable_resolutions
                                .push((run_id, decision));
                            self.process_pending_deliverable_resolutions().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::DispatchIssue {
                            issue_id,
                            agent_name,
                            directives,
                        } => {
                            info!(%issue_id, ?agent_name, has_directives = directives.is_some(), "manual dispatch queued (event loop)");
                            self.pending_manual_dispatches.push(ManualDispatchRequest {
                                issue_id,
                                agent_name,
                                directives,
                            });
                            next_tick = Instant::now();
                        }
                        RuntimeCommand::DispatchPullRequestInboxItem {
                            item_id,
                            directives,
                        } => {
                            info!(%item_id, has_directives = directives.is_some(), "manual pull request inbox dispatch queued (event loop)");
                            self.pending_manual_pull_request_inbox_dispatches.push(
                                ManualPullRequestInboxDispatchRequest {
                                    item_id,
                                    directives,
                                },
                            );
                            next_tick = Instant::now();
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::MergeDeliverable { run_id } => {
                            info!(%run_id, "merge deliverable requested (event loop)");
                            self.merge_deliverable(&run_id).await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::RetryRun { run_id } => {
                            info!(%run_id, "run retry requested");
                            self.pending_run_retries.push(run_id);
                            self.process_pending_run_retries().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::ResolveTask {
                            run_id,
                            task_id,
                        } => {
                            info!(%run_id, %task_id, "manual task resolution requested");
                            self.pending_task_resolutions
                                .push((run_id, task_id));
                            self.process_pending_task_resolutions().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::RetryTask {
                            run_id,
                            task_id,
                        } => {
                            info!(%run_id, %task_id, "task retry requested");
                            self.pending_task_retries
                                .push((run_id, task_id));
                            self.process_pending_task_retries().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::StopAgent { issue_id } => {
                            info!(%issue_id, "user-initiated agent stop requested");
                            self.stop_running_by_user(&issue_id).await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::CreateIssue { title, description } => {
                            info!(%title, "create issue requested");
                            self.pending_create_issues.push(CreateIssueCommandRequest {
                                title,
                                description,
                            });
                            self.process_pending_create_issues().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::InjectRunFeedback {
                            run_id,
                            prompt,
                            agent_name,
                        } => {
                            info!(%run_id, "feedback injection requested");
                            self.pending_feedback_injections.push(FeedbackInjectionRequest {
                                run_id,
                                prompt,
                                agent_name,
                            });
                            self.process_pending_feedback_injections().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::RefreshRepo { repo_id } => {
                            info!(%repo_id, "repo refresh requested (not yet implemented)");
                        }
                        RuntimeCommand::AddRepo(_registration) => {
                            info!("add repo requested (not yet implemented)");
                        }
                        RuntimeCommand::RemoveRepo { repo_id } => {
                            info!(%repo_id, "remove repo requested (not yet implemented)");
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
                        if !startup_cleanup_done {
                            startup_cleanup_done = true;
                            self.startup_cleanup().await;
                            let _ = self.emit_snapshot().await;
                        }
                        let interval = Duration::from_millis(self.workflow_rx.borrow().config.polling.interval_ms);
                        next_tick = Instant::now() + interval;
                    }
                    self.process_due_retries().await;
                }
            }
        }
    }

    pub(crate) fn claim_issue(&mut self, issue_id: impl Into<String>, state: IssueClaimState) {
        self.state.claim_states.insert(issue_id.into(), state);
    }

    pub(crate) fn release_issue(&mut self, issue_id: &str) {
        self.state.claim_states.remove(issue_id);
    }

    pub(crate) fn is_claimed(&self, issue_id: &str) -> bool {
        self.state.claim_states.contains_key(issue_id)
    }

    pub(crate) fn build_workspace_manager(&self, workflow: &LoadedWorkflow) -> WorkspaceManager {
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

    pub(crate) fn select_dispatch_agent(
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
        let ordered_names = ordered_candidates
            .iter()
            .map(|agent| agent.name.clone())
            .collect::<Vec<_>>();
        let selected = ordered_candidates
            .into_iter()
            .find(|agent| !self.is_throttled(&format!("agent:{}", agent.name)))
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "all candidate agents are throttled for issue `{}`",
                    issue.identifier
                )))
            })?;
        info!(
            issue_identifier = %issue.identifier,
            selected_agent = %selected.name,
            candidates = %ordered_names.join(","),
            saved_context_agent = saved_context.map(|context| context.agent_name.as_str()).unwrap_or("none"),
            prefer_alternate_agent,
            "selected dispatch agent"
        );
        Ok(selected)
    }

    /// Drain pending external commands. Returns `true` if shutdown was requested.
    /// Refresh commands set `pending_refresh` so the caller can act on them.
    pub(crate) fn drain_commands(&mut self) -> bool {
        loop {
            match self.external_command_rx.try_recv() {
                Ok(RuntimeCommand::Shutdown) => return true,
                Ok(RuntimeCommand::Refresh) => {
                    self.pending_refresh = true;
                },
                Ok(RuntimeCommand::SetMode(mode)) => {
                    info!(?mode, "dispatch mode changed");
                    self.push_event(EventScope::Dispatch, format!("dispatch mode set to {mode}"));
                    self.state.dispatch_mode = mode;
                },
                Ok(RuntimeCommand::ApproveInboxItem { item_id, source }) => {
                    info!(%item_id, %source, "inbox item approval queued");
                    self.pending_inbox_approvals.push((item_id, source));
                },
                Ok(RuntimeCommand::CloseTrackerIssue { issue_id }) => {
                    info!(%issue_id, "issue closure queued");
                    self.pending_issue_closures.push(issue_id);
                },
                Ok(RuntimeCommand::ResolveRunDeliverable { run_id, decision }) => {
                    self.pending_deliverable_resolutions
                        .push((run_id, decision));
                },
                Ok(RuntimeCommand::DispatchIssue {
                    issue_id,
                    agent_name,
                    directives,
                }) => {
                    info!(%issue_id, ?agent_name, has_directives = directives.is_some(), "manual dispatch queued");
                    self.pending_manual_dispatches.push(ManualDispatchRequest {
                        issue_id,
                        agent_name,
                        directives,
                    });
                },
                Ok(RuntimeCommand::DispatchPullRequestInboxItem {
                    item_id,
                    directives,
                }) => {
                    info!(%item_id, has_directives = directives.is_some(), "manual pull request inbox dispatch queued");
                    self.pending_manual_pull_request_inbox_dispatches.push(
                        ManualPullRequestInboxDispatchRequest {
                            item_id,
                            directives,
                        },
                    );
                },
                Ok(RuntimeCommand::MergeDeliverable { run_id }) => {
                    info!(%run_id, "merge deliverable queued");
                    self.pending_merge_deliverables.push(run_id);
                },
                Ok(RuntimeCommand::RetryRun { run_id }) => {
                    info!(%run_id, "run retry queued");
                    self.pending_run_retries.push(run_id);
                },
                Ok(RuntimeCommand::ResolveTask { run_id, task_id }) => {
                    info!(%run_id, %task_id, "manual task resolution queued");
                    self.pending_task_resolutions.push((run_id, task_id));
                },
                Ok(RuntimeCommand::RetryTask { run_id, task_id }) => {
                    info!(%run_id, %task_id, "task retry queued");
                    self.pending_task_retries.push((run_id, task_id));
                },
                Ok(RuntimeCommand::StopAgent { issue_id }) => {
                    info!(%issue_id, "user-initiated agent stop queued");
                    self.pending_agent_stops.push(issue_id);
                },
                Ok(RuntimeCommand::CreateIssue { title, description }) => {
                    info!(%title, "create issue queued");
                    self.pending_create_issues
                        .push(CreateIssueCommandRequest { title, description });
                },
                Ok(RuntimeCommand::InjectRunFeedback {
                    run_id,
                    prompt,
                    agent_name,
                }) => {
                    info!(%run_id, "feedback injection queued");
                    self.pending_feedback_injections
                        .push(FeedbackInjectionRequest {
                            run_id,
                            prompt,
                            agent_name,
                        });
                },
                Ok(RuntimeCommand::RefreshRepo { repo_id }) => {
                    info!(%repo_id, "repo refresh queued (not yet implemented)");
                },
                Ok(RuntimeCommand::AddRepo(_)) => {
                    info!("add repo queued (not yet implemented)");
                },
                Ok(RuntimeCommand::RemoveRepo { repo_id }) => {
                    info!(%repo_id, "remove repo queued (not yet implemented)");
                },
                Err(_) => return false,
            }
        }
    }

    pub(crate) async fn process_pending_inbox_approvals(&mut self) {
        let approvals = std::mem::take(&mut self.pending_inbox_approvals);
        if approvals.is_empty() {
            return;
        }
        let workflow = self.workflow();
        for (item_id, source) in approvals {
            let approval_key = issue_key_for_source(&source, &item_id);
            if !self.state.approved_inbox_keys.insert(approval_key.clone()) {
                self.push_event(
                    EventScope::Dispatch,
                    format!("{source} inbox item {item_id} is already approved"),
                );
                continue;
            }
            self.push_event(
                EventScope::Dispatch,
                format!("{source} inbox item {item_id} approved for dispatch"),
            );
            if let Some(store) = &self.store {
                let snapshot = self.snapshot();
                if let Err(error) = store.save_snapshot(&snapshot).await {
                    self.push_event(
                        EventScope::Dispatch,
                        format!(
                            "{source} inbox item {item_id} approved but failed to persist: {error}"
                        ),
                    );
                }
            }
            if self.state.dispatch_mode != polyphony_core::DispatchMode::Manual {
                continue;
            }
            if let Some(row) = self
                .state
                .tracker_issues
                .iter()
                .find(|row| approval_key_for_row(workflow.config.tracker.kind, row) == approval_key)
                .map(|row| self.resolved_tracker_issue_row(workflow.config.tracker.kind, row))
                && self.should_dispatch_tracker_issue(&row)
            {
                let issue_identifier = row.issue_identifier.clone();
                self.push_event(
                    EventScope::Dispatch,
                    format!("{issue_identifier} approved and ready for manual dispatch"),
                );
                continue;
            }
            if let Some(event) = self.visible_pull_request_event(&item_id)
                && self.pull_request_event_approval_state(&event) == DispatchApprovalState::Approved
            {
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "{} approved and ready for manual dispatch",
                        event.display_identifier()
                    ),
                );
            }
        }
        self.save_cache().await;
    }

    pub(crate) async fn process_pending_issue_closures(&mut self) {
        let closures = std::mem::take(&mut self.pending_issue_closures);
        if closures.is_empty() {
            return;
        }
        let workflow = self.workflow();
        let tracker_kind = workflow.config.tracker.kind;
        let terminal_state = workflow
            .config
            .tracker
            .terminal_states
            .first()
            .cloned()
            .unwrap_or_else(|| "closed".into());

        for issue_id in closures {
            if self.state.running.contains_key(&issue_id) {
                self.push_event(
                    EventScope::Dispatch,
                    format!("issue {issue_id} cannot be closed while running"),
                );
                continue;
            }

            let request = polyphony_core::UpdateIssueRequest {
                id: issue_id.clone(),
                state: Some(terminal_state.clone()),
                ..Default::default()
            };
            let issue = match self.tracker.update_issue(&request).await {
                Ok(issue) => issue,
                Err(error) => {
                    self.push_event(
                        EventScope::Dispatch,
                        format!("issue {issue_id} failed to close: {error}"),
                    );
                    continue;
                },
            };

            let approval_key = approval_key_for_issue(tracker_kind, &issue);
            self.state.approved_inbox_keys.remove(&approval_key);
            self.state
                .tracker_issues
                .retain(|row| row.issue_id != issue_id);
            self.state
                .bootstrapped_tracker_issues
                .retain(|row| row.issue_id != issue_id);
            self.state.discarded_inbox_items.remove(&issue_id);

            let workspace_key = sanitize_workspace_key(&issue.identifier);
            if self.state.worktree_keys.contains(&workspace_key) {
                let manager = self.build_workspace_manager(&workflow);
                match manager
                    .cleanup_workspace(
                        &issue.identifier,
                        issue.branch_name.clone(),
                        &workflow.config.hooks,
                    )
                    .await
                {
                    Ok(()) => {
                        self.state.worktree_keys.remove(&workspace_key);
                        self.push_event(
                            EventScope::Dispatch,
                            format!("{} workspace cleaned up", issue.identifier),
                        );
                    },
                    Err(error) => {
                        self.push_event(
                            EventScope::Dispatch,
                            format!(
                                "{} closed but workspace cleanup failed: {error}",
                                issue.identifier
                            ),
                        );
                    },
                }
            }

            self.push_event(
                EventScope::Dispatch,
                format!("{} marked {}", issue.identifier, terminal_state),
            );
        }
        self.save_cache().await;
    }

    pub(crate) async fn process_manual_dispatches(&mut self) {
        let dispatches = std::mem::take(&mut self.pending_manual_dispatches);
        if dispatches.is_empty() {
            return;
        }
        // Manual dispatch always proceeds — the user explicitly requested it,
        // even in stop mode.
        info!(count = dispatches.len(), "processing manual dispatches");
        let workflow = self.workflow();
        for request in dispatches {
            let issue_id = request.issue_id.clone();
            let issues = match self
                .tracker
                .fetch_issues_by_ids(std::slice::from_ref(&issue_id))
                .await
            {
                Ok(issues) => issues,
                Err(error) => {
                    self.push_event(
                        EventScope::Dispatch,
                        format!("manual dispatch fetch failed for {issue_id}: {error}"),
                    );
                    warn!(%error, %issue_id, "failed to fetch issue for manual dispatch");
                    continue;
                },
            };
            let Some(issue) = issues.into_iter().next() else {
                self.push_event(
                    EventScope::Dispatch,
                    format!("manual dispatch: issue {issue_id} not found"),
                );
                warn!(%issue_id, "issue not found for manual dispatch");
                continue;
            };
            self.dispatch_requested_issue(
                workflow.clone(),
                issue,
                request.agent_name.as_deref(),
                request.directives.as_deref(),
                "manual dispatch",
            )
            .await;
        }
    }

    pub(crate) async fn process_pending_deliverable_resolutions(&mut self) {
        let resolutions = std::mem::take(&mut self.pending_deliverable_resolutions);
        if resolutions.is_empty() {
            return;
        }

        let mut merge_after = Vec::new();

        for (run_id, decision) in resolutions {
            let Some(run) = self.state.runs.get(&run_id) else {
                self.push_event(
                    EventScope::Handoff,
                    format!("deliverable decision ignored: run {run_id} not found"),
                );
                continue;
            };
            let run_label = Self::run_target_label(run);
            let Some(deliverable) = run.deliverable.as_ref() else {
                let message =
                    format!("deliverable decision ignored: run {run_label} has no deliverable");
                self.push_event(EventScope::Handoff, message);
                continue;
            };
            if deliverable.decision != polyphony_core::DeliverableDecision::Waiting {
                self.push_event(
                    EventScope::Handoff,
                    format!(
                        "deliverable decision ignored: {run_label} already {}",
                        deliverable.decision
                    ),
                );
                continue;
            }

            let Some(run) = self.state.runs.get_mut(&run_id) else {
                self.push_event(
                    EventScope::Handoff,
                    format!("deliverable decision ignored: run {run_id} not found"),
                );
                continue;
            };
            let Some(deliverable) = run.deliverable.as_mut() else {
                let message =
                    format!("deliverable decision ignored: run {run_label} has no deliverable");
                self.push_event(EventScope::Handoff, message);
                continue;
            };
            deliverable.decision = decision;
            run.updated_at = Utc::now();
            let persist_error = if let Some(store) = &self.store {
                store
                    .save_run(run)
                    .await
                    .err()
                    .map(|error| error.to_string())
            } else {
                None
            };
            let message = if let Some(error) = persist_error {
                format!("{run_label} deliverable marked {decision} but failed to persist: {error}")
            } else {
                format!("{run_label} deliverable marked {decision}")
            };
            self.push_event(EventScope::Handoff, message);

            // Auto-merge on accept: queue a merge so the user doesn't have to
            // press a separate key.
            if decision == polyphony_core::DeliverableDecision::Accepted {
                merge_after.push(run_id);
            }
        }

        for run_id in merge_after {
            self.merge_deliverable(&run_id).await;
        }
    }

    pub(crate) async fn process_pending_task_resolutions(&mut self) {
        let resolutions = std::mem::take(&mut self.pending_task_resolutions);
        if resolutions.is_empty() {
            return;
        }

        for (run_id, task_id) in resolutions {
            // Mark the task as completed
            let task_found = if let Some(tasks) = self.state.tasks.get_mut(&run_id) {
                if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = polyphony_core::TaskStatus::Completed;
                    task.finished_at = Some(Utc::now());
                    task.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        let _ = store.save_task(task).await;
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if !task_found {
                self.push_event(
                    EventScope::Dispatch,
                    format!("task resolution ignored: task {task_id} not found in {run_id}"),
                );
                continue;
            }

            // Reset the run from Failed back to Executing so the pipeline continues
            if let Some(run) = self.state.runs.get_mut(&run_id) {
                run.status = RunStatus::InProgress;
                run.pipeline_stage = Some(PipelineStage::Executing);
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    let _ = store.save_run(run).await;
                }
            }

            info!(
                run_id,
                task_id, "task manually resolved — resuming pipeline"
            );
            self.push_event(
                EventScope::Dispatch,
                format!("task {task_id} manually resolved, pipeline resuming"),
            );

            // Build a minimal Issue from the run and dispatch next task
            let run_info = self.state.runs.get(&run_id).map(|m| {
                (
                    m.issue_id.clone().unwrap_or_default(),
                    m.issue_identifier.clone().unwrap_or_default(),
                    m.title.clone(),
                    m.workspace_path.clone(),
                )
            });
            if let Some((issue_id, identifier, title, Some(ws))) = run_info {
                let issue = polyphony_core::Issue {
                    id: issue_id,
                    identifier,
                    title,
                    description: None,
                    priority: None,
                    state: "In Progress".into(),
                    branch_name: None,
                    url: None,
                    author: None,
                    labels: Vec::new(),
                    comments: Vec::new(),
                    blocked_by: Vec::new(),
                    approval_state: polyphony_core::DispatchApprovalState::Approved,
                    parent_id: None,
                    created_at: None,
                    updated_at: None,
                };
                if let Err(error) = self
                    .dispatch_next_task(self.workflow(), issue, None, false, &run_id, &ws)
                    .await
                {
                    warn!(%error, run_id, "failed to dispatch next task after manual resolution");
                }
            }
        }
    }

    pub(crate) async fn process_pending_task_retries(&mut self) {
        let retries = std::mem::take(&mut self.pending_task_retries);
        if retries.is_empty() {
            return;
        }

        for (run_id, task_id) in retries {
            let Some(task) = self
                .state
                .tasks
                .get(&run_id)
                .and_then(|tasks| tasks.iter().find(|task| task.id == task_id))
                .cloned()
            else {
                self.push_event(
                    EventScope::Dispatch,
                    format!("task retry ignored: task {task_id} not found in {run_id}"),
                );
                continue;
            };
            if task.status != polyphony_core::TaskStatus::Failed {
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "task retry ignored: task {task_id} in {run_id} is {}, only failed tasks can retry",
                        task.status
                    ),
                );
                continue;
            }
            if let Err(error) = self
                .retry_failed_run_from_task(&run_id, Some(task.id.clone()))
                .await
            {
                warn!(%error, run_id, task_id, "failed to dispatch task retry");
            }
        }
    }

    pub(crate) async fn process_pending_run_retries(&mut self) {
        let retries = std::mem::take(&mut self.pending_run_retries);
        if retries.is_empty() {
            return;
        }

        for run_id in retries {
            if let Err(error) = self.retry_failed_run_from_task(&run_id, None).await {
                warn!(%error, run_id, "failed to dispatch run retry");
            }
        }
    }

    pub(crate) async fn process_pending_create_issues(&mut self) {
        let requests = std::mem::take(&mut self.pending_create_issues);
        if requests.is_empty() {
            return;
        }

        for request in requests {
            let create_req = polyphony_core::CreateIssueRequest {
                title: request.title.clone(),
                description: Some(request.description.clone()),
                ..Default::default()
            };
            match self.tracker.create_issue(&create_req).await {
                Ok(issue) => {
                    self.push_event(
                        EventScope::Dispatch,
                        format!("created issue {}: {}", issue.identifier, issue.title),
                    );
                    self.pending_refresh = true;
                },
                Err(error) => {
                    warn!(%error, title = %request.title, "failed to create issue");
                    self.push_event(
                        EventScope::Dispatch,
                        format!("failed to create issue: {error}"),
                    );
                },
            }
        }
    }

    pub(crate) async fn process_pending_feedback_injections(&mut self) {
        let requests = std::mem::take(&mut self.pending_feedback_injections);
        if requests.is_empty() {
            return;
        }

        for request in requests {
            if let Err(error) = self.inject_feedback_task(&request).await {
                warn!(%error, run_id = %request.run_id, "failed to inject feedback");
                self.push_event(
                    EventScope::Dispatch,
                    format!("failed to inject feedback into {}: {error}", request.run_id),
                );
            }
        }
    }

    async fn retry_failed_run_from_task(
        &mut self,
        run_id: &str,
        requested_task_id: Option<TaskId>,
    ) -> Result<(), Error> {
        let Some(run) = self.state.runs.get(run_id).cloned() else {
            self.push_event(
                EventScope::Dispatch,
                format!("run retry ignored: run {run_id} not found"),
            );
            return Ok(());
        };
        // If run is Delivered but the workspace has unpushed commits, re-run
        // the handoff (push + PR) instead of re-dispatching the agent.
        if run.status == RunStatus::Delivered {
            let needs_push = run
                .workspace_path
                .as_ref()
                .is_some_and(|path| polyphony_git::has_unpushed_commits(path));
            if needs_push {
                info!(
                    %run_id,
                    "retrying delivered run with unpushed commits"
                );
                self.push_event(
                    EventScope::Dispatch,
                    format!("retrying push for delivered run {run_id}"),
                );
                // Reset failed steps to Pending and mark run as Failed so the
                // dispatch path re-enters completing and re-runs the handoff.
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.reset_failed_steps();
                    run.status = RunStatus::Failed;
                    run.pipeline_stage = Some(polyphony_core::PipelineStage::Completing);
                    run.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        let _ = store.save_run(run).await;
                    }
                }
                // Fall through to the normal retry logic below
            } else {
                self.push_event(
                    EventScope::Dispatch,
                    format!("run retry ignored: run {run_id} is delivered with no pending changes"),
                );
                return Ok(());
            }
        }

        let run_has_running_worker = self
            .state
            .running
            .values()
            .any(|running| running.run_id.as_deref() == Some(run_id));
        let can_retry_stalled_run = requested_task_id.is_none()
            && run.status == RunStatus::InProgress
            && !run_has_running_worker;
        if run.status != RunStatus::Failed && !can_retry_stalled_run {
            self.push_event(
                EventScope::Dispatch,
                format!(
                    "run retry ignored: run {run_id} is {}, only failed or stalled runs can retry",
                    run.status
                ),
            );
            return Ok(());
        }

        let failed_task = {
            let Some(tasks) = self.state.tasks.get(run_id) else {
                self.push_event(
                    EventScope::Dispatch,
                    format!("run retry ignored: run {run_id} has no tasks"),
                );
                return Ok(());
            };
            if let Some(task_id) = requested_task_id.as_ref() {
                let Some(task) = tasks.iter().find(|task| &task.id == task_id).cloned() else {
                    self.push_event(
                        EventScope::Dispatch,
                        format!("task retry ignored: task {task_id} not found in {run_id}"),
                    );
                    return Ok(());
                };
                task
            } else {
                let first_failed_task = tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Failed)
                    .min_by_key(|task| task.ordinal)
                    .cloned();
                let next_retryable_task = tasks
                    .iter()
                    .filter(|task| {
                        matches!(
                            task.status,
                            TaskStatus::Pending | TaskStatus::Cancelled | TaskStatus::InProgress
                        )
                    })
                    .min_by_key(|task| task.ordinal)
                    .cloned();
                let Some(task) = first_failed_task.or(next_retryable_task) else {
                    self.push_event(
                        EventScope::Dispatch,
                        format!("run retry ignored: run {run_id} has no retryable tasks"),
                    );
                    return Ok(());
                };
                task
            }
        };

        if requested_task_id.is_some() && failed_task.status != TaskStatus::Failed {
            self.push_event(
                EventScope::Dispatch,
                format!(
                    "task retry ignored: task {} in {run_id} is {}, only failed tasks can retry",
                    failed_task.id, failed_task.status
                ),
            );
            return Ok(());
        }

        // Reset failed steps so the handoff can re-run from the failure point.
        if let Some(run) = self.state.runs.get_mut(run_id) {
            run.reset_failed_steps();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }

        info!(
            run_id,
            task_id = %failed_task.id,
            task_ordinal = failed_task.ordinal,
            "retrying run from first failed task"
        );
        self.push_event(
            EventScope::Dispatch,
            format!("run {run_id} retrying from {}", failed_task.title),
        );

        match run.kind {
            RunKind::IssueDelivery => {
                self.retry_pipeline_task(&run, &failed_task).await?;
            },
            RunKind::PullRequestReview | RunKind::PullRequestCommentReview => {
                self.retry_pull_request_run(self.workflow(), run_id, failed_task.ordinal)
                    .await?;
            },
        }

        Ok(())
    }

    pub(crate) async fn normalize_restored_in_progress_runs(&mut self) -> Result<(), Error> {
        let now = Utc::now();
        let stale_error = "restored without an active agent session; retry the run to continue";
        let run_ids = self.state.runs.keys().cloned().collect::<Vec<_>>();

        for run_id in run_ids {
            let Some(run_snapshot) = self.state.runs.get(&run_id).cloned() else {
                continue;
            };
            let has_active_running =
                self.state.running.values().any(|running| {
                    running.run_id.as_deref() == Some(run_id.as_str())
                        || run_snapshot
                            .issue_id
                            .as_deref()
                            .is_some_and(|issue_id| running.issue.id == issue_id)
                        || run_snapshot.issue_identifier.as_deref().is_some_and(
                            |issue_identifier| running.issue.identifier == issue_identifier,
                        )
                });
            if has_active_running {
                continue;
            }

            let Some(tasks) = self.state.tasks.get_mut(&run_id) else {
                continue;
            };
            let has_stale_in_progress_task = tasks
                .iter()
                .any(|task| task.status == TaskStatus::InProgress);
            let needs_retryable_failure = has_stale_in_progress_task
                || matches!(
                    run_snapshot.status,
                    RunStatus::InProgress | RunStatus::Planning | RunStatus::Review
                ) && tasks
                    .iter()
                    .any(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Cancelled));
            if !needs_retryable_failure {
                continue;
            }

            let mut normalized_task_ids = Vec::new();
            let mut selected_fallback = false;
            for task in tasks
                .iter_mut()
                .filter(|task| task.status == TaskStatus::InProgress)
            {
                task.status = TaskStatus::Failed;
                task.error = Some(stale_error.into());
                task.finished_at = Some(now);
                task.updated_at = now;
                normalized_task_ids.push(task.id.clone());
            }
            if normalized_task_ids.is_empty()
                && let Some(task) = tasks
                    .iter_mut()
                    .find(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Cancelled))
            {
                task.status = TaskStatus::Failed;
                task.error = Some(stale_error.into());
                task.finished_at = Some(now);
                task.updated_at = now;
                normalized_task_ids.push(task.id.clone());
                selected_fallback = true;
            }
            if normalized_task_ids.is_empty() {
                continue;
            }

            if let Some(store) = &self.store {
                for task_id in &normalized_task_ids {
                    if let Some(task) = tasks.iter().find(|task| &task.id == task_id) {
                        store.save_task(task).await?;
                    }
                }
            }

            let Some(run) = self.state.runs.get_mut(&run_id) else {
                continue;
            };
            run.status = RunStatus::Failed;
            run.updated_at = now;
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }

            let normalized_count = normalized_task_ids.len();
            let reason = if selected_fallback {
                "run was restored active without an agent session"
            } else {
                "restored in-progress task had no active agent session"
            };
            self.push_event(
                EventScope::Startup,
                format!(
                    "marked stale run {run_id} as failed, {reason} ({normalized_count} task(s))"
                ),
            );
        }

        Ok(())
    }

    async fn retry_pipeline_task(&mut self, run: &Run, failed_task: &Task) -> Result<(), Error> {
        if let Some(tasks) = self.state.tasks.get_mut(&run.id)
            && let Some(task) = tasks.iter_mut().find(|task| task.id == failed_task.id)
        {
            task.status = TaskStatus::Pending;
            task.error = None;
            task.finished_at = None;
            task.started_at = None;
            task.session_id = None;
            task.thread_id = None;
            task.turns_completed = 0;
            task.tokens = TokenUsage::default();
            task.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }
        if let Some(run_row) = self.state.runs.get_mut(&run.id) {
            run_row.status = RunStatus::InProgress;
            run_row.pipeline_stage = Some(PipelineStage::Executing);
            run_row.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run_row).await?;
            }
        }

        let Some(workspace_path) = run.workspace_path.clone() else {
            return Err(Error::Core(CoreError::Adapter(format!(
                "run {} has no workspace path for retry",
                run.id
            ))));
        };
        let issue = polyphony_core::Issue {
            id: run.issue_id.clone().unwrap_or_default(),
            identifier: run.issue_identifier.clone().unwrap_or_default(),
            title: run.title.clone(),
            description: None,
            priority: None,
            state: "In Progress".into(),
            branch_name: None,
            url: None,
            author: None,
            labels: Vec::new(),
            comments: Vec::new(),
            blocked_by: Vec::new(),
            approval_state: polyphony_core::DispatchApprovalState::Approved,
            parent_id: None,
            created_at: None,
            updated_at: None,
        };
        self.dispatch_next_task(
            self.workflow(),
            issue,
            None,
            false,
            &run.id,
            &workspace_path,
        )
        .await
    }

    pub(crate) async fn process_pending_agent_stops(&mut self) {
        let stops = std::mem::take(&mut self.pending_agent_stops);
        for issue_id in stops {
            self.stop_running_by_user(&issue_id).await;
        }
    }

    pub(crate) fn visible_pull_request_event(&self, event_id: &str) -> Option<PullRequestEvent> {
        self.state
            .visible_review_events
            .get(event_id)
            .cloned()
            .map(PullRequestEvent::Review)
            .or_else(|| {
                self.state
                    .visible_comment_events
                    .get(event_id)
                    .cloned()
                    .map(PullRequestEvent::Comment)
            })
            .or_else(|| {
                self.state
                    .visible_conflict_events
                    .get(event_id)
                    .cloned()
                    .map(PullRequestEvent::Conflict)
            })
    }

    pub(crate) fn discarded_inbox_item_ttl(&self) -> chrono::Duration {
        let poll_interval_ms = self.workflow_rx.borrow().config.polling.interval_ms;
        let clamped_ms = (poll_interval_ms.saturating_mul(3)).clamp(30_000, 300_000);
        chrono::Duration::milliseconds(clamped_ms as i64)
    }

    pub(crate) fn issue_is_approved(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        issue: &Issue,
    ) -> bool {
        self.issue_approval_state(tracker_kind, issue) == DispatchApprovalState::Approved
    }

    pub(crate) fn issue_approval_state(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        issue: &Issue,
    ) -> DispatchApprovalState {
        let approval_key = approval_key_for_issue(tracker_kind, issue);
        if self.state.approved_inbox_keys.contains(&approval_key) {
            DispatchApprovalState::Approved
        } else {
            issue.approval_state
        }
    }

    pub(crate) fn tracker_issue_approval_state(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        row: &TrackerIssueRow,
    ) -> DispatchApprovalState {
        let approval_key = approval_key_for_row(tracker_kind, row);
        if self.state.approved_inbox_keys.contains(&approval_key) {
            DispatchApprovalState::Approved
        } else {
            row.approval_state
        }
    }

    pub(crate) fn resolved_tracker_issue_row(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        row: &TrackerIssueRow,
    ) -> TrackerIssueRow {
        let mut row = row.clone();
        row.approval_state = self.tracker_issue_approval_state(tracker_kind, &row);
        row
    }

    pub(crate) fn should_dispatch_tracker_issue(&self, row: &TrackerIssueRow) -> bool {
        row.approval_state == DispatchApprovalState::Approved
    }

    pub(crate) fn pull_request_event_approval_state(
        &self,
        event: &PullRequestEvent,
    ) -> DispatchApprovalState {
        let approval_key = match event {
            PullRequestEvent::Review(event) => {
                issue_key_for_source(&event.provider.to_string(), &event.dedupe_key())
            },
            PullRequestEvent::Comment(event) => {
                issue_key_for_source(&event.provider.to_string(), &event.dedupe_key())
            },
            PullRequestEvent::Conflict(event) => {
                issue_key_for_source(&event.provider.to_string(), &event.dedupe_key())
            },
        };
        if self.state.approved_inbox_keys.contains(&approval_key) {
            DispatchApprovalState::Approved
        } else {
            match event {
                PullRequestEvent::Review(event) => event.approval_state,
                PullRequestEvent::Comment(event) => event.approval_state,
                PullRequestEvent::Conflict(event) => event.approval_state,
            }
        }
    }

    pub(crate) fn issue_inbox_item_row(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        row: &TrackerIssueRow,
    ) -> InboxItemRow {
        let mut row = self.resolved_tracker_issue_row(tracker_kind, row);
        let key = sanitize_workspace_key(&row.issue_identifier);
        row.has_workspace = self.state.worktree_keys.contains(&key);
        InboxItemRow {
            repo_id: row.repo_id.clone(),
            item_id: row.issue_id.clone(),
            kind: InboxItemKind::Issue,
            source: issue_event_source(tracker_kind, &row),
            identifier: row.issue_identifier.clone(),
            title: row.title.clone(),
            status: row.state.clone(),
            approval_state: row.approval_state,
            priority: row.priority,
            labels: row.labels.clone(),
            description: row.description.clone(),
            url: row.url.clone(),
            author: row.author.clone(),
            parent_id: row.parent_id.clone(),
            updated_at: row.updated_at,
            created_at: row.created_at,
            has_workspace: row.has_workspace,
        }
    }

    pub(crate) fn run_target_label(run: &Run) -> String {
        run.review_target
            .as_ref()
            .map(|target| format!("{}#{}", target.repository, target.number))
            .or_else(|| run.issue_identifier.clone())
            .unwrap_or_else(|| run.id.clone())
    }

    pub(crate) fn pull_request_inbox_item_row(&self, event: &PullRequestEvent) -> InboxItemRow {
        match event {
            PullRequestEvent::Review(event) => InboxItemRow {
                repo_id: String::new(),
                item_id: event.dedupe_key(),
                kind: InboxItemKind::PullRequestReview,
                source: event.provider.to_string(),
                identifier: event.display_identifier(),
                title: event.title.clone(),
                status: self.pull_request_event_status(&PullRequestEvent::Review(event.clone())),
                approval_state: self
                    .pull_request_event_approval_state(&PullRequestEvent::Review(event.clone())),
                priority: None,
                labels: event.labels.clone(),
                description: Some(format!(
                    "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nCheckout ref: {}",
                    event.repository,
                    event.base_branch,
                    event.head_branch,
                    event.head_sha,
                    event.checkout_ref.as_deref().unwrap_or("<none>"),
                )),
                url: event.url.clone(),
                author: event.author_login.clone(),
                parent_id: None,
                updated_at: event.updated_at,
                created_at: event.created_at.or(event.updated_at),
                has_workspace: self
                    .state
                    .worktree_keys
                    .contains(&sanitize_workspace_key(&event.display_identifier())),
            },
            PullRequestEvent::Comment(event) => InboxItemRow {
                repo_id: String::new(),
                item_id: event.dedupe_key(),
                kind: InboxItemKind::PullRequestComment,
                source: event.provider.to_string(),
                identifier: event.display_identifier(),
                title: format!(
                    "{}{}: {}",
                    event.path,
                    event
                        .line
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default(),
                    truncate_for_inbox_title(&event.body, 72),
                ),
                status: self.pull_request_event_status(&PullRequestEvent::Comment(event.clone())),
                approval_state: self
                    .pull_request_event_approval_state(&PullRequestEvent::Comment(event.clone())),
                priority: None,
                labels: event.labels.clone(),
                description: Some(format!(
                    "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nPath: {}\nLine: {}\nComment author: {}\n\n{}",
                    event.repository,
                    event.base_branch,
                    event.head_branch,
                    event.head_sha,
                    event.path,
                    event
                        .line
                        .map(|line| line.to_string())
                        .unwrap_or_else(|| "<none>".into()),
                    event
                        .author_login
                        .clone()
                        .unwrap_or_else(|| "<unknown>".into()),
                    event.body,
                )),
                url: event.url.clone(),
                author: event.author_login.clone(),
                parent_id: None,
                updated_at: event.updated_at.or(event.created_at),
                created_at: event.created_at.or(event.updated_at),
                has_workspace: self
                    .state
                    .worktree_keys
                    .contains(&sanitize_workspace_key(&event.display_identifier())),
            },
            PullRequestEvent::Conflict(event) => InboxItemRow {
                repo_id: String::new(),
                item_id: event.dedupe_key(),
                kind: InboxItemKind::PullRequestConflict,
                source: event.provider.to_string(),
                identifier: event.display_identifier(),
                title: format!(
                    "conflicts with {}: {}",
                    event.base_branch, event.pull_request_title
                ),
                status: self.pull_request_event_status(&PullRequestEvent::Conflict(event.clone())),
                approval_state: self
                    .pull_request_event_approval_state(&PullRequestEvent::Conflict(event.clone())),
                priority: None,
                labels: event.labels.clone(),
                description: Some(format!(
                    "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nMergeable: {}\nMerge state: {}",
                    event.repository,
                    event.base_branch,
                    event.head_branch,
                    event.head_sha,
                    event.mergeable_state,
                    event.merge_state_status,
                )),
                url: event.url.clone(),
                author: event.author_login.clone(),
                parent_id: None,
                updated_at: event.updated_at.or(event.created_at),
                created_at: event.created_at.or(event.updated_at),
                has_workspace: self
                    .state
                    .worktree_keys
                    .contains(&sanitize_workspace_key(&event.display_identifier())),
            },
        }
    }

    pub(crate) fn record_discarded_inbox_item(&mut self, mut row: InboxItemRow) {
        let became_discarded = !self.state.discarded_inbox_items.contains_key(&row.item_id);
        let identifier = row.identifier.clone();
        let kind = row.kind;
        row.status = "already_fixed".into();
        row.updated_at = Some(Utc::now());
        self.state
            .discarded_inbox_items
            .insert(row.item_id.clone(), DiscardedInboxItemEntry {
                row,
                discarded_at: Utc::now(),
            });
        if became_discarded {
            self.push_event(
                EventScope::Tracker,
                format!("{kind} {identifier} is already fixed"),
            );
        }
    }

    pub(crate) fn prune_discarded_inbox_items(&mut self) {
        let ttl = self.discarded_inbox_item_ttl();
        let now = Utc::now();
        self.state
            .discarded_inbox_items
            .retain(|_, entry| now.signed_duration_since(entry.discarded_at) < ttl);
    }

    pub(crate) fn issue_is_actionable(&self, issue_id: &str) -> bool {
        self.state.running.contains_key(issue_id)
            || self.state.retrying.contains_key(issue_id)
            || self.is_claimed(issue_id)
    }

    pub(crate) async fn process_manual_pull_request_inbox_dispatches(&mut self) {
        let dispatches = std::mem::take(&mut self.pending_manual_pull_request_inbox_dispatches);
        if dispatches.is_empty() {
            return;
        }
        if self.state.dispatch_mode == polyphony_core::DispatchMode::Stop {
            info!("manual PR inbox dispatches dropped (stop mode)");
            self.push_event(
                EventScope::Dispatch,
                "manual PR inbox dispatch blocked: orchestrator is in stop mode".into(),
            );
            return;
        }
        info!(
            count = dispatches.len(),
            "processing manual pull request inbox dispatches"
        );
        let workflow = self.workflow();
        for request in dispatches {
            let item_id = request.item_id;
            let Some(event) = self.visible_pull_request_event(&item_id) else {
                self.push_event(
                    EventScope::Dispatch,
                    format!("manual dispatch: pull request inbox item {item_id} not found"),
                );
                warn!(%item_id, "pull request inbox item not found for manual dispatch");
                continue;
            };
            if let Some(reason) = self.pull_request_event_suppression(&workflow, &event) {
                let status = self.pull_request_event_status(&event);
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "manual dispatch skipped: {} is {status} ({reason:?})",
                        event.display_identifier()
                    ),
                );
                continue;
            }
            if let Err(error) = self
                .dispatch_pull_request_event(
                    workflow.clone(),
                    event.clone(),
                    None,
                    request.directives.as_deref(),
                )
                .await
            {
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "manual dispatch failed: {} ({error})",
                        event.display_identifier()
                    ),
                );
                error!(
                    %error,
                    item_id = %event.dedupe_key(),
                    "manual pull request inbox dispatch failed"
                );
            }
        }
    }

    pub(crate) async fn dispatch_requested_issue(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        agent_name: Option<&str>,
        directives: Option<&str>,
        source: &'static str,
    ) {
        let skip_workspace_sync = source == "orphan auto-dispatch";
        if self.state.running.contains_key(&issue.id) {
            let msg = format!("{source} skipped: {} already running", issue.identifier);
            info!(issue_id = %issue.id, "{msg}");
            self.push_event(EventScope::Dispatch, msg);
            return;
        }
        if !self.has_available_slot(&workflow, &issue.state) {
            let msg = format!("{source} skipped: no slot for {}", issue.identifier);
            warn!(issue_id = %issue.id, "{msg}");
            self.push_event(EventScope::Dispatch, msg);
            return;
        }
        info!(
            issue_identifier = %issue.identifier,
            issue_id = %issue.id,
            agent = agent_name.unwrap_or("default"),
            dispatch_source = source,
            "dispatching requested issue"
        );
        self.push_event(
            EventScope::Dispatch,
            format!(
                "{source}: {} → {}",
                issue.identifier,
                agent_name.unwrap_or("default")
            ),
        );
        if let Err(error) = self
            .dispatch_issue(
                workflow,
                issue,
                None,
                false,
                agent_name,
                skip_workspace_sync,
                directives,
            )
            .await
        {
            self.push_event(EventScope::Dispatch, format!("{source} failed: {error}"));
            error!(%error, dispatch_source = source, "requested dispatch failed");
        }
    }

    pub(crate) fn idle_dispatch_allowed_for_agents(
        &self,
        candidate_agents: &[polyphony_core::AgentDefinition],
    ) -> bool {
        if !self.state.running.is_empty() {
            return false;
        }

        let budgets = candidate_agents
            .iter()
            .filter_map(|agent| self.state.budgets.get(&format!("agent:{}", agent.name)))
            .collect::<Vec<_>>();

        if budgets.is_empty() {
            return false;
        }

        if budgets
            .iter()
            .any(|budget| budget.has_weekly_credit_deficit())
        {
            return false;
        }

        budgets.iter().any(|budget| budget.has_credit_headroom())
    }

    pub(crate) fn idle_dispatch_allowed_for_issue(
        &self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
    ) -> Result<bool, Error> {
        let candidate_agents = workflow.config.candidate_agents_for_issue(issue)?;
        Ok(self.idle_dispatch_allowed_for_agents(&candidate_agents))
    }

    pub(crate) fn idle_dispatch_allowed_for_pr_reviews(
        &self,
        workflow: &LoadedWorkflow,
    ) -> Result<bool, Error> {
        let Some(review_agent) = workflow.config.pr_review_agent()? else {
            return Ok(false);
        };
        let candidate_agents = workflow
            .config
            .expand_agent_candidates(&review_agent.name)?;
        Ok(self.idle_dispatch_allowed_for_agents(&candidate_agents))
    }

    pub(crate) async fn tick(&mut self) -> bool {
        self.reload_workflow_from_disk(false, "poll_tick").await;

        if self.drain_commands() {
            return true;
        }

        self.process_pending_inbox_approvals().await;
        self.process_pending_issue_closures().await;
        self.process_pending_deliverable_resolutions().await;
        // Process merge requests
        let merge_ids = std::mem::take(&mut self.pending_merge_deliverables);
        for run_id in merge_ids {
            self.merge_deliverable(&run_id).await;
        }

        self.process_pending_run_retries().await;
        self.process_pending_task_resolutions().await;
        self.process_pending_task_retries().await;
        self.process_pending_agent_stops().await;
        self.process_manual_dispatches().await;
        self.process_manual_pull_request_inbox_dispatches().await;
        // Emit snapshot immediately after dispatches so runs appear in the TUI
        let _ = self.emit_snapshot().await;

        debug!("tick: reconciling running sessions");
        self.state.loading.reconciling = true;
        let _ = self.emit_snapshot().await;
        self.reconcile_running().await;
        self.state.loading.reconciling = false;

        if self.drain_commands() {
            return true;
        }

        self.refresh_tracker_connection(false).await;

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
        let tracker_component = self.tracker.component_key();
        let tracker_fetch_started = Instant::now();
        self.state.last_tracker_poll_at = Some(Utc::now());
        self.state.loading.fetching_issues = true;
        info!("tick: fetching issues from tracker");
        let _ = self.emit_snapshot().await;
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.state.loading.fetching_issues = false;
                let elapsed = tracker_fetch_started.elapsed();
                warn!("tick: tracker returned rate-limited, re-throttling");
                if elapsed >= SLOW_TRACKER_FETCH_WARN_THRESHOLD {
                    warn!(
                        component = %tracker_component,
                        elapsed_ms = elapsed.as_millis(),
                        "tracker candidate fetch was slow before rate limiting"
                    );
                }
                self.register_throttle(*signal);
                let _ = self.emit_snapshot().await;
                return false;
            },
            Err(error) => {
                self.state.loading.fetching_issues = false;
                let elapsed = tracker_fetch_started.elapsed();
                self.push_event(
                    EventScope::Tracker,
                    format!("candidate fetch failed: {error}"),
                );
                if elapsed >= SLOW_TRACKER_FETCH_WARN_THRESHOLD {
                    warn!(
                        component = %tracker_component,
                        elapsed_ms = elapsed.as_millis(),
                        %error,
                        "tracker candidate fetch failed after a slow request"
                    );
                }
                error!(%error, "candidate fetch failed");
                let _ = self.emit_snapshot().await;
                return false;
            },
        };
        let tracker_fetch_elapsed = tracker_fetch_started.elapsed();
        self.state.loading.fetching_issues = false;
        self.state.from_cache = false;
        self.state.cached_at = None;
        info!(
            component = %tracker_component,
            count = issues.len(),
            elapsed_ms = tracker_fetch_elapsed.as_millis(),
            "tick: fetched issues from tracker"
        );
        if tracker_fetch_elapsed >= SLOW_TRACKER_FETCH_WARN_THRESHOLD {
            warn!(
                component = %tracker_component,
                count = issues.len(),
                elapsed_ms = tracker_fetch_elapsed.as_millis(),
                "tracker candidate fetch exceeded the slow-fetch threshold"
            );
        }

        let previous_issue_rows = self.state.tracker_issues.clone();
        let mut issues = issues;
        issues.sort_by(dispatch_order);
        self.state.tracker_issue_snapshot_loaded = true;
        let tracker_kind = workflow.config.tracker.kind;
        self.state.tracker_issues = issues
            .iter()
            .map(summarize_issue)
            .map(|row| self.resolved_tracker_issue_row(tracker_kind, &row))
            .collect();
        let current_issue_ids = self
            .state
            .tracker_issues
            .iter()
            .map(|issue| issue.issue_id.clone())
            .collect::<HashSet<_>>();
        for issue_row in previous_issue_rows {
            if current_issue_ids.contains(&issue_row.issue_id) {
                self.state.discarded_inbox_items.remove(&issue_row.issue_id);
                continue;
            }
            if self.issue_is_actionable(&issue_row.issue_id) {
                continue;
            }
            self.record_discarded_inbox_item(self.issue_inbox_item_row(tracker_kind, &issue_row));
        }
        self.prune_discarded_inbox_items();
        self.save_cache().await;

        // Auto-dispatch issues whose orphaned workspaces were found at startup.
        // Skip in stop mode — nothing should start.
        if !self.state.orphan_dispatch_keys.is_empty()
            && self.state.dispatch_mode != polyphony_core::DispatchMode::Stop
        {
            let orphan_keys = std::mem::take(&mut self.state.orphan_dispatch_keys);
            let mut pending_orphan_keys = orphan_keys.clone();
            for issue in &issues {
                let key = sanitize_workspace_key(&issue.identifier);
                if orphan_keys.contains(&key)
                    && !self.is_claimed(&issue.id)
                    && self.issue_is_approved(tracker_kind, issue)
                {
                    info!(
                        issue_identifier = %issue.identifier,
                        workspace_key = %key,
                        "auto-dispatching orphaned workspace issue"
                    );
                    self.push_event(
                        EventScope::Dispatch,
                        format!("auto-dispatch orphaned: {}", issue.identifier),
                    );
                    self.dispatch_requested_issue(
                        workflow.clone(),
                        issue.clone(),
                        None,
                        None,
                        "orphan auto-dispatch",
                    )
                    .await;
                    pending_orphan_keys.remove(&key);
                }
            }
            self.state.orphan_dispatch_keys = pending_orphan_keys;
            // Emit snapshot so orphan events and dispatch state updates become visible immediately.
            let _ = self.emit_snapshot().await;
        }

        if self.pull_request_event_source.is_some() {
            let allow_pull_request_dispatch = match self.state.dispatch_mode {
                polyphony_core::DispatchMode::Manual | polyphony_core::DispatchMode::Stop => false,
                polyphony_core::DispatchMode::Automatic
                | polyphony_core::DispatchMode::Nightshift => true,
                polyphony_core::DispatchMode::Idle => {
                    match self.idle_dispatch_allowed_for_pr_reviews(&workflow) {
                        Ok(allowed) => allowed,
                        Err(error) => {
                            self.push_event(
                                EventScope::Dispatch,
                                format!("idle PR review gate failed: {error}"),
                            );
                            false
                        },
                    }
                },
            };
            self.poll_pull_request_events(workflow.clone(), allow_pull_request_dispatch)
                .await;
        }
        if !workflow.config.has_dispatch_agents() {
            let _ = self.emit_snapshot().await;
            return false;
        }
        if matches!(
            self.state.dispatch_mode,
            polyphony_core::DispatchMode::Manual | polyphony_core::DispatchMode::Stop
        ) {
            debug!("tick: dispatch skipped ({} mode)", self.state.dispatch_mode);
            let _ = self.emit_snapshot().await;
            return false;
        }
        for issue in issues {
            if !self.issue_is_approved(tracker_kind, &issue) {
                continue;
            }
            if !self.should_dispatch(&workflow, &issue) {
                continue;
            }
            if self.state.dispatch_mode == polyphony_core::DispatchMode::Idle {
                match self.idle_dispatch_allowed_for_issue(&workflow, &issue) {
                    Ok(true) => {},
                    Ok(false) => continue,
                    Err(error) => {
                        self.push_event(
                            EventScope::Dispatch,
                            format!(
                                "idle dispatch gate failed for {}: {error}",
                                issue.identifier
                            ),
                        );
                        continue;
                    },
                }
            }
            if !self.has_available_slot(&workflow, &issue.state) {
                break;
            }
            if let Err(error) = self
                .dispatch_issue(workflow.clone(), issue, None, false, None, false, None)
                .await
            {
                self.push_event(EventScope::Dispatch, format!("dispatch failed: {error}"));
                error!(%error, "dispatch failed");
            }
        }
        let _ = self.emit_snapshot().await;
        false
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use chrono::Utc;
    use polyphony_core::{
        RunKind, RunRow, RunStatus, RuntimeCadence, RuntimeSnapshot, SnapshotCounts, TaskCategory,
        TaskRow, TaskStatus, WorkspaceProgressUpdate,
    };

    use super::apply_workspace_progress_to_snapshot;

    #[test]
    fn workspace_progress_updates_current_snapshot_immediately() {
        let now = Utc::now();
        let mut snapshot = RuntimeSnapshot {
            repo_ids: Vec::new(), repo_registrations: Vec::new(),
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            tracker_issues: Vec::new(),
            inbox_items: Vec::new(),
            approved_inbox_keys: Vec::new(),
            running: Vec::new(),
            agent_run_history: Vec::new(),
            retrying: Vec::new(),
            codex_totals: Default::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: Vec::new(),
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            runs: vec![RunRow {
                repo_id: String::new(),
                id: "run-1".into(),
                kind: RunKind::PullRequestReview,
                issue_identifier: Some("penso/arbor#89".into()),
                title: "Review me".into(),
                status: RunStatus::InProgress,
                task_count: 2,
                tasks_completed: 0,
                deliverable: None,
                has_deliverable: false,
                review_target: None,
                workspace_key: Some("penso_arbor_89".into()),
                workspace_path: None,
                created_at: now,
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
            }],
            tasks: vec![TaskRow {
                repo_id: String::new(),
                id: "task-1".into(),
                run_id: "run-1".into(),
                title: "Creating worktree".into(),
                description: None,
                activity_log: Vec::new(),
                category: TaskCategory::Research,
                status: TaskStatus::InProgress,
                ordinal: 0,
                agent_name: Some("orchestrator".into()),
                turns_completed: 0,
                total_tokens: 0,
                started_at: Some(now),
                finished_at: None,
                error: None,
                created_at: now,
                updated_at: now,
            }],
            loading: Default::default(),
            dispatch_mode: Default::default(),
            tracker_kind: Default::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
        };

        apply_workspace_progress_to_snapshot(&mut snapshot, &WorkspaceProgressUpdate {
            issue_identifier: "penso/arbor#89".into(),
            workspace_key: "penso_arbor_89".into(),
            message: "Fetching origin".into(),
            at: now,
        });
        apply_workspace_progress_to_snapshot(&mut snapshot, &WorkspaceProgressUpdate {
            issue_identifier: "penso/arbor#89".into(),
            workspace_key: "penso_arbor_89".into(),
            message: "Fetching origin".into(),
            at: now + chrono::Duration::seconds(1),
        });

        assert_eq!(snapshot.tasks[0].activity_log.len(), 1);
        assert!(snapshot.tasks[0].activity_log[0].ends_with("Fetching origin"));
    }
}
