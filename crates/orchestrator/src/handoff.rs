use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) fn update_saved_context_from_event(
        &mut self,
        event: &AgentEvent,
        model: Option<String>,
    ) {
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
            compact_saved_context_in_place(context);
        }
    }

    pub(crate) fn finalize_saved_context(
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
        compact_saved_context_in_place(context);
    }

    pub(crate) async fn run_success_handoff(
        &mut self,
        workflow: &LoadedWorkflow,
        running: &RunningTask,
    ) -> Result<(), Error> {
        if !workflow.config.automation.enabled {
            return Ok(());
        }
        info!(
            issue_identifier = %running.issue.identifier,
            workspace_path = %running.workspace_path.display(),
            agent = %running.agent_name,
            "starting automated handoff"
        );
        let committer = self
            .committer
            .clone()
            .ok_or_else(|| CoreError::Adapter("workspace committer is not configured".into()))?;
        let pull_request_manager = self
            .pull_request_manager
            .clone()
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
                base_branch: Some(base_branch.clone()),
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
        info!(
            issue_identifier = %running.issue.identifier,
            branch_name = %commit_result.branch_name,
            head_sha = %commit_result.head_sha,
            changed_files = commit_result.changed_files,
            "workspace changes committed and pushed"
        );
        self.push_event(
            EventScope::Handoff,
            format!(
                "{} pushed {} on {}",
                running.issue.identifier, commit_result.head_sha, commit_result.branch_name
            ),
        );

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
        let pr_title_copy = pr_title.clone();
        let pr_body_copy = pr_body.clone();
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
        info!(
            issue_identifier = %running.issue.identifier,
            pull_request_number = pull_request.number,
            pull_request_url = pull_request.url.as_deref().unwrap_or(""),
            "pull request ensured for handoff"
        );
        if let Some(movement_id) = running.movement_id.as_ref()
            && let Some(movement) = self.state.movements.get_mut(movement_id)
        {
            movement.status = MovementStatus::Delivered;
            let mut metadata = std::collections::HashMap::new();
            metadata.insert(
                "changed_files".into(),
                serde_json::Value::Number(commit_result.changed_files.into()),
            );
            if let Some(added) = commit_result.lines_added {
                metadata
                    .insert("lines_added".into(), serde_json::Value::Number(added.into()));
            }
            if let Some(removed) = commit_result.lines_removed {
                metadata.insert(
                    "lines_removed".into(),
                    serde_json::Value::Number(removed.into()),
                );
            }
            metadata.insert(
                "head_sha".into(),
                serde_json::Value::String(commit_result.head_sha.clone()),
            );
            movement.deliverable = Some(polyphony_core::Deliverable {
                kind: polyphony_core::DeliverableKind::GithubPullRequest,
                status: polyphony_core::DeliverableStatus::Open,
                url: pull_request.url.clone(),
                decision: polyphony_core::DeliverableDecision::Waiting,
                title: Some(pr_title_copy),
                description: Some(pr_body_copy),
                metadata,
            });
            movement.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_movement(movement).await?;
            }
        }

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
        let manager = self.build_workspace_manager(workflow);
        manager
            .run_after_outcome_best_effort(&workflow.config.hooks, &running.workspace_path)
            .await;
        Ok(())
    }

    pub(crate) async fn run_review_pass(
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
        info!(
            issue_identifier = %running.issue.identifier,
            review_agent = %review_agent.name,
            pull_request_number = pull_request.number,
            "starting automated PR review pass"
        );
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
        let prompt = apply_agent_prompt_template(
            workflow,
            &review_agent.name,
            prompt,
            &running.issue,
            running.attempt,
            1,
            workflow.config.agent.max_turns,
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
                info!(
                    issue_identifier = %running.issue.identifier,
                    pull_request_number = pull_request.number,
                    review_generated = review.as_ref().is_some_and(|body| !body.trim().is_empty()),
                    "automated PR review pass completed"
                );
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

    pub(crate) async fn send_handoff_feedback(
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
    user_config_path: Option<PathBuf>,
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
        if let Ok(agent_dirs) = agent_prompt_dirs(&workflow_path, user_config_path.as_deref()) {
            for dir in agent_dirs {
                if dir.exists() {
                    watcher.watch(&dir, RecursiveMode::Recursive)?;
                }
            }
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
