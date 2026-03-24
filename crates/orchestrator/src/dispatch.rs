use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn reconcile_running(&mut self) {
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

        // Synthetic issue IDs (PR reviews, comments, conflicts) have no
        // tracker-side state — skip them so they are not treated as "missing".
        let running_ids = self
            .state
            .running
            .keys()
            .filter(|id| !is_synthetic_issue_id(id))
            .cloned()
            .collect::<Vec<_>>();
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

    pub(crate) async fn dispatch_issue(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        prefer_alternate_agent: bool,
        agent_override: Option<&str>,
        skip_workspace_sync: bool,
        directives: Option<&str>,
    ) -> Result<(), Error> {
        let manual_dispatch_directives = directives
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned);
        if workflow.config.pipeline_active() {
            info!(
                issue_identifier = %issue.identifier,
                attempt = attempt.unwrap_or(0),
                "dispatching issue via pipeline orchestration"
            );
            return self
                .dispatch_pipeline(
                    workflow,
                    issue,
                    attempt,
                    prefer_alternate_agent,
                    skip_workspace_sync,
                    manual_dispatch_directives.as_deref(),
                )
                .await;
        }
        let saved_context = self.state.saved_contexts.get(&issue.id).cloned();
        let selected_agent = if let Some(name) = agent_override {
            let candidates = workflow.config.expand_agent_candidates(name)?;
            self.select_dispatch_agent(&issue, &candidates, saved_context.as_ref(), false)?
        } else {
            let candidate_agents = workflow.config.candidate_agents_for_issue(&issue)?;
            self.select_dispatch_agent(
                &issue,
                &candidate_agents,
                saved_context.as_ref(),
                prefer_alternate_agent,
            )?
        };
        let workspace_manager = if skip_workspace_sync {
            info!(
                issue_identifier = %issue.identifier,
                "resuming orphaned workspace without sync_on_reuse"
            );
            WorkspaceManager::new(
                workflow.config.workspace.root.clone(),
                self.provisioner.clone(),
                workflow.config.workspace.checkout_kind,
                false,
                workflow.config.workspace.transient_paths.clone(),
                workflow.config.workspace.source_repo_path.clone(),
                workflow.config.workspace.clone_url.clone(),
                workflow.config.workspace.default_branch.clone(),
            )
        } else {
            self.build_workspace_manager(&workflow)
        };
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
        self.state
            .worktree_keys
            .insert(workspace.workspace_key.clone());

        // Reuse an existing active movement for this issue if one exists,
        // otherwise create a new one.  This prevents duplicate movements when
        // the same issue is re-dispatched via retry or continuation.
        if let Some(existing_id) = self.find_existing_movement_for_issue(&issue.id) {
            if let Some(movement) = self.state.movements.get_mut(&existing_id) {
                movement.status = MovementStatus::InProgress;
                movement.manual_dispatch_directives = manual_dispatch_directives.clone();
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_movement(movement).await?;
                }
            }
        } else {
            let movement_id = new_movement_id();
            let now = Utc::now();
            let movement = Movement {
                id: movement_id.clone(),
                kind: MovementKind::IssueDelivery,
                issue_id: Some(issue.id.clone()),
                issue_identifier: Some(issue.identifier.clone()),
                title: issue.title.clone(),
                status: MovementStatus::InProgress,
                pipeline_stage: None,
                manual_dispatch_directives: manual_dispatch_directives.clone(),
                workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
                workspace_path: Some(workspace.path.clone()),
                review_target: None,
                deliverable: None,
                created_at: now,
                updated_at: now,
            };
            if let Some(store) = &self.store {
                store.save_movement(&movement).await?;
            }
            self.state.movements.insert(movement_id.clone(), movement);
        }

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
            prepend_manual_dispatch_directives(
                apply_agent_prompt_template(
                    &workflow,
                    &selected_agent.name,
                    render_turn_prompt(&workflow.definition, &issue, attempt, 1, max_turns)?,
                    &issue,
                    attempt,
                    1,
                    max_turns,
                )?,
                manual_dispatch_directives.as_deref(),
            ),
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
            active_task_id: None,
            movement_id: None,
            review_target: None,
            review_comment_marker: None,
        });
        self.push_event(
            EventScope::Dispatch,
            format!("dispatched {issue_identifier}"),
        );
        Ok(())
    }
}
