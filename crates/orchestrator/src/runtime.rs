use crate::{prelude::*, *};

const SLOW_TRACKER_FETCH_WARN_THRESHOLD: Duration = Duration::from_millis(750);

impl RuntimeService {
    pub fn new(
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
    ) -> (Self, RuntimeHandle) {
        let (snapshot_tx, snapshot_rx) = watch::channel(empty_snapshot());
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (external_command_tx, external_command_rx) = mpsc::unbounded_channel();
        let initial_dispatch_mode = workflow_rx.borrow().config.startup_dispatch_mode();
        let state = RuntimeState {
            dispatch_mode: initial_dispatch_mode,
            ..RuntimeState::default()
        };
        (
            Self {
                tracker,
                pull_request_trigger_source,
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
                pending_issue_approvals: Vec::new(),
                pending_deliverable_resolutions: Vec::new(),
                pending_manual_dispatches: Vec::new(),
                pending_manual_pull_request_trigger_dispatches: Vec::new(),
                pending_merge_deliverables: Vec::new(),
                pending_task_resolutions: Vec::new(),
                pending_task_retries: Vec::new(),
                reload_support: None,
                state,
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
                        RuntimeCommand::ApproveIssueTrigger { issue_id, source } => {
                            info!(%issue_id, %source, "issue approval queued (event loop)");
                            self.pending_issue_approvals.push((issue_id, source));
                            self.process_pending_issue_approvals().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::ResolveMovementDeliverable {
                            movement_id,
                            decision,
                        } => {
                            self.pending_deliverable_resolutions
                                .push((movement_id, decision));
                            self.process_pending_deliverable_resolutions().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::DispatchIssue { issue_id, agent_name } => {
                            info!(%issue_id, ?agent_name, "manual dispatch queued (event loop)");
                            self.pending_manual_dispatches.push((issue_id, agent_name));
                            next_tick = Instant::now();
                        }
                        RuntimeCommand::DispatchPullRequestTrigger { trigger_id } => {
                            info!(%trigger_id, "manual pull request trigger dispatch queued (event loop)");
                            self.pending_manual_pull_request_trigger_dispatches
                                .push(trigger_id);
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::MergeDeliverable { movement_id } => {
                            info!(%movement_id, "merge deliverable requested (event loop)");
                            self.merge_deliverable(&movement_id).await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::ResolveTask {
                            movement_id,
                            task_id,
                        } => {
                            info!(%movement_id, %task_id, "manual task resolution requested");
                            self.pending_task_resolutions
                                .push((movement_id, task_id));
                            self.process_pending_task_resolutions().await;
                            let _ = self.emit_snapshot().await;
                        }
                        RuntimeCommand::RetryTask {
                            movement_id,
                            task_id,
                        } => {
                            info!(%movement_id, %task_id, "task retry requested");
                            self.pending_task_retries
                                .push((movement_id, task_id));
                            self.process_pending_task_retries().await;
                            let _ = self.emit_snapshot().await;
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
                Ok(RuntimeCommand::ApproveIssueTrigger { issue_id, source }) => {
                    info!(%issue_id, %source, "issue approval queued");
                    self.pending_issue_approvals.push((issue_id, source));
                },
                Ok(RuntimeCommand::ResolveMovementDeliverable {
                    movement_id,
                    decision,
                }) => {
                    self.pending_deliverable_resolutions
                        .push((movement_id, decision));
                },
                Ok(RuntimeCommand::DispatchIssue {
                    issue_id,
                    agent_name,
                }) => {
                    info!(%issue_id, ?agent_name, "manual dispatch queued");
                    self.pending_manual_dispatches.push((issue_id, agent_name));
                },
                Ok(RuntimeCommand::DispatchPullRequestTrigger { trigger_id }) => {
                    info!(%trigger_id, "manual pull request trigger dispatch queued");
                    self.pending_manual_pull_request_trigger_dispatches
                        .push(trigger_id);
                },
                Ok(RuntimeCommand::MergeDeliverable { movement_id }) => {
                    info!(%movement_id, "merge deliverable queued");
                    self.pending_merge_deliverables.push(movement_id);
                },
                Ok(RuntimeCommand::ResolveTask {
                    movement_id,
                    task_id,
                }) => {
                    info!(%movement_id, %task_id, "manual task resolution queued");
                    self.pending_task_resolutions
                        .push((movement_id, task_id));
                },
                Ok(RuntimeCommand::RetryTask {
                    movement_id,
                    task_id,
                }) => {
                    info!(%movement_id, %task_id, "task retry queued");
                    self.pending_task_retries
                        .push((movement_id, task_id));
                },
                Err(_) => return false,
            }
        }
    }

    pub(crate) async fn process_pending_issue_approvals(&mut self) {
        let approvals = std::mem::take(&mut self.pending_issue_approvals);
        if approvals.is_empty() {
            return;
        }
        let workflow = self.workflow();
        for (issue_id, source) in approvals {
            let approval_key = issue_key_for_source(&source, &issue_id);
            if !self.state.approved_issue_keys.insert(approval_key.clone()) {
                self.push_event(
                    EventScope::Dispatch,
                    format!("{source} issue {issue_id} is already approved"),
                );
                continue;
            }
            self.push_event(
                EventScope::Dispatch,
                format!("{source} issue {issue_id} approved for dispatch"),
            );
            if let Some(store) = &self.store {
                let snapshot = self.snapshot();
                if let Err(error) = store.save_snapshot(&snapshot).await {
                    self.push_event(
                        EventScope::Dispatch,
                        format!(
                            "{source} issue {issue_id} approved but failed to persist: {error}"
                        ),
                    );
                }
            }
            if self.state.dispatch_mode != polyphony_core::DispatchMode::Manual {
                continue;
            }
            if let Some(row) = self
                .state
                .visible_issues
                .iter()
                .find(|row| approval_key_for_row(workflow.config.tracker.kind, row) == approval_key)
                .map(|row| self.resolved_issue_row(workflow.config.tracker.kind, row))
                && self.should_dispatch_visible_row(&row)
            {
                let issue_identifier = row.issue_identifier.clone();
                self.push_event(
                    EventScope::Dispatch,
                    format!("{issue_identifier} approved and ready for manual dispatch"),
                );
            }
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
        for (issue_id, agent_name) in dispatches {
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
                agent_name.as_deref(),
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

        for (movement_id, decision) in resolutions {
            let Some(movement) = self.state.movements.get_mut(&movement_id) else {
                self.push_event(
                    EventScope::Handoff,
                    format!("deliverable decision ignored: movement {movement_id} not found"),
                );
                continue;
            };
            let movement_label = Self::movement_target_label(movement);
            let Some(deliverable) = movement.deliverable.as_mut() else {
                let message = format!(
                    "deliverable decision ignored: movement {movement_label} has no deliverable"
                );
                self.push_event(EventScope::Handoff, message);
                continue;
            };

            deliverable.decision = decision;
            movement.updated_at = Utc::now();
            let persist_error = if let Some(store) = &self.store {
                store
                    .save_movement(movement)
                    .await
                    .err()
                    .map(|error| error.to_string())
            } else {
                None
            };
            let message = if let Some(error) = persist_error {
                format!(
                    "{movement_label} deliverable marked {decision} but failed to persist: {error}"
                )
            } else {
                format!("{movement_label} deliverable marked {decision}")
            };
            self.push_event(EventScope::Handoff, message);
        }
    }

    pub(crate) async fn process_pending_task_resolutions(&mut self) {
        let resolutions = std::mem::take(&mut self.pending_task_resolutions);
        if resolutions.is_empty() {
            return;
        }

        for (movement_id, task_id) in resolutions {
            // Mark the task as completed
            let task_found = if let Some(tasks) = self.state.tasks.get_mut(&movement_id) {
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
                    format!("task resolution ignored: task {task_id} not found in {movement_id}"),
                );
                continue;
            }

            // Reset the movement from Failed back to Executing so the pipeline continues
            if let Some(movement) = self.state.movements.get_mut(&movement_id) {
                movement.status = MovementStatus::InProgress;
                movement.pipeline_stage = Some(PipelineStage::Executing);
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    let _ = store.save_movement(movement).await;
                }
            }

            info!(
                movement_id,
                task_id,
                "task manually resolved — resuming pipeline"
            );
            self.push_event(
                EventScope::Dispatch,
                format!("task {task_id} manually resolved, pipeline resuming"),
            );

            // Build a minimal Issue from the movement and dispatch next task
            let movement_info = self.state.movements.get(&movement_id).map(|m| {
                (
                    m.issue_id.clone().unwrap_or_default(),
                    m.issue_identifier.clone().unwrap_or_default(),
                    m.title.clone(),
                    m.workspace_path.clone(),
                )
            });
            if let Some((issue_id, identifier, title, Some(ws))) = movement_info {
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
                    approval_state: polyphony_core::IssueApprovalState::Approved,
                    parent_id: None,
                    created_at: None,
                    updated_at: None,
                };
                if let Err(error) = self
                    .dispatch_next_task(self.workflow(), issue, None, false, &movement_id, &ws)
                    .await
                {
                    warn!(%error, movement_id, "failed to dispatch next task after manual resolution");
                }
            }
        }
    }

    pub(crate) async fn process_pending_task_retries(&mut self) {
        let retries = std::mem::take(&mut self.pending_task_retries);
        if retries.is_empty() {
            return;
        }

        for (movement_id, task_id) in retries {
            // Reset the task to Pending
            let task_found = if let Some(tasks) = self.state.tasks.get_mut(&movement_id) {
                if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = polyphony_core::TaskStatus::Pending;
                    task.error = None;
                    task.finished_at = None;
                    task.started_at = None;
                    task.turns_completed = 0;
                    task.tokens = TokenUsage::default();
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
                    format!("task retry ignored: task {task_id} not found in {movement_id}"),
                );
                continue;
            }

            // Reset the movement back to Executing
            if let Some(movement) = self.state.movements.get_mut(&movement_id) {
                movement.status = MovementStatus::InProgress;
                movement.pipeline_stage = Some(PipelineStage::Executing);
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    let _ = store.save_movement(movement).await;
                }
            }

            info!(
                movement_id,
                task_id,
                "task reset to pending — dispatching retry"
            );
            self.push_event(
                EventScope::Dispatch,
                format!("task {task_id} retrying"),
            );

            // Build a minimal Issue and dispatch
            let movement_info = self.state.movements.get(&movement_id).map(|m| {
                (
                    m.issue_id.clone().unwrap_or_default(),
                    m.issue_identifier.clone().unwrap_or_default(),
                    m.title.clone(),
                    m.workspace_path.clone(),
                )
            });
            if let Some((issue_id, identifier, title, Some(ws))) = movement_info {
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
                    approval_state: polyphony_core::IssueApprovalState::Approved,
                    parent_id: None,
                    created_at: None,
                    updated_at: None,
                };
                if let Err(error) = self
                    .dispatch_next_task(self.workflow(), issue, None, false, &movement_id, &ws)
                    .await
                {
                    warn!(%error, movement_id, "failed to dispatch task retry");
                }
            }
        }
    }

    pub(crate) fn visible_pull_request_trigger(
        &self,
        trigger_id: &str,
    ) -> Option<PullRequestTrigger> {
        self.state
            .visible_review_triggers
            .get(trigger_id)
            .cloned()
            .map(PullRequestTrigger::Review)
            .or_else(|| {
                self.state
                    .visible_comment_triggers
                    .get(trigger_id)
                    .cloned()
                    .map(PullRequestTrigger::Comment)
            })
            .or_else(|| {
                self.state
                    .visible_conflict_triggers
                    .get(trigger_id)
                    .cloned()
                    .map(PullRequestTrigger::Conflict)
            })
    }

    pub(crate) fn discarded_trigger_ttl(&self) -> chrono::Duration {
        let poll_interval_ms = self.workflow_rx.borrow().config.polling.interval_ms;
        let clamped_ms = (poll_interval_ms.saturating_mul(3)).clamp(30_000, 300_000);
        chrono::Duration::milliseconds(clamped_ms as i64)
    }

    pub(crate) fn issue_is_approved(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        issue: &Issue,
    ) -> bool {
        self.issue_approval_state(tracker_kind, issue) == IssueApprovalState::Approved
    }

    pub(crate) fn issue_approval_state(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        issue: &Issue,
    ) -> IssueApprovalState {
        let approval_key = approval_key_for_issue(tracker_kind, issue);
        if self.state.approved_issue_keys.contains(&approval_key) {
            IssueApprovalState::Approved
        } else {
            issue.approval_state
        }
    }

    pub(crate) fn visible_issue_approval_state(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        row: &VisibleIssueRow,
    ) -> IssueApprovalState {
        let approval_key = approval_key_for_row(tracker_kind, row);
        if self.state.approved_issue_keys.contains(&approval_key) {
            IssueApprovalState::Approved
        } else {
            row.approval_state
        }
    }

    pub(crate) fn resolved_issue_row(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        row: &VisibleIssueRow,
    ) -> VisibleIssueRow {
        let mut row = row.clone();
        row.approval_state = self.visible_issue_approval_state(tracker_kind, &row);
        row
    }

    pub(crate) fn should_dispatch_visible_row(&self, row: &VisibleIssueRow) -> bool {
        row.approval_state == IssueApprovalState::Approved
    }

    pub(crate) fn issue_trigger_row(
        &self,
        tracker_kind: polyphony_core::TrackerKind,
        row: &VisibleIssueRow,
    ) -> VisibleTriggerRow {
        let mut row = self.resolved_issue_row(tracker_kind, row);
        let key = sanitize_workspace_key(&row.issue_identifier);
        row.has_workspace = self.state.worktree_keys.contains(&key);
        VisibleTriggerRow {
            trigger_id: row.issue_id.clone(),
            kind: VisibleTriggerKind::Issue,
            source: issue_trigger_source(tracker_kind, &row),
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

    pub(crate) fn movement_target_label(movement: &Movement) -> String {
        movement
            .review_target
            .as_ref()
            .map(|target| format!("{}#{}", target.repository, target.number))
            .or_else(|| movement.issue_identifier.clone())
            .unwrap_or_else(|| movement.id.clone())
    }

    pub(crate) fn pull_request_trigger_row(
        &self,
        trigger: &PullRequestTrigger,
    ) -> VisibleTriggerRow {
        match trigger {
            PullRequestTrigger::Review(trigger) => VisibleTriggerRow {
                trigger_id: trigger.dedupe_key(),
                kind: VisibleTriggerKind::PullRequestReview,
                source: trigger.provider.to_string(),
                identifier: trigger.display_identifier(),
                title: trigger.title.clone(),
                status: self
                    .pull_request_trigger_status(&PullRequestTrigger::Review(trigger.clone())),
                approval_state: IssueApprovalState::Approved,
                priority: None,
                labels: trigger.labels.clone(),
                description: Some(format!(
                    "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nCheckout ref: {}",
                    trigger.repository,
                    trigger.base_branch,
                    trigger.head_branch,
                    trigger.head_sha,
                    trigger.checkout_ref.as_deref().unwrap_or("<none>"),
                )),
                url: trigger.url.clone(),
                author: trigger.author_login.clone(),
                parent_id: None,
                updated_at: trigger.updated_at,
                created_at: trigger.created_at.or(trigger.updated_at),
                has_workspace: self
                    .state
                    .worktree_keys
                    .contains(&sanitize_workspace_key(&trigger.display_identifier())),
            },
            PullRequestTrigger::Comment(trigger) => VisibleTriggerRow {
                trigger_id: trigger.dedupe_key(),
                kind: VisibleTriggerKind::PullRequestComment,
                source: trigger.provider.to_string(),
                identifier: trigger.display_identifier(),
                title: format!(
                    "{}{}: {}",
                    trigger.path,
                    trigger
                        .line
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default(),
                    truncate_for_trigger_title(&trigger.body, 72),
                ),
                status: self
                    .pull_request_trigger_status(&PullRequestTrigger::Comment(trigger.clone())),
                approval_state: IssueApprovalState::Approved,
                priority: None,
                labels: trigger.labels.clone(),
                description: Some(format!(
                    "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nPath: {}\nLine: {}\nComment author: {}\n\n{}",
                    trigger.repository,
                    trigger.base_branch,
                    trigger.head_branch,
                    trigger.head_sha,
                    trigger.path,
                    trigger
                        .line
                        .map(|line| line.to_string())
                        .unwrap_or_else(|| "<none>".into()),
                    trigger
                        .author_login
                        .clone()
                        .unwrap_or_else(|| "<unknown>".into()),
                    trigger.body,
                )),
                url: trigger.url.clone(),
                author: trigger.author_login.clone(),
                parent_id: None,
                updated_at: trigger.updated_at.or(trigger.created_at),
                created_at: trigger.created_at.or(trigger.updated_at),
                has_workspace: self
                    .state
                    .worktree_keys
                    .contains(&sanitize_workspace_key(&trigger.display_identifier())),
            },
            PullRequestTrigger::Conflict(trigger) => VisibleTriggerRow {
                trigger_id: trigger.dedupe_key(),
                kind: VisibleTriggerKind::PullRequestConflict,
                source: trigger.provider.to_string(),
                identifier: trigger.display_identifier(),
                title: format!(
                    "conflicts with {}: {}",
                    trigger.base_branch, trigger.pull_request_title
                ),
                status: self
                    .pull_request_trigger_status(&PullRequestTrigger::Conflict(trigger.clone())),
                approval_state: IssueApprovalState::Approved,
                priority: None,
                labels: trigger.labels.clone(),
                description: Some(format!(
                    "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nMergeable: {}\nMerge state: {}",
                    trigger.repository,
                    trigger.base_branch,
                    trigger.head_branch,
                    trigger.head_sha,
                    trigger.mergeable_state,
                    trigger.merge_state_status,
                )),
                url: trigger.url.clone(),
                author: trigger.author_login.clone(),
                parent_id: None,
                updated_at: trigger.updated_at.or(trigger.created_at),
                created_at: trigger.created_at.or(trigger.updated_at),
                has_workspace: self
                    .state
                    .worktree_keys
                    .contains(&sanitize_workspace_key(&trigger.display_identifier())),
            },
        }
    }

    pub(crate) fn record_discarded_trigger(&mut self, mut row: VisibleTriggerRow) {
        let became_discarded = !self.state.discarded_triggers.contains_key(&row.trigger_id);
        let identifier = row.identifier.clone();
        let kind = row.kind;
        row.status = "already_fixed".into();
        row.updated_at = Some(Utc::now());
        self.state
            .discarded_triggers
            .insert(row.trigger_id.clone(), DiscardedTriggerEntry {
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

    pub(crate) fn prune_discarded_triggers(&mut self) {
        let ttl = self.discarded_trigger_ttl();
        let now = Utc::now();
        self.state
            .discarded_triggers
            .retain(|_, entry| now.signed_duration_since(entry.discarded_at) < ttl);
    }

    pub(crate) fn issue_is_actionable(&self, issue_id: &str) -> bool {
        self.state.running.contains_key(issue_id)
            || self.state.retrying.contains_key(issue_id)
            || self.is_claimed(issue_id)
    }

    pub(crate) async fn process_manual_pull_request_trigger_dispatches(&mut self) {
        let trigger_ids = std::mem::take(&mut self.pending_manual_pull_request_trigger_dispatches);
        if trigger_ids.is_empty() {
            return;
        }
        if self.state.dispatch_mode == polyphony_core::DispatchMode::Stop {
            info!("manual PR trigger dispatches dropped (stop mode)");
            self.push_event(
                EventScope::Dispatch,
                "manual PR trigger dispatch blocked: orchestrator is in stop mode".into(),
            );
            return;
        }
        info!(
            count = trigger_ids.len(),
            "processing manual pull request trigger dispatches"
        );
        let workflow = self.workflow();
        for trigger_id in trigger_ids {
            let Some(trigger) = self.visible_pull_request_trigger(&trigger_id) else {
                self.push_event(
                    EventScope::Dispatch,
                    format!("manual dispatch: pull request trigger {trigger_id} not found"),
                );
                warn!(%trigger_id, "pull request trigger not found for manual dispatch");
                continue;
            };
            if let Some(reason) = self.pull_request_trigger_suppression(&workflow, &trigger) {
                let status = self.pull_request_trigger_status(&trigger);
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "manual dispatch skipped: {} is {status} ({reason:?})",
                        trigger.display_identifier()
                    ),
                );
                continue;
            }
            if let Err(error) = self
                .dispatch_pull_request_trigger(workflow.clone(), trigger.clone(), None)
                .await
            {
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "manual dispatch failed: {} ({error})",
                        trigger.display_identifier()
                    ),
                );
                error!(
                    %error,
                    trigger_id = %trigger.dedupe_key(),
                    "manual pull request trigger dispatch failed"
                );
            }
        }
    }

    pub(crate) async fn dispatch_requested_issue(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        agent_name: Option<&str>,
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

        self.process_pending_issue_approvals().await;
        self.process_pending_deliverable_resolutions().await;
        // Process merge requests
        let merge_ids = std::mem::take(&mut self.pending_merge_deliverables);
        for movement_id in merge_ids {
            self.merge_deliverable(&movement_id).await;
        }

        self.process_pending_task_resolutions().await;
        self.process_pending_task_retries().await;
        self.process_manual_dispatches().await;
        self.process_manual_pull_request_trigger_dispatches().await;
        // Emit snapshot immediately after dispatches so movements appear in the TUI
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

        let previous_issue_rows = self.state.visible_issues.clone();
        let mut issues = issues;
        issues.sort_by(dispatch_order);
        self.state.issue_snapshot_loaded = true;
        let tracker_kind = workflow.config.tracker.kind;
        self.state.visible_issues = issues
            .iter()
            .map(summarize_issue)
            .map(|row| self.resolved_issue_row(tracker_kind, &row))
            .collect();
        let current_issue_ids = self
            .state
            .visible_issues
            .iter()
            .map(|issue| issue.issue_id.clone())
            .collect::<HashSet<_>>();
        for issue_row in previous_issue_rows {
            if current_issue_ids.contains(&issue_row.issue_id) {
                self.state.discarded_triggers.remove(&issue_row.issue_id);
                continue;
            }
            if self.issue_is_actionable(&issue_row.issue_id) {
                continue;
            }
            self.record_discarded_trigger(self.issue_trigger_row(tracker_kind, &issue_row));
        }
        self.prune_discarded_triggers();
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

        if self.pull_request_trigger_source.is_some() {
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
            self.poll_pull_request_triggers(workflow.clone(), allow_pull_request_dispatch)
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
                .dispatch_issue(workflow.clone(), issue, None, false, None, false)
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
