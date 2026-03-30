use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn dispatch_pipeline(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        prefer_alternate_agent: bool,
        skip_workspace_sync: bool,
        directives: Option<&str>,
    ) -> Result<(), Error> {
        let manual_dispatch_directives = directives
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned);
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

        let tracker = self.tracker_for_issue(&issue.id);
        if let Err(error) = tracker.ensure_issue_workflow_tracking(&issue).await {
            warn!(%error, issue_identifier = %issue.identifier, "issue workflow tracking setup failed");
        }
        if let Err(error) = tracker
            .update_issue_workflow_status(&issue, "In Progress")
            .await
        {
            warn!(%error, issue_identifier = %issue.identifier, "issue workflow status sync failed");
        }

        let has_planner = workflow.config.router_agent_name().is_some();
        let initial_status = if has_planner {
            RunStatus::Planning
        } else {
            RunStatus::InProgress
        };
        // Reuse an existing active run for this issue if one exists,
        // otherwise create a new one.
        let (run_id, existing_stage) =
            if let Some(existing_id) = self.find_existing_run_for_issue(&issue.id) {
                let stage = self
                    .state
                    .runs
                    .get(&existing_id)
                    .and_then(|m| m.pipeline_stage);
                // Determine what status this run should have on reuse.
                let reuse_status = match stage {
                    Some(PipelineStage::Completing) => RunStatus::Delivered,
                    Some(PipelineStage::Executing) => RunStatus::InProgress,
                    _ => initial_status,
                };
                let past_planning = matches!(
                    stage,
                    Some(PipelineStage::Executing) | Some(PipelineStage::Completing)
                );
                if let Some(run) = self.state.runs.get_mut(&existing_id) {
                    run.status = reuse_status;
                    run.manual_dispatch_directives = manual_dispatch_directives.clone();
                    run.updated_at = Utc::now();
                    if !past_planning {
                        run.pipeline_stage = if has_planner {
                            Some(PipelineStage::Planning)
                        } else {
                            Some(PipelineStage::Executing)
                        };
                    }
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                info!(
                    issue_identifier = %issue.identifier,
                    run_id = %existing_id,
                    workspace_path = %workspace.path.display(),
                    has_planner,
                    existing_stage = ?stage,
                    reuse_status = ?reuse_status,
                    "pipeline run reused"
                );
                (existing_id, stage)
            } else {
                let run_id = new_run_id();
                let now = Utc::now();
                let pipeline_stage = if has_planner {
                    Some(PipelineStage::Planning)
                } else {
                    Some(PipelineStage::Executing)
                };
                let initial_steps = if has_planner {
                    polyphony_core::build_planner_steps()
                } else {
                    Vec::new()
                };
                let run = Run {
                    id: run_id.clone(),
                    kind: RunKind::IssueDelivery,
                    issue_id: Some(issue.id.clone()),
                    issue_identifier: Some(issue.identifier.clone()),
                    title: issue.title.clone(),
                    status: initial_status,
                    pipeline_stage,
                    manual_dispatch_directives: manual_dispatch_directives.clone(),
                    workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
                    workspace_path: Some(workspace.path.clone()),
                    review_target: None,
                    deliverable: None,
                    created_at: now,
                    updated_at: now,
                    cancel_reason: None,
                    steps: initial_steps,
                    activity_log: Vec::new(),
                };
                if let Some(store) = &self.store {
                    store.save_run(&run).await?;
                }
                self.state.runs.insert(run_id.clone(), run);
                info!(
                    issue_identifier = %issue.identifier,
                    run_id,
                    workspace_path = %workspace.path.display(),
                    has_planner,
                    initial_status = ?initial_status,
                    "pipeline run created"
                );
                (run_id, None)
            };

        // Pipeline already completed — check for deliverable, mark failed if no output.
        if matches!(existing_stage, Some(PipelineStage::Completing)) {
            let has_deliverable = self
                .state
                .runs
                .get(&run_id)
                .is_some_and(|m| m.deliverable.is_some());
            if !has_deliverable {
                // Try to detect any changes in the workspace
                self.create_local_branch_deliverable_from_workspace(&run_id, &workspace.path)
                    .await;
            }
            // Check if the deliverable has actual changes
            let deliverable = self
                .state
                .runs
                .get(&run_id)
                .and_then(|m| m.deliverable.as_ref());
            let confirmed_no_changes = deliverable.is_some_and(|d| {
                d.metadata
                    .get("lines_added")
                    .and_then(|v| v.as_u64())
                    .is_some_and(|added| added == 0)
            });
            let no_output = confirmed_no_changes || deliverable.is_none();
            if no_output {
                warn!(
                    issue_identifier = %issue.identifier,
                    run_id,
                    "pipeline completed with no code changes — marking as failed"
                );
                if let Some(run) = self.state.runs.get_mut(&run_id) {
                    run.status = RunStatus::Failed;
                    run.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "{} pipeline failed: completed without producing any code changes",
                        issue.identifier
                    ),
                );
            } else {
                info!(
                    issue_identifier = %issue.identifier,
                    run_id,
                    "pipeline already completed, skipping re-dispatch"
                );
            }
            return Ok(());
        }

        // Tasks exist from a prior planner run — skip the router and resume.
        if matches!(existing_stage, Some(PipelineStage::Executing)) {
            info!(
                issue_identifier = %issue.identifier,
                run_id,
                "skipping planner — resuming from next pending task"
            );
            return self
                .dispatch_next_task(
                    workflow,
                    issue,
                    attempt,
                    prefer_alternate_agent,
                    &run_id,
                    &workspace.path,
                )
                .await;
        }

        if has_planner {
            self.dispatch_planner_task(
                &workflow,
                &issue,
                attempt,
                prefer_alternate_agent,
                &run_id,
                &workspace.path,
            )
            .await
        } else {
            let tasks = self.create_tasks_from_stages(&workflow.config.pipeline.stages, &run_id);
            if let Some(store) = &self.store {
                for task in &tasks {
                    store.save_task(task).await?;
                }
            }
            info!(
                issue_identifier = %issue.identifier,
                run_id,
                stage_tasks = tasks.len(),
                "pipeline stages expanded without planner"
            );
            // Populate delivery steps from the created tasks.
            let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
            if let Some(run) = self.state.runs.get_mut(&run_id) {
                run.steps = polyphony_core::build_delivery_steps(
                    &task_ids,
                    workflow.config.automation.enabled,
                    workflow.config.pr_review_agent().ok().flatten().is_some(),
                );
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            self.state.tasks.insert(run_id.clone(), tasks);
            self.dispatch_next_task(
                workflow,
                issue,
                attempt,
                prefer_alternate_agent,
                &run_id,
                &workspace.path,
            )
            .await
        }
    }

    pub(crate) fn create_tasks_from_stages(
        &self,
        stages: &[polyphony_workflow::PipelineStageConfig],
        run_id: &str,
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
                    run_id: run_id.to_string(),
                    title: format!("{} stage", stage.category),
                    description: None,
                    activity_log: Vec::new(),
                    category,
                    status: TaskStatus::Pending,
                    ordinal: (index + 1) as u32,
                    parent_id: None,
                    agent_name: stage.agent.clone(),
                    session_id: None,
                    thread_id: None,
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
        run_id: &str,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        let planner_agent_name = workflow.config.router_agent_name().ok_or_else(|| {
            Error::Core(CoreError::Adapter(
                "orchestration.router_agent or pipeline.planner_agent is required".into(),
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
        let selected_agent = agent_definition_with_pty(
            planner_agent_name,
            profile,
            workflow.config.agent.pty_backend,
        );
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            planner_agent = %selected_agent.name,
            attempt = attempt.unwrap_or(0),
            "dispatching pipeline planner"
        );

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
        let prompt =
            prepend_manual_dispatch_directives(prompt, self.manual_dispatch_directives(run_id));

        // Mark the PlannerRun step as running.
        if let Some(run) = self.state.runs.get_mut(run_id)
            && let Some(step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == polyphony_core::StepKind::PlannerRun && !s.is_complete())
        {
            step.mark_running();
        }

        self.spawn_pipeline_worker(
            workflow.clone(),
            issue.clone(),
            attempt,
            workspace_path.to_path_buf(),
            prompt,
            selected_agent,
            None,
            Some(run_id.to_string()),
            None,
        )
        .await
    }

    pub(crate) async fn dispatch_next_task(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        _prefer_alternate_agent: bool,
        run_id: &str,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        let next_task = self.state.tasks.get(run_id).and_then(|tasks| {
            tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Pending)
                .min_by_key(|task| task.ordinal)
                .cloned()
        });

        let Some(task) = next_task else {
            self.complete_pipeline(&workflow, &issue, run_id).await?;
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
        let selected_agent =
            agent_definition_with_pty(&agent_name, profile, workflow.config.agent.pty_backend);
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            task_id = %task.id,
            task_title = %task.title,
            task_category = %task.category,
            task_ordinal = task.ordinal,
            task_count = self.state.tasks.get(run_id).map(|tasks| tasks.len()).unwrap_or(0),
            selected_agent = %selected_agent.name,
            "dispatching next pipeline task"
        );

        // Build task prompt with pipeline context
        let prompt = self.build_task_prompt(
            &workflow,
            &selected_agent.name,
            &issue,
            &task,
            run_id,
            attempt,
            workflow.config.agent.max_turns,
        )?;

        // Mark task in progress
        if let Some(tasks) = self.state.tasks.get_mut(run_id)
            && let Some(t) = tasks.iter_mut().find(|t| t.id == task.id)
        {
            t.status = TaskStatus::InProgress;
            t.started_at = Some(Utc::now());
            t.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_task(t).await?;
            }
        }

        // Build prior context from the task's stored session info for resume.
        let prior_context = if task.session_id.is_some() || task.thread_id.is_some() {
            Some(AgentContextSnapshot {
                repo_id: self.repo_id_for_issue(&issue.id),
                issue_id: issue.id.clone(),
                issue_identifier: issue.identifier.clone(),
                updated_at: Utc::now(),
                agent_name: selected_agent.name.clone(),
                model: selected_agent.model.clone(),
                session_id: task.session_id.clone(),
                thread_id: task.thread_id.clone(),
                turn_id: None,
                codex_app_server_pid: None,
                status: None,
                error: None,
                usage: TokenUsage::default(),
                transcript: Vec::new(),
            })
        } else {
            None
        };

        // Mark the AgentRun step as running.
        if let Some(run) = self.state.runs.get_mut(run_id)
            && let Some(step) = run.steps.iter_mut().find(|s| {
                s.kind == polyphony_core::StepKind::AgentRun
                    && s.task_id.as_deref() == Some(&task.id)
                    && !s.is_complete()
            })
        {
            step.mark_running();
        }

        self.spawn_pipeline_worker(
            workflow,
            issue,
            attempt,
            workspace_path.to_path_buf(),
            prompt,
            selected_agent,
            Some(task.id.clone()),
            Some(run_id.to_string()),
            prior_context,
        )
        .await
    }

    pub(crate) fn build_task_prompt(
        &self,
        workflow: &LoadedWorkflow,
        agent_name: &str,
        issue: &Issue,
        task: &Task,
        run_id: &str,
        attempt: Option<u32>,
        max_turns: u32,
    ) -> Result<String, Error> {
        let tasks = self.state.tasks.get(run_id);
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
            .runs
            .get(run_id)
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

        let mut prompt = prepend_manual_dispatch_directives(
            base_prompt,
            self.manual_dispatch_directives(run_id),
        );
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

    fn manual_dispatch_directives(&self, run_id: &str) -> Option<&str> {
        self.state
            .runs
            .get(run_id)
            .and_then(|run| run.manual_dispatch_directives.as_deref())
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
        run_id: Option<RunId>,
        prior_context: Option<AgentContextSnapshot>,
    ) -> Result<(), Error> {
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let issue_identifier_for_task = issue_identifier.clone();
        let issue_for_task = issue.clone();
        let command_tx = self.command_tx.clone();
        let agent = self.agent_for_issue(&issue.id);
        let tracker = self.tracker_for_issue(&issue.id);
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
            run_id = ?run_id,
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
                    prior_context,
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
            run_id,
            review_target: None,
            review_comment_marker: None,
            recent_log: VecDeque::new(),
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
        run_id: &str,
        workspace_path: &Path,
        outcome: &AgentRunResult,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        if !matches!(outcome.status, AttemptStatus::Succeeded) {
            warn!(
                issue_identifier = %issue.identifier,
                run_id,
                "planner failed, marking run as failed"
            );
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
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
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            plan_path = %plan_path.display(),
            planned_tasks = plan.tasks.len(),
            "planner output loaded"
        );

        if plan.tasks.is_empty() {
            warn!(
                issue_identifier = %issue.identifier,
                "planner produced empty plan"
            );
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return Ok(());
        }

        let planned_tasks = plan
            .tasks
            .iter()
            .cloned()
            .map(|mut planned_task| {
                if let Some(agent_name) = &planned_task.agent
                    && !workflow.config.agents.profiles.contains_key(agent_name)
                {
                    warn!(
                        issue_identifier = %issue.identifier,
                        agent = agent_name,
                        "planner referenced unknown agent, ignoring agent hint"
                    );
                    planned_task.agent = None;
                }
                planned_task
            })
            .collect::<Vec<_>>();

        let tasks: Vec<Task> = planned_tasks
            .iter()
            .enumerate()
            .map(|(index, planned)| planned.to_task(run_id, (index + 1) as u32))
            .collect();

        if let Some(store) = &self.store {
            for task in &tasks {
                store.save_task(task).await?;
            }
        }
        for task in &tasks {
            info!(
                issue_identifier = %issue.identifier,
                run_id,
                task_id = %task.id,
                task_title = %task.title,
                task_category = %task.category,
                task_ordinal = task.ordinal,
                assigned_agent = task.agent_name.as_deref().unwrap_or("auto"),
                "planner task registered"
            );
        }
        // Build delivery steps from the tasks the planner created.
        let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        self.state.tasks.insert(run_id.to_string(), tasks);

        if let Some(run) = self.state.runs.get_mut(run_id) {
            // Mark the PlannerRun step as succeeded.
            if let Some(planner_step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == polyphony_core::StepKind::PlannerRun)
            {
                planner_step.mark_succeeded();
            }
            // Append the delivery steps (AgentRun per task + handoff steps).
            let next_ordinal = run.steps.last().map(|s| s.ordinal + 1).unwrap_or(0);
            let mut delivery_steps = polyphony_core::build_delivery_steps(
                &task_ids,
                workflow.config.automation.enabled,
                workflow.config.pr_review_agent().ok().flatten().is_some(),
            );
            for (i, step) in delivery_steps.iter_mut().enumerate() {
                step.ordinal = next_ordinal + i as u32;
            }
            run.steps.extend(delivery_steps);

            run.status = RunStatus::InProgress;
            run.pipeline_stage = Some(PipelineStage::Executing);
            run.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!(
                    "planning → executing: {} tasks created",
                    self.state.tasks.get(run_id).map(|t| t.len()).unwrap_or(0)
                ),
            );
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }

        self.push_event(
            EventScope::Dispatch,
            format!(
                "{} planner created {} tasks",
                issue.identifier,
                self.state.tasks.get(run_id).map(|t| t.len()).unwrap_or(0)
            ),
        );

        self.dispatch_next_task(
            self.workflow(),
            issue.clone(),
            attempt,
            false,
            run_id,
            workspace_path,
        )
        .await
    }

    pub(crate) async fn handle_task_finished(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        run_id: &str,
        task_id: &str,
        workspace_path: &Path,
        outcome: &AgentRunResult,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        let now = Utc::now();
        let task_snapshot = self
            .state
            .tasks
            .get(run_id)
            .and_then(|tasks| tasks.iter().find(|t| t.id == task_id))
            .cloned();
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            task_id,
            task_title = task_snapshot.as_ref().map(|task| task.title.as_str()).unwrap_or("unknown"),
            task_category = task_snapshot.as_ref().map(|task| task.category.to_string()).unwrap_or_else(|| "unknown".into()),
            status = ?outcome.status,
            turns_completed = outcome.turns_completed,
            error = outcome.error.as_deref().unwrap_or("none"),
            "pipeline task finished"
        );
        if let Some(tasks) = self.state.tasks.get_mut(run_id)
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

        // Mark the corresponding AgentRun step.
        if let Some(run) = self.state.runs.get_mut(run_id) {
            if let Some(step) = run.steps.iter_mut().find(|s| {
                s.kind == polyphony_core::StepKind::AgentRun
                    && s.task_id.as_deref() == Some(task_id)
            }) {
                if matches!(outcome.status, AttemptStatus::Succeeded) {
                    step.mark_succeeded();
                } else {
                    step.mark_failed(outcome.error.as_deref().unwrap_or("task failed"));
                }
            }
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }

        if matches!(outcome.status, AttemptStatus::Succeeded) {
            self.dispatch_next_task(
                self.workflow(),
                issue.clone(),
                attempt,
                false,
                run_id,
                workspace_path,
            )
            .await
        } else {
            let max_replan_attempts = 2;
            if workflow.config.pipeline.replan_on_failure
                && workflow.config.router_agent_name().is_some()
                && attempt.unwrap_or(0) < max_replan_attempts
            {
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "{} task failed, re-running planner (attempt {})",
                        issue.identifier,
                        attempt.unwrap_or(0) + 1
                    ),
                );
                // Reset tasks and re-plan
                if let Some(tasks) = self.state.tasks.get_mut(run_id) {
                    for task in tasks.iter_mut() {
                        if task.status == TaskStatus::Pending {
                            task.status = TaskStatus::Cancelled;
                            task.updated_at = Utc::now();
                        }
                    }
                }
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.status = RunStatus::Planning;
                    run.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                return self
                    .dispatch_planner_task(workflow, issue, attempt, false, run_id, workspace_path)
                    .await;
            }
            // Mark run as failed
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            Ok(())
        }
    }

    pub(crate) async fn complete_pipeline(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        run_id: &str,
    ) -> Result<(), Error> {
        let status = if workflow.config.automation.enabled {
            RunStatus::Review
        } else {
            RunStatus::Delivered
        };
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            automation_enabled = workflow.config.automation.enabled,
            run_status = ?status,
            "pipeline completed"
        );
        if let Some(run) = self.state.runs.get_mut(run_id) {
            run.status = status;
            run.pipeline_stage = Some(PipelineStage::Completing);
            run.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!("executing → completing ({status})"),
            );
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }
        self.push_event(
            EventScope::Dispatch,
            format!("{} pipeline completed ({:?})", issue.identifier, status),
        );
        Ok(())
    }

    pub(crate) async fn inject_feedback_task(
        &mut self,
        request: &FeedbackInjectionRequest,
    ) -> Result<(), Error> {
        let run_id = &request.run_id;
        let Some(run) = self.state.runs.get(run_id).cloned() else {
            return Err(Error::Core(CoreError::Adapter(format!(
                "run {run_id} not found"
            ))));
        };
        let Some(issue_id) = &run.issue_id else {
            return Err(Error::Core(CoreError::Adapter(format!(
                "run {run_id} has no associated issue"
            ))));
        };

        // Resolve the issue from tracker_issues
        let issue = self
            .state
            .tracker_issues
            .iter()
            .find(|row| row.issue_id == *issue_id)
            .map(|row| Issue {
                id: row.issue_id.clone(),
                identifier: row.issue_identifier.clone(),
                title: row.title.clone(),
                description: row.description.clone(),
                priority: row.priority,
                state: row.state.clone(),
                labels: row.labels.clone(),
                branch_name: None,
                url: row.url.clone(),
                author: None,
                created_at: row.created_at,
                updated_at: row.updated_at,
                parent_id: row.parent_id.clone(),
                comments: vec![],
                blocked_by: vec![],
                approval_state: row.approval_state,
            })
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "issue {issue_id} not found for run {run_id}"
                )))
            })?;

        // Compute next ordinal
        let next_ordinal = self
            .state
            .tasks
            .get(run_id)
            .map(|tasks| tasks.iter().map(|t| t.ordinal).max().unwrap_or(0) + 1)
            .unwrap_or(1);

        // Create the feedback task
        let now = Utc::now();
        let feedback_title = request
            .prompt
            .lines()
            .next()
            .unwrap_or("User feedback")
            .chars()
            .take(80)
            .collect::<String>();
        let task = Task {
            id: format!("task-{}", uuid::Uuid::new_v4()),
            run_id: run_id.clone(),
            title: feedback_title.clone(),
            description: Some(request.prompt.clone()),
            activity_log: Vec::new(),
            category: TaskCategory::Feedback,
            status: TaskStatus::Pending,
            ordinal: next_ordinal,
            parent_id: None,
            agent_name: request.agent_name.clone(),
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

        // Persist the task
        if let Some(store) = &self.store {
            store.save_task(&task).await?;
        }
        self.state
            .tasks
            .entry(run_id.clone())
            .or_default()
            .push(task.clone());

        // Add an AgentRun step for this task
        let step_ordinal = run.steps.last().map(|s| s.ordinal + 1).unwrap_or(0);
        let step =
            polyphony_core::StepRecord::new(polyphony_core::StepKind::AgentRun, step_ordinal)
                .with_task_id(task.id.clone());

        if let Some(m) = self.state.runs.get_mut(run_id) {
            m.steps.push(step);
            // Reset run status if delivered/failed so the pipeline resumes
            if matches!(m.status, RunStatus::Delivered | RunStatus::Failed) {
                m.status = RunStatus::InProgress;
                m.pipeline_stage = Some(PipelineStage::Executing);
            }
            // Store user feedback as manual_dispatch_directives for prompt injection
            m.manual_dispatch_directives = Some(request.prompt.clone());
            m.updated_at = now;
            m.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!("feedback injected: {feedback_title}"),
            );
            if let Some(store) = &self.store {
                store.save_run(m).await?;
            }
        }

        self.push_event(
            EventScope::Dispatch,
            format!(
                "{} feedback task injected: {feedback_title}",
                issue.identifier
            ),
        );

        // Dispatch the next pending task (which will be this feedback task)
        let workflow = self.workflow();
        let workspace_path = run
            .workspace_path
            .as_deref()
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!("run {run_id} has no workspace")))
            })?
            .to_path_buf();

        self.dispatch_next_task(workflow, issue, None, false, run_id, &workspace_path)
            .await?;

        Ok(())
    }
}
