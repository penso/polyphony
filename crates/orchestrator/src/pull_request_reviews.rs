use crate::{prelude::*, *};

impl RuntimeService {
    fn pull_request_retry_trigger(
        &self,
        issue_id: &str,
        review_target: Option<&ReviewTarget>,
    ) -> Option<PullRequestTrigger> {
        self.state
            .pull_request_retry_triggers
            .get(issue_id)
            .cloned()
            .or_else(|| {
                self.state
                    .visible_review_triggers
                    .values()
                    .find(|trigger| {
                        trigger.synthetic_issue_id() == issue_id
                            || review_target.is_some_and(|target| {
                                review_target_key(&trigger.review_target())
                                    == review_target_key(target)
                            })
                    })
                    .cloned()
                    .map(PullRequestTrigger::Review)
            })
            .or_else(|| {
                self.state
                    .visible_comment_triggers
                    .values()
                    .find(|trigger| {
                        trigger.synthetic_issue_id() == issue_id
                            || review_target.is_some_and(|target| {
                                review_target_key(&trigger.review_target())
                                    == review_target_key(target)
                            })
                    })
                    .cloned()
                    .map(PullRequestTrigger::Comment)
            })
            .or_else(|| {
                self.state
                    .visible_conflict_triggers
                    .values()
                    .find(|trigger| {
                        trigger.synthetic_issue_id() == issue_id
                            || review_target.is_some_and(|target| {
                                review_target_key(&trigger.review_target())
                                    == review_target_key(target)
                            })
                    })
                    .cloned()
                    .map(PullRequestTrigger::Conflict)
            })
    }

    pub(crate) async fn dispatch_pull_request_review(
        &mut self,
        workflow: LoadedWorkflow,
        trigger: PullRequestReviewTrigger,
        attempt: Option<u32>,
        directives: Option<&str>,
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
        let movement_title = trigger.title.clone();
        let (movement_id, workspace_setup_task_id, review_task_id) = self
            .prepare_pull_request_dispatch(
                &workflow,
                &issue,
                MovementKind::PullRequestReview,
                movement_title,
                review_target.clone(),
                &review_agent.name,
                directives,
            )
            .await?;
        self.emit_snapshot().await?;
        let workspace_manager = self.build_workspace_manager(&workflow);
        let workspace = match workspace_manager
            .ensure_workspace_with_ref(
                &issue.identifier,
                issue.branch_name.clone(),
                review_target.checkout_ref.clone(),
                &workflow.config.hooks,
            )
            .await
        {
            Ok(workspace) => workspace,
            Err(error) => {
                self.fail_pull_request_workspace_setup(
                    &movement_id,
                    &workspace_setup_task_id,
                    &review_task_id,
                    &error.to_string(),
                )
                .await?;
                return Err(error.into());
            },
        };
        self.state
            .worktree_keys
            .insert(workspace.workspace_key.clone());
        self.finish_pull_request_workspace_setup(
            &movement_id,
            &workspace_setup_task_id,
            &review_task_id,
            &workspace.workspace_key,
            &workspace.path,
            &review_agent.name,
        )
        .await?;
        self.state
            .pull_request_retry_triggers
            .insert(issue_id, PullRequestTrigger::Review(trigger.clone()));
        let prompt = self.build_pull_request_review_prompt(
            &workflow,
            &trigger,
            &issue,
            &review_agent.name,
            &movement_id,
            attempt,
        )?;
        self.start_pull_request_worker(
            workflow,
            issue,
            attempt,
            workspace.path,
            prompt,
            review_agent_for_task,
            &movement_id,
            &review_task_id,
            review_target,
            pull_request_review_comment_marker(&trigger.review_target()),
            format!("dispatched PR review {issue_identifier}"),
            "PR review worker launched",
            "pull_request_review_worker",
        )
        .await
    }

    pub(crate) async fn dispatch_pull_request_comment_review(
        &mut self,
        workflow: LoadedWorkflow,
        trigger: PullRequestCommentTrigger,
        attempt: Option<u32>,
        directives: Option<&str>,
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
        let movement_title = format!(
            "Review PR comment on {}: {}",
            trigger.path, trigger.pull_request_title
        );
        let (movement_id, workspace_setup_task_id, review_task_id) = self
            .prepare_pull_request_dispatch(
                &workflow,
                &issue,
                MovementKind::PullRequestCommentReview,
                movement_title,
                review_target.clone(),
                &review_agent.name,
                directives,
            )
            .await?;
        self.emit_snapshot().await?;
        let workspace_manager = self.build_workspace_manager(&workflow);
        let workspace = match workspace_manager
            .ensure_workspace_with_ref(
                &issue.identifier,
                issue.branch_name.clone(),
                review_target.checkout_ref.clone(),
                &workflow.config.hooks,
            )
            .await
        {
            Ok(workspace) => workspace,
            Err(error) => {
                self.fail_pull_request_workspace_setup(
                    &movement_id,
                    &workspace_setup_task_id,
                    &review_task_id,
                    &error.to_string(),
                )
                .await?;
                return Err(error.into());
            },
        };
        self.state
            .worktree_keys
            .insert(workspace.workspace_key.clone());
        self.finish_pull_request_workspace_setup(
            &movement_id,
            &workspace_setup_task_id,
            &review_task_id,
            &workspace.workspace_key,
            &workspace.path,
            &review_agent.name,
        )
        .await?;
        self.state
            .pull_request_retry_triggers
            .insert(issue_id, PullRequestTrigger::Comment(trigger.clone()));
        let prompt = self.build_pull_request_comment_review_prompt(
            &workflow,
            &trigger,
            &issue,
            &review_agent.name,
            &movement_id,
            attempt,
        )?;
        self.start_pull_request_worker(
            workflow,
            issue,
            attempt,
            workspace.path,
            prompt,
            review_agent_for_task,
            &movement_id,
            &review_task_id,
            review_target,
            pull_request_comment_review_comment_marker(
                &trigger.review_target(),
                &trigger.thread_id,
            ),
            format!("dispatched PR comment review {issue_identifier}"),
            "PR comment review worker launched",
            "pull_request_comment_review_worker",
        )
        .await
    }

    fn build_pull_request_review_prompt(
        &self,
        workflow: &LoadedWorkflow,
        trigger: &PullRequestReviewTrigger,
        issue: &Issue,
        review_agent_name: &str,
        movement_id: &str,
        attempt: Option<u32>,
    ) -> Result<String, Error> {
        let review_target = trigger.review_target();
        let prompt = render_issue_template_with_strings(
            workflow
                .config
                .review_triggers
                .pr_reviews
                .prompt
                .as_deref()
                .unwrap_or(DEFAULT_PULL_REQUEST_REVIEW_PROMPT),
            issue,
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
            workflow,
            review_agent_name,
            prompt,
            issue,
            attempt,
            1,
            workflow.config.agent.max_turns,
        )?;
        Ok(prepend_manual_dispatch_directives(
            prompt,
            self.manual_dispatch_directives_for_movement(movement_id),
        ))
    }

    fn build_pull_request_comment_review_prompt(
        &self,
        workflow: &LoadedWorkflow,
        trigger: &PullRequestCommentTrigger,
        issue: &Issue,
        review_agent_name: &str,
        movement_id: &str,
        attempt: Option<u32>,
    ) -> Result<String, Error> {
        let review_target = trigger.review_target();
        let prompt = render_issue_template_with_strings(
            workflow
                .config
                .review_triggers
                .pr_reviews
                .prompt
                .as_deref()
                .unwrap_or(DEFAULT_PULL_REQUEST_COMMENT_REVIEW_PROMPT),
            issue,
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
            workflow,
            review_agent_name,
            prompt,
            issue,
            attempt,
            1,
            workflow.config.agent.max_turns,
        )?;
        Ok(prepend_manual_dispatch_directives(
            prompt,
            self.manual_dispatch_directives_for_movement(movement_id),
        ))
    }

    async fn start_pull_request_worker(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        workspace_path: PathBuf,
        prompt: String,
        review_agent: polyphony_core::AgentDefinition,
        movement_id: &str,
        review_task_id: &str,
        review_target: ReviewTarget,
        review_comment_marker: String,
        dispatch_event: String,
        running_message: &'static str,
        worker_span_name: &'static str,
    ) -> Result<(), Error> {
        let command_tx = self.command_tx.clone();
        let agent = self.agent.clone();
        let hooks = workflow.config.hooks.clone();
        let active_states = workflow.config.tracker.active_states.clone();
        let max_turns = workflow.config.agent.max_turns;
        let provisioner = self.provisioner.clone();
        let tracker = self.tracker.clone();
        let selected_agent_name = review_agent.name.clone();
        let started_at = Utc::now();
        let issue_for_task = issue.clone();
        let issue_identifier_for_task = issue.identifier.clone();
        let issue_id_for_task = issue.id.clone();
        let workspace_path_for_task = workspace_path.clone();
        let review_agent_for_task = review_agent.clone();
        let worker_span = info_span!(
            "pull_request_review_worker",
            kind = worker_span_name,
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
                    workspace_path_for_task,
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

        self.claim_issue(issue.id.clone(), IssueClaimState::Running);
        self.state.retrying.remove(&issue.id);
        self.state.running.insert(issue.id.clone(), RunningTask {
            issue,
            agent_name: selected_agent_name,
            model: review_agent
                .model
                .clone()
                .or_else(|| review_agent.models.first().cloned()),
            attempt,
            workspace_path,
            stall_timeout_ms: review_agent.stall_timeout_ms,
            max_turns,
            started_at,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("dispatch_started".into()),
            last_message: Some(running_message.into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle,
            active_task_id: Some(review_task_id.to_string()),
            movement_id: Some(movement_id.to_string()),
            review_target: Some(review_target),
            review_comment_marker: Some(review_comment_marker),
        });
        self.push_event(EventScope::Dispatch, dispatch_event);
        Ok(())
    }

    fn manual_dispatch_directives_for_movement(&self, movement_id: &str) -> Option<&str> {
        self.state
            .movements
            .get(movement_id)
            .and_then(|movement| movement.manual_dispatch_directives.as_deref())
    }

    async fn prepare_pull_request_dispatch(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        movement_kind: MovementKind,
        movement_title: String,
        review_target: ReviewTarget,
        review_agent_name: &str,
        directives: Option<&str>,
    ) -> Result<(MovementId, TaskId, TaskId), Error> {
        let now = Utc::now();
        let workspace_key = sanitize_workspace_key(&issue.identifier);
        let workspace_path = workflow.config.workspace.root.join(&workspace_key);
        let requested_directives = directives
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned);
        let movement_id =
            if let Some(existing_id) = self.find_existing_movement_for_issue(&issue.id) {
                let directives = requested_directives.or_else(|| {
                    self.state
                        .movements
                        .get(&existing_id)
                        .and_then(|movement| movement.manual_dispatch_directives.clone())
                });
                if let Some(movement) = self.state.movements.get_mut(&existing_id) {
                    movement.kind = movement_kind;
                    movement.title = movement_title;
                    movement.status = MovementStatus::InProgress;
                    movement.pipeline_stage = None;
                    movement.manual_dispatch_directives = directives;
                    movement.workspace_key = Some(workspace_key.clone());
                    movement.workspace_path = Some(workspace_path.clone());
                    movement.review_target = Some(review_target.clone());
                    movement.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_movement(movement).await?;
                    }
                }
                existing_id
            } else {
                let movement_id = new_movement_id();
                let movement = Movement {
                    id: movement_id.clone(),
                    kind: movement_kind,
                    issue_id: Some(issue.id.clone()),
                    issue_identifier: Some(issue.identifier.clone()),
                    title: movement_title,
                    status: MovementStatus::InProgress,
                    pipeline_stage: None,
                    manual_dispatch_directives: requested_directives,
                    workspace_key: Some(workspace_key.clone()),
                    workspace_path: Some(workspace_path.clone()),
                    review_target: Some(review_target),
                    deliverable: None,
                    created_at: now,
                    updated_at: now,
                };
                if let Some(store) = &self.store {
                    store.save_movement(&movement).await?;
                }
                self.state.movements.insert(movement_id.clone(), movement);
                movement_id
            };

        let (workspace_setup_task_id, review_task_id) = self
            .reset_pull_request_tasks(&movement_id, review_agent_name)
            .await?;
        self.state.workspace_setup_tasks_by_issue_identifier.insert(
            issue.identifier.clone(),
            (movement_id.clone(), workspace_setup_task_id.clone()),
        );
        self.state.workspace_setup_tasks_by_key.insert(
            workspace_key,
            (movement_id.clone(), workspace_setup_task_id.clone()),
        );
        Ok((movement_id, workspace_setup_task_id, review_task_id))
    }

    async fn reset_pull_request_tasks(
        &mut self,
        movement_id: &str,
        review_agent_name: &str,
    ) -> Result<(TaskId, TaskId), Error> {
        let now = Utc::now();
        let tasks = self.state.tasks.entry(movement_id.to_string()).or_default();
        let workspace_setup_task_id = upsert_pull_request_task(
            tasks,
            movement_id,
            0,
            "Creating worktree",
            polyphony_core::TaskCategory::Research,
            Some("orchestrator".into()),
        );
        let review_task_id = upsert_pull_request_task(
            tasks,
            movement_id,
            1,
            "Run PR review",
            polyphony_core::TaskCategory::Review,
            Some(review_agent_name.to_string()),
        );
        for task in tasks.iter_mut() {
            match task.ordinal {
                0 => {
                    task.status = TaskStatus::InProgress;
                    task.agent_name = Some("orchestrator".into());
                    task.started_at = Some(now);
                    task.finished_at = None;
                    task.error = None;
                    task.activity_log.clear();
                    task.turns_completed = 0;
                    task.tokens = TokenUsage::default();
                    task.updated_at = now;
                },
                1 => {
                    task.status = TaskStatus::Pending;
                    task.agent_name = Some(review_agent_name.to_string());
                    task.started_at = None;
                    task.finished_at = None;
                    task.error = None;
                    task.activity_log.clear();
                    task.turns_completed = 0;
                    task.tokens = TokenUsage::default();
                    task.updated_at = now;
                },
                _ => {},
            }
        }
        if let Some(store) = &self.store {
            for task in tasks.iter() {
                if task.ordinal <= 1 {
                    store.save_task(task).await?;
                }
            }
        }
        Ok((workspace_setup_task_id, review_task_id))
    }

    async fn reset_pull_request_review_task_for_retry(
        &mut self,
        movement_id: &str,
        review_task_id: &str,
        review_agent_name: &str,
    ) -> Result<(), Error> {
        let now = Utc::now();
        if let Some(movement) = self.state.movements.get_mut(movement_id) {
            movement.status = MovementStatus::InProgress;
            movement.updated_at = now;
            if let Some(store) = &self.store {
                store.save_movement(movement).await?;
            }
        }
        if let Some(tasks) = self.state.tasks.get_mut(movement_id)
            && let Some(task) = tasks.iter_mut().find(|task| task.id == review_task_id)
        {
            task.status = TaskStatus::InProgress;
            task.agent_name = Some(review_agent_name.to_string());
            task.started_at = Some(now);
            task.finished_at = None;
            task.error = None;
            task.activity_log.clear();
            task.session_id = None;
            task.thread_id = None;
            task.turns_completed = 0;
            task.tokens = TokenUsage::default();
            task.updated_at = now;
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }
        Ok(())
    }

    async fn finish_pull_request_workspace_setup(
        &mut self,
        movement_id: &str,
        workspace_setup_task_id: &str,
        review_task_id: &str,
        workspace_key: &str,
        workspace_path: &Path,
        review_agent_name: &str,
    ) -> Result<(), Error> {
        let now = Utc::now();
        if let Some(movement) = self.state.movements.get_mut(movement_id) {
            if let Some(issue_identifier) = movement.issue_identifier.clone() {
                self.state
                    .workspace_setup_tasks_by_issue_identifier
                    .remove(&issue_identifier);
            }
            if let Some(workspace_key) = movement.workspace_key.clone() {
                self.state
                    .workspace_setup_tasks_by_key
                    .remove(&workspace_key);
            }
            movement.workspace_key = Some(workspace_key.to_string());
            movement.workspace_path = Some(workspace_path.to_path_buf());
            movement.updated_at = now;
            if let Some(store) = &self.store {
                store.save_movement(movement).await?;
            }
        }
        if let Some(tasks) = self.state.tasks.get_mut(movement_id) {
            for task in tasks.iter_mut() {
                if task.id == workspace_setup_task_id {
                    task.status = TaskStatus::Completed;
                    task.finished_at = Some(now);
                    task.error = None;
                    task.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_task(task).await?;
                    }
                } else if task.id == review_task_id {
                    task.status = TaskStatus::InProgress;
                    task.agent_name = Some(review_agent_name.to_string());
                    task.started_at = Some(now);
                    task.finished_at = None;
                    task.error = None;
                    task.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_task(task).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn fail_pull_request_workspace_setup(
        &mut self,
        movement_id: &str,
        workspace_setup_task_id: &str,
        review_task_id: &str,
        error: &str,
    ) -> Result<(), Error> {
        let now = Utc::now();
        if let Some(movement) = self.state.movements.get_mut(movement_id) {
            if let Some(issue_identifier) = movement.issue_identifier.clone() {
                self.state
                    .workspace_setup_tasks_by_issue_identifier
                    .remove(&issue_identifier);
            }
            if let Some(workspace_key) = movement.workspace_key.clone() {
                self.state
                    .workspace_setup_tasks_by_key
                    .remove(&workspace_key);
            }
            movement.status = MovementStatus::Failed;
            movement.updated_at = now;
            if let Some(store) = &self.store {
                store.save_movement(movement).await?;
            }
        }
        if let Some(tasks) = self.state.tasks.get_mut(movement_id) {
            for task in tasks.iter_mut() {
                if task.id == workspace_setup_task_id {
                    task.status = TaskStatus::Failed;
                    task.error = Some(error.to_string());
                    task.finished_at = Some(now);
                    task.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_task(task).await?;
                    }
                } else if task.id == review_task_id {
                    task.status = TaskStatus::Cancelled;
                    task.error = Some("workspace setup failed".into());
                    task.finished_at = Some(now);
                    task.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_task(task).await?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn record_workspace_progress(
        &mut self,
        update: WorkspaceProgressUpdate,
    ) -> Result<(), Error> {
        let task_ref = self
            .state
            .workspace_setup_tasks_by_key
            .get(&update.workspace_key)
            .cloned()
            .or_else(|| {
                self.state
                    .workspace_setup_tasks_by_issue_identifier
                    .get(&update.issue_identifier)
                    .cloned()
            });
        let Some((movement_id, task_id)) = task_ref else {
            return Ok(());
        };
        if let Some(tasks) = self.state.tasks.get_mut(&movement_id)
            && let Some(task) = tasks.iter_mut().find(|task| task.id == task_id)
        {
            if task.activity_log.last().is_some_and(|line| {
                line.strip_prefix('[')
                    .and_then(|line| line.split_once("] "))
                    .map_or(line == update.message.as_str(), |(_, suffix)| {
                        suffix == update.message
                    })
            }) {
                return Ok(());
            }
            let line = format!("[{}] {}", update.at.format("%H:%M:%S"), update.message);
            task.activity_log.push(line);
            const TASK_ACTIVITY_LOG_LIMIT: usize = 64;
            if task.activity_log.len() > TASK_ACTIVITY_LOG_LIMIT {
                let excess = task.activity_log.len() - TASK_ACTIVITY_LOG_LIMIT;
                task.activity_log.drain(0..excess);
            }
            task.updated_at = update.at;
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn retry_pull_request_movement(
        &mut self,
        workflow: LoadedWorkflow,
        movement_id: &str,
        failed_task_ordinal: u32,
    ) -> Result<(), Error> {
        let Some(movement) = self.state.movements.get(movement_id).cloned() else {
            return Ok(());
        };
        let Some(issue_id) = movement.issue_id.clone() else {
            return Ok(());
        };
        let Some(trigger) =
            self.pull_request_retry_trigger(&issue_id, movement.review_target.as_ref())
        else {
            return Err(Error::Core(CoreError::Adapter(format!(
                "no retry trigger available for movement {movement_id}"
            ))));
        };
        let workspace_path = movement.workspace_path.clone();
        if failed_task_ordinal == 0 || workspace_path.is_none() {
            return self
                .dispatch_pull_request_trigger(workflow, trigger, None, None)
                .await;
        }
        let Some(workspace_path) = workspace_path else {
            return self
                .dispatch_pull_request_trigger(workflow, trigger, None, None)
                .await;
        };

        let review_agent = workflow
            .config
            .pr_review_agent()?
            .ok_or_else(|| CoreError::Adapter("PR review agent is not available".into()))?;
        let review_task_id = self
            .state
            .tasks
            .get(movement_id)
            .and_then(|tasks| {
                tasks
                    .iter()
                    .find(|task| task.ordinal == failed_task_ordinal)
                    .map(|task| task.id.clone())
            })
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "movement {movement_id} has no task at ordinal {failed_task_ordinal}"
                )))
            })?;
        self.reset_pull_request_review_task_for_retry(
            movement_id,
            &review_task_id,
            &review_agent.name,
        )
        .await?;

        match trigger {
            PullRequestTrigger::Review(trigger) => {
                let issue = synthetic_issue_for_pull_request_review(&trigger);
                let issue_identifier = issue.identifier.clone();
                let prompt = self.build_pull_request_review_prompt(
                    &workflow,
                    &trigger,
                    &issue,
                    &review_agent.name,
                    movement_id,
                    None,
                )?;
                self.start_pull_request_worker(
                    workflow,
                    issue,
                    None,
                    workspace_path.clone(),
                    prompt,
                    review_agent,
                    movement_id,
                    &review_task_id,
                    trigger.review_target(),
                    pull_request_review_comment_marker(&trigger.review_target()),
                    format!("retried PR review {issue_identifier}"),
                    "PR review worker relaunched",
                    "review",
                )
                .await
            },
            PullRequestTrigger::Comment(trigger) => {
                let issue = synthetic_issue_for_pull_request_comment(&trigger);
                let issue_identifier = issue.identifier.clone();
                let prompt = self.build_pull_request_comment_review_prompt(
                    &workflow,
                    &trigger,
                    &issue,
                    &review_agent.name,
                    movement_id,
                    None,
                )?;
                self.start_pull_request_worker(
                    workflow,
                    issue,
                    None,
                    workspace_path,
                    prompt,
                    review_agent,
                    movement_id,
                    &review_task_id,
                    trigger.review_target(),
                    pull_request_comment_review_comment_marker(
                        &trigger.review_target(),
                        &trigger.thread_id,
                    ),
                    format!("retried PR comment review {issue_identifier}"),
                    "PR comment review worker relaunched",
                    "comment",
                )
                .await
            },
            PullRequestTrigger::Conflict(_) => Err(Error::Core(CoreError::Adapter(
                "retry for PR conflict review is not implemented".into(),
            ))),
        }
    }

    async fn finalize_pull_request_review_task(
        &mut self,
        movement_id: Option<&str>,
        task_id: Option<&str>,
        running: &RunningTask,
        outcome: &AgentRunResult,
        task_error: Option<String>,
    ) -> Result<(), Error> {
        let Some(movement_id) = movement_id else {
            return Ok(());
        };
        let Some(task_id) = task_id else {
            return Ok(());
        };
        let now = Utc::now();
        if let Some(tasks) = self.state.tasks.get_mut(movement_id)
            && let Some(task) = tasks.iter_mut().find(|task| task.id == task_id)
        {
            task.status = match outcome.status {
                AttemptStatus::Succeeded if task_error.is_none() => TaskStatus::Completed,
                AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser => {
                    TaskStatus::Cancelled
                },
                _ => TaskStatus::Failed,
            };
            task.turns_completed = outcome.turns_completed;
            task.tokens = running.tokens.clone();
            task.error = task_error.or_else(|| outcome.error.clone());
            task.finished_at = Some(now);
            task.updated_at = now;
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }
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
                    self.finalize_pull_request_review_task(
                        movement_id.as_deref(),
                        running.active_task_id.as_deref(),
                        &running,
                        &outcome,
                        Some(error.to_string()),
                    )
                    .await?;
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
                    self.finalize_pull_request_review_task(
                        movement_id.as_deref(),
                        running.active_task_id.as_deref(),
                        &running,
                        &outcome,
                        None,
                    )
                    .await?;
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
                self.finalize_pull_request_review_task(
                    movement_id.as_deref(),
                    running.active_task_id.as_deref(),
                    &running,
                    &outcome,
                    None,
                )
                .await?;
                self.release_issue(&issue_id);
            },
            _ => {
                self.finalize_pull_request_review_task(
                    movement_id.as_deref(),
                    running.active_task_id.as_deref(),
                    &running,
                    &outcome,
                    None,
                )
                .await?;
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
        let verdict = parse_review_verdict(trimmed);
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
        let verdict_badge = match verdict {
            polyphony_core::ReviewVerdict::Approve => {
                "![Approved](https://img.shields.io/badge/Approved-success?style=flat-square)  "
            },
            polyphony_core::ReviewVerdict::RequestChanges => {
                "![Changes Requested](https://img.shields.io/badge/Changes%20Requested-critical?style=flat-square)  "
            },
            polyphony_core::ReviewVerdict::Comment => {
                "![Review](https://img.shields.io/badge/Review-blue?style=flat-square)  "
            },
        };
        let body = format!("{verdict_badge}\n\n{trimmed}\n\n{marker}");
        let pull_request = PullRequestRef {
            repository: review_target.repository.clone(),
            number: review_target.number,
            url: review_target.url.clone(),
        };
        let uses_review_api =
            comment_mode == "inline" || verdict != polyphony_core::ReviewVerdict::Comment;
        if uses_review_api {
            commenter
                .sync_pull_request_review(
                    &pull_request,
                    &marker,
                    &body,
                    &review_comments,
                    &review_target.head_sha,
                    verdict,
                )
                .await?;
        } else {
            commenter
                .sync_pull_request_comment(&pull_request, &marker, &body)
                .await?;
        }

        // Create a deliverable so the movement detail shows the review outcome.
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "verdict".into(),
            serde_json::Value::String(verdict.to_string()),
        );
        if !review_comments.is_empty() {
            metadata.insert(
                "inline_comments".into(),
                serde_json::Value::Number(review_comments.len().into()),
            );
        }
        if let Some(confidence) = parse_review_confidence(trimmed) {
            metadata.insert(
                "confidence".into(),
                serde_json::Value::String(format!("{confidence}/5")),
            );
        }
        // Extract the Summary section content as a short description for the
        // deliverable.  Falls back to the first non-heading, non-verdict line.
        let summary = extract_review_summary(trimmed);
        if let Some(movement_id) = running.movement_id.as_ref()
            && let Some(movement) = self.state.movements.get_mut(movement_id)
        {
            movement.deliverable = Some(polyphony_core::Deliverable {
                kind: polyphony_core::DeliverableKind::PullRequestReview,
                status: polyphony_core::DeliverableStatus::Reviewed,
                url: review_target.url.clone(),
                decision: polyphony_core::DeliverableDecision::Accepted,
                title: Some(format!("Review: {verdict}")),
                description: Some(summary),
                metadata,
            });
            movement.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_movement(movement).await;
            }
        }

        self.push_event(
            EventScope::Handoff,
            if uses_review_api && !review_comments.is_empty() {
                format!(
                    "{} reviewed PR #{} at {} ({verdict}) with {} inline comments",
                    running.issue.identifier,
                    review_target.number,
                    review_target.head_sha,
                    review_comments.len()
                )
            } else {
                format!(
                    "{} reviewed PR #{} at {} ({verdict})",
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

fn upsert_pull_request_task(
    tasks: &mut Vec<polyphony_core::Task>,
    movement_id: &str,
    ordinal: u32,
    title: &str,
    category: polyphony_core::TaskCategory,
    agent_name: Option<String>,
) -> TaskId {
    if let Some(task) = tasks.iter_mut().find(|task| task.ordinal == ordinal) {
        task.title = title.to_string();
        task.category = category;
        task.agent_name = agent_name;
        return task.id.clone();
    }

    let now = Utc::now();
    let task = polyphony_core::Task {
        id: format!("task-{}", uuid::Uuid::new_v4()),
        movement_id: movement_id.to_string(),
        title: title.to_string(),
        description: None,
        activity_log: Vec::new(),
        category,
        status: TaskStatus::Pending,
        ordinal,
        parent_id: None,
        agent_name,
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: None,
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    let task_id = task.id.clone();
    tasks.push(task);
    task_id
}
