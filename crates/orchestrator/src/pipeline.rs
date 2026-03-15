use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn dispatch_pipeline(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        prefer_alternate_agent: bool,
        skip_workspace_sync: bool,
    ) -> Result<(), Error> {
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

        let movement_id = new_movement_id();
        let has_planner = workflow.config.pipeline.planner_agent.is_some();
        let initial_status = if has_planner {
            MovementStatus::Planning
        } else {
            MovementStatus::InProgress
        };
        let now = Utc::now();
        let movement = Movement {
            id: movement_id.clone(),
            kind: MovementKind::IssueDelivery,
            issue_id: Some(issue.id.clone()),
            issue_identifier: Some(issue.identifier.clone()),
            title: issue.title.clone(),
            status: initial_status,
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

        if has_planner {
            self.dispatch_planner_task(
                &workflow,
                &issue,
                attempt,
                prefer_alternate_agent,
                &movement_id,
                &workspace.path,
            )
            .await
        } else {
            let tasks =
                self.create_tasks_from_stages(&workflow.config.pipeline.stages, &movement_id);
            if let Some(store) = &self.store {
                for task in &tasks {
                    store.save_task(task).await?;
                }
            }
            self.state.tasks.insert(movement_id.clone(), tasks);
            self.dispatch_next_task(
                workflow,
                issue,
                attempt,
                prefer_alternate_agent,
                &movement_id,
                &workspace.path,
            )
            .await
        }
    }

    pub(crate) fn create_tasks_from_stages(
        &self,
        stages: &[polyphony_workflow::PipelineStageConfig],
        movement_id: &str,
    ) -> Vec<Task> {
        let now = Utc::now();
        stages
            .iter()
            .enumerate()
            .map(|(index, stage)| {
                let category = match stage.category.to_ascii_lowercase().as_str() {
                    "research" => polyphony_core::TaskCategory::Research,
                    "coding" => polyphony_core::TaskCategory::Coding,
                    "testing" => polyphony_core::TaskCategory::Testing,
                    "documentation" => polyphony_core::TaskCategory::Documentation,
                    "review" => polyphony_core::TaskCategory::Review,
                    _ => polyphony_core::TaskCategory::Coding,
                };
                Task {
                    id: format!("task-{}", uuid::Uuid::new_v4()),
                    movement_id: movement_id.to_string(),
                    title: format!("{} stage", stage.category),
                    description: None,
                    category,
                    status: TaskStatus::Pending,
                    ordinal: (index + 1) as u32,
                    parent_id: None,
                    agent_name: stage.agent.clone(),
                    turns_completed: 0,
                    tokens: TokenUsage::default(),
                    started_at: None,
                    finished_at: None,
                    error: None,
                    created_at: now,
                    updated_at: now,
                }
            })
            .collect()
    }

    pub(crate) async fn dispatch_planner_task(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        attempt: Option<u32>,
        _prefer_alternate_agent: bool,
        movement_id: &str,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        let planner_agent_name =
            workflow
                .config
                .pipeline
                .planner_agent
                .as_ref()
                .ok_or_else(|| {
                    Error::Core(CoreError::Adapter(
                        "pipeline.planner_agent is required".into(),
                    ))
                })?;
        let profile = workflow
            .config
            .agents
            .profiles
            .get(planner_agent_name)
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "unknown planner agent `{planner_agent_name}`"
                )))
            })?;
        let selected_agent = agent_definition(planner_agent_name, profile);

        let prompt = workflow
            .config
            .pipeline
            .planner_prompt
            .as_deref()
            .map(|template| render_issue_template_with_strings(template, issue, attempt, &[]))
            .unwrap_or_else(|| {
                render_issue_template_with_strings(DEFAULT_PLANNER_PROMPT, issue, attempt, &[])
            })?;
        let prompt = apply_agent_prompt_template(
            workflow,
            &selected_agent.name,
            prompt,
            issue,
            attempt,
            1,
            workflow.config.agent.max_turns,
        )?;

        self.spawn_pipeline_worker(
            workflow.clone(),
            issue.clone(),
            attempt,
            workspace_path.to_path_buf(),
            prompt,
            selected_agent,
            None,
            Some(movement_id.to_string()),
        )
        .await
    }

    pub(crate) async fn dispatch_next_task(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        _prefer_alternate_agent: bool,
        movement_id: &str,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        let next_task = self.state.tasks.get(movement_id).and_then(|tasks| {
            tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Pending)
                .min_by_key(|task| task.ordinal)
                .cloned()
        });

        let Some(task) = next_task else {
            self.complete_pipeline(&workflow, &issue, movement_id)
                .await?;
            return Ok(());
        };

        // Select agent for this task
        let agent_name = task
            .agent_name
            .clone()
            .or_else(|| {
                workflow
                    .config
                    .pipeline
                    .stages
                    .iter()
                    .find(|s| s.category.eq_ignore_ascii_case(&task.category.to_string()))
                    .and_then(|s| s.agent.clone())
            })
            .or_else(|| workflow.config.agents.default.clone())
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(
                    "no agent available for pipeline task".into(),
                ))
            })?;

        let profile = workflow
            .config
            .agents
            .profiles
            .get(&agent_name)
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "unknown agent `{agent_name}` for pipeline task"
                )))
            })?;
        let selected_agent = agent_definition(&agent_name, profile);

        // Build task prompt with pipeline context
        let prompt = self.build_task_prompt(
            &workflow,
            &selected_agent.name,
            &issue,
            &task,
            movement_id,
            attempt,
            workflow.config.agent.max_turns,
        )?;

        // Mark task in progress
        if let Some(tasks) = self.state.tasks.get_mut(movement_id)
            && let Some(t) = tasks.iter_mut().find(|t| t.id == task.id)
        {
            t.status = TaskStatus::InProgress;
            t.started_at = Some(Utc::now());
            t.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_task(t).await?;
            }
        }

        self.spawn_pipeline_worker(
            workflow,
            issue,
            attempt,
            workspace_path.to_path_buf(),
            prompt,
            selected_agent,
            Some(task.id.clone()),
            Some(movement_id.to_string()),
        )
        .await
    }

    pub(crate) fn build_task_prompt(
        &self,
        workflow: &LoadedWorkflow,
        agent_name: &str,
        issue: &Issue,
        task: &Task,
        movement_id: &str,
        attempt: Option<u32>,
        max_turns: u32,
    ) -> Result<String, Error> {
        let tasks = self.state.tasks.get(movement_id);
        let completed_tasks: Vec<String> = tasks
            .map(|ts| {
                ts.iter()
                    .filter(|t| t.status == TaskStatus::Completed)
                    .map(|t| {
                        format!(
                            "- [{}] {}: {}",
                            t.category,
                            t.title,
                            t.description.as_deref().unwrap_or("completed")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let total_tasks = tasks.map(|ts| ts.len()).unwrap_or(0);

        // Read plan.json if it exists
        let has_plan = self
            .state
            .movements
            .get(movement_id)
            .and_then(|m| m.workspace_path.as_ref())
            .is_some_and(|path| path.join(".polyphony").join("plan.json").exists());

        // Render the workflow template with pipeline context injected
        let base_prompt = apply_agent_prompt_template(
            workflow,
            agent_name,
            render_turn_prompt(&workflow.definition, issue, attempt, 1, max_turns)?,
            issue,
            attempt,
            1,
            max_turns,
        )?;

        let mut prompt = base_prompt;
        prompt.push_str(&format!(
            "\n\n## Pipeline Task {}/{}\n\
             **Task:** {}\n\
             **Category:** {}\n",
            task.ordinal, total_tasks, task.title, task.category
        ));
        if let Some(desc) = &task.description {
            prompt.push_str(&format!("**Description:** {desc}\n"));
        }
        if !completed_tasks.is_empty() {
            prompt.push_str("\n### Completed tasks\n");
            prompt.push_str(&completed_tasks.join("\n"));
            prompt.push('\n');
        }
        if has_plan {
            prompt.push_str("\n### Execution plan\nThe full plan is in `.polyphony/plan.json`.\n");
        }
        prompt.push_str(
            "\nRead any workspace artifacts from previous tasks in `.polyphony/` for context.\n",
        );

        Ok(prompt)
    }

    pub(crate) async fn spawn_pipeline_worker(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        workspace_path: PathBuf,
        prompt: String,
        selected_agent: polyphony_core::AgentDefinition,
        active_task_id: Option<TaskId>,
        movement_id: Option<MovementId>,
    ) -> Result<(), Error> {
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
        let started_at = Utc::now();
        let selected_agent_for_task = selected_agent.clone();
        let workspace_path_for_running = workspace_path.clone();

        info!(
            issue_identifier = %issue.identifier,
            agent = %selected_agent.name,
            task_id = ?active_task_id,
            movement_id = ?movement_id,
            "dispatching pipeline worker"
        );

        let worker_span = info_span!(
            "pipeline_worker",
            issue_identifier = %issue_identifier_for_task,
            agent = %selected_agent_for_task.name,
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
                    None,
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
            workspace_path: workspace_path_for_running,
            stall_timeout_ms: selected_agent.stall_timeout_ms,
            max_turns,
            started_at,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("pipeline_dispatch_started".into()),
            last_message: Some("pipeline worker launched".into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            handle,
            active_task_id,
            movement_id,
            review_target: None,
            review_comment_marker: None,
        });
        self.push_event(
            EventScope::Dispatch,
            format!("pipeline dispatched {issue_identifier}"),
        );
        Ok(())
    }

    pub(crate) async fn handle_planner_finished(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        movement_id: &str,
        workspace_path: &Path,
        outcome: &AgentRunResult,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        if !matches!(outcome.status, AttemptStatus::Succeeded) {
            warn!(
                issue_identifier = %issue.identifier,
                movement_id,
                "planner failed, marking movement as failed"
            );
            if let Some(movement) = self.state.movements.get_mut(movement_id) {
                movement.status = MovementStatus::Failed;
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_movement(movement).await?;
                }
            }
            return Ok(());
        }

        let plan_path = workspace_path.join(".polyphony").join("plan.json");
        let plan_raw = tokio::fs::read_to_string(&plan_path)
            .await
            .map_err(|error| {
                Error::Core(CoreError::Adapter(format!(
                    "failed to read plan.json: {error}"
                )))
            })?;
        let plan: PipelinePlan = serde_json::from_str(&plan_raw).map_err(|error| {
            Error::Core(CoreError::Adapter(format!(
                "failed to parse plan.json: {error}"
            )))
        })?;

        if plan.tasks.is_empty() {
            warn!(
                issue_identifier = %issue.identifier,
                "planner produced empty plan"
            );
            if let Some(movement) = self.state.movements.get_mut(movement_id) {
                movement.status = MovementStatus::Failed;
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_movement(movement).await?;
                }
            }
            return Ok(());
        }

        // Validate agent names
        for planned_task in &plan.tasks {
            if let Some(agent_name) = &planned_task.agent
                && !workflow.config.agents.profiles.contains_key(agent_name)
            {
                warn!(
                    issue_identifier = %issue.identifier,
                    agent = agent_name,
                    "planner referenced unknown agent, ignoring agent hint"
                );
            }
        }

        let tasks: Vec<Task> = plan
            .tasks
            .iter()
            .enumerate()
            .map(|(index, planned)| planned.to_task(movement_id, (index + 1) as u32))
            .collect();

        if let Some(store) = &self.store {
            for task in &tasks {
                store.save_task(task).await?;
            }
        }
        self.state.tasks.insert(movement_id.to_string(), tasks);

        if let Some(movement) = self.state.movements.get_mut(movement_id) {
            movement.status = MovementStatus::InProgress;
            movement.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_movement(movement).await?;
            }
        }

        self.push_event(
            EventScope::Dispatch,
            format!(
                "{} planner created {} tasks",
                issue.identifier,
                self.state
                    .tasks
                    .get(movement_id)
                    .map(|t| t.len())
                    .unwrap_or(0)
            ),
        );

        self.dispatch_next_task(
            self.workflow(),
            issue.clone(),
            attempt,
            false,
            movement_id,
            workspace_path,
        )
        .await
    }

    pub(crate) async fn handle_task_finished(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        movement_id: &str,
        task_id: &str,
        workspace_path: &Path,
        outcome: &AgentRunResult,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        let now = Utc::now();
        if let Some(tasks) = self.state.tasks.get_mut(movement_id)
            && let Some(task) = tasks.iter_mut().find(|t| t.id == task_id)
        {
            task.status = if matches!(outcome.status, AttemptStatus::Succeeded) {
                TaskStatus::Completed
            } else {
                TaskStatus::Failed
            };
            task.turns_completed = outcome.turns_completed;
            task.error = outcome.error.clone();
            task.finished_at = Some(now);
            task.updated_at = now;
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }

        if matches!(outcome.status, AttemptStatus::Succeeded) {
            self.dispatch_next_task(
                self.workflow(),
                issue.clone(),
                attempt,
                false,
                movement_id,
                workspace_path,
            )
            .await
        } else {
            if workflow.config.pipeline.replan_on_failure
                && workflow.config.pipeline.planner_agent.is_some()
            {
                self.push_event(
                    EventScope::Dispatch,
                    format!("{} task failed, re-running planner", issue.identifier),
                );
                // Reset tasks and re-plan
                if let Some(tasks) = self.state.tasks.get_mut(movement_id) {
                    for task in tasks.iter_mut() {
                        if task.status == TaskStatus::Pending {
                            task.status = TaskStatus::Cancelled;
                            task.updated_at = Utc::now();
                        }
                    }
                }
                if let Some(movement) = self.state.movements.get_mut(movement_id) {
                    movement.status = MovementStatus::Planning;
                    movement.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        store.save_movement(movement).await?;
                    }
                }
                return self
                    .dispatch_planner_task(
                        workflow,
                        issue,
                        attempt,
                        false,
                        movement_id,
                        workspace_path,
                    )
                    .await;
            }
            // Mark movement as failed
            if let Some(movement) = self.state.movements.get_mut(movement_id) {
                movement.status = MovementStatus::Failed;
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_movement(movement).await?;
                }
            }
            Ok(())
        }
    }

    pub(crate) async fn complete_pipeline(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        movement_id: &str,
    ) -> Result<(), Error> {
        let status = if workflow.config.automation.enabled {
            MovementStatus::Review
        } else {
            MovementStatus::Delivered
        };
        if let Some(movement) = self.state.movements.get_mut(movement_id) {
            movement.status = status;
            movement.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_movement(movement).await?;
            }
        }
        self.push_event(
            EventScope::Dispatch,
            format!("{} pipeline completed ({:?})", issue.identifier, status),
        );
        Ok(())
    }
}
