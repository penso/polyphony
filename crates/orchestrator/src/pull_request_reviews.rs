use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn dispatch_pull_request_review(
        &mut self,
        workflow: LoadedWorkflow,
        trigger: PullRequestReviewTrigger,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        let issue = synthetic_issue_for_pull_request_review(&trigger);
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let review_target = trigger.review_target();
        let review_agent = workflow
            .config
            .pr_review_agent()?
            .ok_or_else(|| CoreError::Adapter("PR review agent is not available".into()))?;
        let review_agent_for_task = review_agent.clone();
        if self.is_throttled(&format!("agent:{}", review_agent.name)) {
            return Err(Error::Core(CoreError::Adapter(format!(
                "PR review agent `{}` is throttled",
                review_agent.name
            ))));
        }
        let workspace_manager = self.build_workspace_manager(&workflow);
        let workspace = workspace_manager
            .ensure_workspace_with_ref(
                &issue.identifier,
                issue.branch_name.clone(),
                review_target.checkout_ref.clone(),
                &workflow.config.hooks,
            )
            .await?;
        self.state
            .worktree_keys
            .insert(workspace.workspace_key.clone());
        let movement_id = new_movement_id();
        let now = Utc::now();
        let movement = Movement {
            id: movement_id.clone(),
            kind: MovementKind::PullRequestReview,
            issue_id: Some(issue_id.clone()),
            issue_identifier: Some(issue_identifier.clone()),
            title: trigger.title.clone(),
            status: MovementStatus::InProgress,
            pipeline_stage: None,
            workspace_key: Some(workspace.workspace_key.clone()),
            workspace_path: Some(workspace.path.clone()),
            review_target: Some(review_target.clone()),
            deliverable: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(store) = &self.store {
            store.save_movement(&movement).await?;
        }
        self.state.movements.insert(movement_id.clone(), movement);

        let prompt = render_issue_template_with_strings(
            workflow
                .config
                .review_triggers
                .pr_reviews
                .prompt
                .as_deref()
                .unwrap_or(DEFAULT_PULL_REQUEST_REVIEW_PROMPT),
            &issue,
            attempt,
            &[
                ("repository", review_target.repository.clone()),
                ("base_branch", review_target.base_branch.clone()),
                ("head_branch", review_target.head_branch.clone()),
                ("head_sha", review_target.head_sha.clone()),
                (
                    "pull_request_url",
                    review_target.url.clone().unwrap_or_default(),
                ),
                ("pull_request_number", review_target.number.to_string()),
                (
                    "pull_request_author",
                    trigger.author_login.clone().unwrap_or_default(),
                ),
                ("pull_request_labels", trigger.labels.join(", ")),
            ],
        )?;
        let prompt = apply_agent_prompt_template(
            &workflow,
            &review_agent.name,
            prompt,
            &issue,
            attempt,
            1,
            workflow.config.agent.max_turns,
        )?;
        let command_tx = self.command_tx.clone();
        let agent = self.agent.clone();
        let workspace_path = workspace.path.clone();
        let hooks = workflow.config.hooks.clone();
        let active_states = workflow.config.tracker.active_states.clone();
        let max_turns = workflow.config.agent.max_turns;
        let provisioner = self.provisioner.clone();
        let tracker = self.tracker.clone();
        let selected_agent_name = review_agent.name.clone();
        let started_at = Utc::now();
        let trigger_for_retry = trigger.clone();
        let issue_for_task = issue.clone();
        let issue_identifier_for_task = issue_identifier.clone();
        let issue_id_for_task = issue_id.clone();
        let worker_span = info_span!(
            "pull_request_review_worker",
            issue_identifier = %issue_identifier_for_task,
            agent = %selected_agent_name,
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
                    workspace_path,
                    prompt,
                    active_states,
                    max_turns,
                    workflow.config.agent.continuation_prompt.clone(),
                    review_agent_for_task,
                    None,
                    command_tx.clone(),
                )
                .await;
                let outcome = match outcome {
                    Ok(result) => result,
                    Err(error) => agent_run_result_from_error(&error),
                };
                let _ = command_tx.send(OrchestratorMessage::WorkerFinished {
                    issue_id: issue_id_for_task,
                    issue_identifier: issue_identifier_for_task,
                    attempt,
                    started_at,
                    outcome,
                });
            }
            .instrument(worker_span),
        );

        self.claim_issue(issue_id.clone(), IssueClaimState::Running);
        self.state.retrying.remove(&issue_id);
        self.state.pull_request_retry_triggers.insert(
            issue_id.clone(),
            PullRequestTrigger::Review(trigger_for_retry),
        );
        self.state.running.insert(issue_id.clone(), RunningTask {
            issue,
            agent_name: selected_agent_name,
            model: review_agent
                .model
                .clone()
                .or_else(|| review_agent.models.first().cloned()),
            attempt,
            workspace_path: workspace.path,
            stall_timeout_ms: review_agent.stall_timeout_ms,
            max_turns,
            started_at,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("dispatch_started".into()),
            last_message: Some("PR review worker launched".into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle,
            active_task_id: None,
            movement_id: Some(movement_id),
            review_target: Some(review_target),
            review_comment_marker: Some(pull_request_review_comment_marker(
                &trigger.review_target(),
            )),
        });
        self.push_event(
            EventScope::Dispatch,
            format!("dispatched PR review {issue_identifier}"),
        );
        Ok(())
    }

    pub(crate) async fn dispatch_pull_request_comment_review(
        &mut self,
        workflow: LoadedWorkflow,
        trigger: PullRequestCommentTrigger,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        let issue = synthetic_issue_for_pull_request_comment(&trigger);
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let review_target = trigger.review_target();
        let review_agent = workflow
            .config
            .pr_review_agent()?
            .ok_or_else(|| CoreError::Adapter("PR review agent is not available".into()))?;
        let review_agent_for_task = review_agent.clone();
        if self.is_throttled(&format!("agent:{}", review_agent.name)) {
            return Err(Error::Core(CoreError::Adapter(format!(
                "PR review agent `{}` is throttled",
                review_agent.name
            ))));
        }
        let workspace_manager = self.build_workspace_manager(&workflow);
        let workspace = workspace_manager
            .ensure_workspace_with_ref(
                &issue.identifier,
                issue.branch_name.clone(),
                review_target.checkout_ref.clone(),
                &workflow.config.hooks,
            )
            .await?;
        self.state
            .worktree_keys
            .insert(workspace.workspace_key.clone());
        let movement_id = new_movement_id();
        let now = Utc::now();
        let movement = Movement {
            id: movement_id.clone(),
            kind: MovementKind::PullRequestCommentReview,
            issue_id: Some(issue_id.clone()),
            issue_identifier: Some(issue_identifier.clone()),
            title: format!(
                "Review PR comment on {}: {}",
                trigger.path, trigger.pull_request_title
            ),
            status: MovementStatus::InProgress,
            pipeline_stage: None,
            workspace_key: Some(workspace.workspace_key.clone()),
            workspace_path: Some(workspace.path.clone()),
            review_target: Some(review_target.clone()),
            deliverable: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(store) = &self.store {
            store.save_movement(&movement).await?;
        }
        self.state.movements.insert(movement_id.clone(), movement);

        let prompt = render_issue_template_with_strings(
            workflow
                .config
                .review_triggers
                .pr_reviews
                .prompt
                .as_deref()
                .unwrap_or(DEFAULT_PULL_REQUEST_COMMENT_REVIEW_PROMPT),
            &issue,
            attempt,
            &[
                ("repository", review_target.repository.clone()),
                ("base_branch", review_target.base_branch.clone()),
                ("head_branch", review_target.head_branch.clone()),
                ("head_sha", review_target.head_sha.clone()),
                (
                    "pull_request_url",
                    review_target.url.clone().unwrap_or_default(),
                ),
                ("pull_request_number", review_target.number.to_string()),
                (
                    "pull_request_comment_author",
                    trigger.author_login.clone().unwrap_or_default(),
                ),
                ("pull_request_comment_path", trigger.path.clone()),
                (
                    "pull_request_comment_line",
                    trigger
                        .line
                        .map(|line| line.to_string())
                        .unwrap_or_default(),
                ),
                ("pull_request_comment_body", trigger.body.clone()),
                ("pull_request_labels", trigger.labels.join(", ")),
            ],
        )?;
        let prompt = apply_agent_prompt_template(
            &workflow,
            &review_agent.name,
            prompt,
            &issue,
            attempt,
            1,
            workflow.config.agent.max_turns,
        )?;
        let command_tx = self.command_tx.clone();
        let agent = self.agent.clone();
        let workspace_path = workspace.path.clone();
        let hooks = workflow.config.hooks.clone();
        let active_states = workflow.config.tracker.active_states.clone();
        let max_turns = workflow.config.agent.max_turns;
        let provisioner = self.provisioner.clone();
        let tracker = self.tracker.clone();
        let selected_agent_name = review_agent.name.clone();
        let started_at = Utc::now();
        let trigger_for_retry = trigger.clone();
        let issue_for_task = issue.clone();
        let issue_identifier_for_task = issue_identifier.clone();
        let issue_id_for_task = issue_id.clone();
        let worker_span = info_span!(
            "pull_request_comment_review_worker",
            issue_identifier = %issue_identifier_for_task,
            agent = %selected_agent_name,
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
                    workspace_path,
                    prompt,
                    active_states,
                    max_turns,
                    workflow.config.agent.continuation_prompt.clone(),
                    review_agent_for_task,
                    None,
                    command_tx.clone(),
                )
                .await;
                let outcome = match outcome {
                    Ok(result) => result,
                    Err(error) => agent_run_result_from_error(&error),
                };
                let _ = command_tx.send(OrchestratorMessage::WorkerFinished {
                    issue_id: issue_id_for_task,
                    issue_identifier: issue_identifier_for_task,
                    attempt,
                    started_at,
                    outcome,
                });
            }
            .instrument(worker_span),
        );

        self.claim_issue(issue_id.clone(), IssueClaimState::Running);
        self.state.retrying.remove(&issue_id);
        self.state.pull_request_retry_triggers.insert(
            issue_id.clone(),
            PullRequestTrigger::Comment(trigger_for_retry.clone()),
        );
        self.state.running.insert(issue_id.clone(), RunningTask {
            issue,
            agent_name: selected_agent_name,
            model: review_agent
                .model
                .clone()
                .or_else(|| review_agent.models.first().cloned()),
            attempt,
            workspace_path: workspace.path,
            stall_timeout_ms: review_agent.stall_timeout_ms,
            max_turns,
            started_at,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("dispatch_started".into()),
            last_message: Some("PR comment review worker launched".into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle,
            active_task_id: None,
            movement_id: Some(movement_id),
            review_target: Some(review_target),
            review_comment_marker: Some(pull_request_comment_review_comment_marker(
                &trigger.review_target(),
                &trigger.thread_id,
            )),
        });
        self.push_event(
            EventScope::Dispatch,
            format!("dispatched PR comment review {issue_identifier}"),
        );
        Ok(())
    }

    pub(crate) async fn finish_running(
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
        let finished_at = Utc::now();
        self.finalize_saved_context(&issue_id, &issue_identifier, &running, &outcome);
        if let Some(context) = self.state.saved_contexts.get(&issue_id)
            && let Err(error) =
                persist_workspace_saved_context_artifact(&running.workspace_path, context).await
        {
            warn!(
                %error,
                workspace_path = %running.workspace_path.display(),
                issue_identifier = %issue_identifier,
                "persisting workspace saved context failed"
            );
        }
        let persisted_run = build_persisted_run_record(
            &running,
            outcome.status,
            finished_at,
            outcome.error.clone(),
            self.state.saved_contexts.get(&issue_id).cloned(),
        );
        self.record_run_history(persisted_run).await?;

        if running.review_target.is_some() {
            let result = self
                .finish_pull_request_review(issue_id, issue_identifier, attempt, running, outcome)
                .await;
            self.emit_snapshot().await?;
            return result;
        }

        // Pipeline worker handling
        if let Some(movement_id) = running.movement_id.clone() {
            let stopped = self.state.dispatch_mode == polyphony_core::DispatchMode::Stop;
            let workflow = self.workflow();
            let issue = running.issue.clone();
            let workspace_path = running.workspace_path.clone();
            let active_task_id = running.active_task_id.clone();
            self.push_event(
                EventScope::Worker,
                format!("{} pipeline worker {:?}", issue_identifier, outcome.status),
            );

            if stopped {
                // In stop mode, do not dispatch follow-up pipeline work or retries.
                self.release_issue(&issue_id);
                self.emit_snapshot().await?;
                return Ok(());
            }

            // Determine if this was a planner or a task worker
            if active_task_id.is_none() {
                // This was the planner
                let result = self
                    .handle_planner_finished(
                        &workflow,
                        &issue,
                        &movement_id,
                        &workspace_path,
                        &outcome,
                        attempt,
                    )
                    .await;
                if let Err(error) = &result {
                    warn!(%error, issue_identifier = %issue_identifier, "pipeline planner handling failed");
                    self.release_issue(&issue_id);
                }
                self.emit_snapshot().await?;
                return result;
            }

            // This was a task worker
            let task_id = active_task_id.unwrap();
            let result = self
                .handle_task_finished(
                    &workflow,
                    &issue,
                    &movement_id,
                    &task_id,
                    &workspace_path,
                    &outcome,
                    attempt,
                )
                .await;
            if let Err(error) = &result {
                warn!(%error, issue_identifier = %issue_identifier, "pipeline task handling failed");
                self.release_issue(&issue_id);
            }

            // After all tasks complete, run success handoff
            let pipeline_done = self.state.movements.get(&movement_id).is_some_and(|m| {
                matches!(m.status, MovementStatus::Review | MovementStatus::Delivered)
            });
            if pipeline_done {
                let workflow_status = outcome
                    .final_issue_state
                    .clone()
                    .unwrap_or_else(|| "Human Review".into());
                if !workflow.config.is_active_state(&workflow_status) {
                    if let Err(error) = self
                        .tracker
                        .update_issue_workflow_status(&issue, &workflow_status)
                        .await
                    {
                        warn!(%error, issue_identifier = %issue.identifier, "issue workflow status sync failed");
                    }
                    if let Err(error) = self.run_success_handoff(&workflow, &running).await {
                        warn!(%error, issue_identifier = %issue.identifier, "pipeline handoff failed");
                        self.push_event(
                            EventScope::Handoff,
                            format!("{} pipeline handoff failed: {}", issue.identifier, error),
                        );
                    }
                }
                // For non-automated pipelines, verify the agent produced actual changes.
                // Automated pipelines go to Review for human inspection even without changes.
                let movement_kind = self.state.movements.get(&movement_id).map(|m| m.kind);
                let deliverable = self
                    .state
                    .movements
                    .get(&movement_id)
                    .and_then(|m| m.deliverable.as_ref());
                let confirmed_no_changes = deliverable.is_some_and(|d| {
                    d.metadata
                        .get("lines_added")
                        .and_then(|v| v.as_u64())
                        .is_some_and(|added| added == 0)
                });
                let no_output = (confirmed_no_changes || deliverable.is_none())
                    && matches!(
                        movement_kind,
                        Some(polyphony_core::MovementKind::IssueDelivery)
                    )
                    && !workflow.config.automation.enabled;
                if no_output {
                    warn!(
                        issue_identifier = %issue.identifier,
                        movement_id,
                        "pipeline completed with no code changes — marking as failed"
                    );
                    if let Some(movement) = self.state.movements.get_mut(&movement_id) {
                        movement.status = MovementStatus::Failed;
                        movement.updated_at = Utc::now();
                        if let Some(store) = &self.store {
                            let _ = store.save_movement(movement).await;
                        }
                    }
                    self.push_event(
                        EventScope::Dispatch,
                        format!(
                            "{} pipeline failed: completed without producing any code changes",
                            issue.identifier
                        ),
                    );
                    self.schedule_retry(
                        issue_id.clone(),
                        issue_identifier.clone(),
                        attempt.unwrap_or(0) + 1,
                        Some("pipeline completed without code changes".into()),
                        false,
                        workflow.config.agent.max_retry_backoff_ms,
                    );
                } else {
                    self.state.completed.insert(issue_id.clone());
                    self.schedule_retry(
                        issue_id.clone(),
                        issue_identifier.clone(),
                        1,
                        None,
                        true,
                        workflow.config.agent.max_retry_backoff_ms,
                    );
                }
            } else if self
                .state
                .movements
                .get(&movement_id)
                .is_some_and(|m| matches!(m.status, MovementStatus::Failed))
            {
                self.schedule_retry(
                    issue_id.clone(),
                    issue_identifier.clone(),
                    attempt.unwrap_or(0) + 1,
                    outcome.error.clone(),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            }

            self.emit_snapshot().await?;
            return result;
        }

        // Non-pipeline (existing behavior)
        // Update the movement status to reflect the outcome.
        let movement_status = match outcome.status {
            AttemptStatus::Succeeded => MovementStatus::Delivered,
            AttemptStatus::Failed | AttemptStatus::TimedOut | AttemptStatus::Stalled => {
                MovementStatus::Failed
            },
            AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser => {
                MovementStatus::Cancelled
            },
        };
        if let Some(movement) = self
            .state
            .movements
            .values_mut()
            .find(|m| m.issue_id.as_deref() == Some(&issue_id))
        {
            movement.status = movement_status;
            movement.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_movement(movement).await;
            }
        }

        let stopped = self.state.dispatch_mode == polyphony_core::DispatchMode::Stop;
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
                if stopped {
                    self.release_issue(&issue_id);
                } else {
                    self.schedule_retry(
                        issue_id.clone(),
                        issue_identifier.clone(),
                        1,
                        None,
                        true,
                        workflow.config.agent.max_retry_backoff_ms,
                    );
                }
            },
            AttemptStatus::CancelledByReconciliation => {
                self.release_issue(&issue_id);
                self.state.pull_request_retry_triggers.remove(&issue_id);
            },
            _ => {
                if stopped {
                    self.release_issue(&issue_id);
                } else {
                    self.schedule_retry(
                        issue_id.clone(),
                        issue_identifier.clone(),
                        attempt.unwrap_or(0) + 1,
                        outcome.error.clone(),
                        false,
                        workflow.config.agent.max_retry_backoff_ms,
                    );
                }
            },
        }
        self.push_event(
            EventScope::Worker,
            format!("{} {:?}", issue_identifier, outcome.status),
        );
        self.emit_snapshot().await?;
        Ok(())
    }

    pub(crate) async fn finish_pull_request_review(
        &mut self,
        issue_id: String,
        issue_identifier: String,
        attempt: Option<u32>,
        running: RunningTask,
        outcome: AgentRunResult,
    ) -> Result<(), Error> {
        let Some(review_target) = running.review_target.clone() else {
            return Ok(());
        };
        let movement_id = running.movement_id.clone();
        let movement_status = match outcome.status {
            AttemptStatus::Succeeded => MovementStatus::Delivered,
            AttemptStatus::Failed | AttemptStatus::TimedOut | AttemptStatus::Stalled => {
                MovementStatus::Failed
            },
            AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser => {
                MovementStatus::Cancelled
            },
        };
        if let Some(movement_id) = movement_id.as_ref()
            && let Some(movement) = self.state.movements.get_mut(movement_id)
        {
            movement.status = movement_status;
            movement.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_movement(movement).await;
            }
        }

        let stopped = self.state.dispatch_mode == polyphony_core::DispatchMode::Stop;
        match outcome.status {
            AttemptStatus::Succeeded => {
                if let Err(error) = self
                    .post_pull_request_review_comment(&running, &review_target)
                    .await
                {
                    if let Some(movement_id) = movement_id.as_ref()
                        && let Some(movement) = self.state.movements.get_mut(movement_id)
                    {
                        movement.status = MovementStatus::Failed;
                        movement.updated_at = Utc::now();
                        if let Some(store) = &self.store {
                            let _ = store.save_movement(movement).await;
                        }
                    }
                    self.push_event(
                        EventScope::Handoff,
                        format!("{} review comment failed: {}", issue_identifier, error),
                    );
                    if stopped {
                        self.release_issue(&issue_id);
                    } else {
                        self.schedule_retry(
                            issue_id.clone(),
                            issue_identifier.clone(),
                            attempt.unwrap_or(0) + 1,
                            Some(error.to_string()),
                            false,
                            self.workflow().config.agent.max_retry_backoff_ms,
                        );
                    }
                } else {
                    let reviewed = ReviewedPullRequestHead {
                        key: review_target_key(&review_target),
                        target: review_target.clone(),
                        reviewed_at: Utc::now(),
                        movement_id: movement_id.clone(),
                    };
                    if let Some(store) = &self.store {
                        store.save_reviewed_pull_request_head(&reviewed).await?;
                    }
                    self.state
                        .reviewed_pull_request_heads
                        .insert(reviewed.key.clone(), reviewed);
                    self.state.completed.insert(issue_id.clone());
                    self.release_issue(&issue_id);
                    self.state.pull_request_retry_triggers.remove(&issue_id);
                }
            },
            AttemptStatus::CancelledByReconciliation => {
                self.release_issue(&issue_id);
            },
            _ => {
                if stopped {
                    self.release_issue(&issue_id);
                } else {
                    self.schedule_retry(
                        issue_id.clone(),
                        issue_identifier.clone(),
                        attempt.unwrap_or(0) + 1,
                        outcome.error.clone(),
                        false,
                        self.workflow().config.agent.max_retry_backoff_ms,
                    );
                }
            },
        }
        self.push_event(
            EventScope::Worker,
            format!("{} PR review {:?}", issue_identifier, outcome.status),
        );
        Ok(())
    }

    pub(crate) async fn post_pull_request_review_comment(
        &mut self,
        running: &RunningTask,
        review_target: &ReviewTarget,
    ) -> Result<(), Error> {
        let review_path = running.workspace_path.join(".polyphony").join("review.md");
        let review_body = tokio::fs::read_to_string(&review_path).await?;
        let trimmed = review_body.trim();
        let _ = tokio::fs::remove_file(&review_path).await;
        if trimmed.is_empty() {
            return Err(Error::Core(CoreError::Adapter(
                "PR review agent produced an empty `.polyphony/review.md`".into(),
            )));
        }
        let comment_mode = self
            .workflow()
            .config
            .review_triggers
            .pr_reviews
            .comment_mode
            .clone();
        let review_comments_path = running
            .workspace_path
            .join(".polyphony")
            .join("review-comments.json");
        let review_comments = load_pull_request_review_comments(&review_comments_path).await?;
        let commenter = self.pull_request_commenter.clone().ok_or_else(|| {
            Error::Core(CoreError::Adapter(
                "pull request commenter is not configured".into(),
            ))
        })?;
        let marker = running
            .review_comment_marker
            .clone()
            .unwrap_or_else(|| pull_request_review_comment_marker(review_target));
        let body = format!("{trimmed}\n\n{marker}");
        let pull_request = PullRequestRef {
            repository: review_target.repository.clone(),
            number: review_target.number,
            url: review_target.url.clone(),
        };
        if comment_mode == "inline" && !review_comments.is_empty() {
            commenter
                .sync_pull_request_review(
                    &pull_request,
                    &marker,
                    &body,
                    &review_comments,
                    &review_target.head_sha,
                )
                .await?;
        } else {
            commenter
                .sync_pull_request_comment(&pull_request, &marker, &body)
                .await?;
        }
        self.push_event(
            EventScope::Handoff,
            if comment_mode == "inline" && !review_comments.is_empty() {
                format!(
                    "{} reviewed PR #{} at {} with {} inline comments",
                    running.issue.identifier,
                    review_target.number,
                    review_target.head_sha,
                    review_comments.len()
                )
            } else {
                format!(
                    "{} reviewed PR #{} at {}",
                    running.issue.identifier, review_target.number, review_target.head_sha
                )
            },
        );
        let workflow = self.workflow();
        let manager = self.build_workspace_manager(&workflow);
        manager
            .run_after_outcome_best_effort(&workflow.config.hooks, &running.workspace_path)
            .await;
        Ok(())
    }
}
