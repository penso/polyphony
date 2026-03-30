use polyphony_core::StepKind;

use crate::{prelude::*, *};

impl RuntimeService {
    // -----------------------------------------------------------------------
    // Step tracking helpers
    // -----------------------------------------------------------------------

    async fn mark_step_running(&mut self, run_id: &Option<String>, kind: StepKind) {
        let Some(id) = run_id.as_deref() else {
            return;
        };
        if let Some(run) = self.state.runs.get_mut(id)
            && let Some(step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == kind && !s.is_complete())
        {
            step.mark_running();
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
    }

    async fn complete_step(&mut self, run_id: &Option<String>, kind: StepKind) {
        let Some(id) = run_id.as_deref() else {
            return;
        };
        if let Some(run) = self.state.runs.get_mut(id)
            && let Some(step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == kind && s.status == polyphony_core::StepStatus::Running)
        {
            step.mark_succeeded();
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
    }

    async fn fail_step(&mut self, run_id: &Option<String>, kind: StepKind, error: &str) {
        let Some(id) = run_id.as_deref() else {
            return;
        };
        if let Some(run) = self.state.runs.get_mut(id)
            && let Some(step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == kind && s.status == polyphony_core::StepStatus::Running)
        {
            step.mark_failed(error);
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
    }

    async fn skip_step(&mut self, run_id: &Option<String>, kind: StepKind) {
        let Some(id) = run_id.as_deref() else {
            return;
        };
        if let Some(run) = self.state.runs.get_mut(id)
            && let Some(step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == kind && !s.is_complete())
        {
            step.mark_skipped();
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
    }

    /// Skip all remaining handoff steps (Commit, Push, CreatePR, etc.)
    /// when the workspace is clean.
    async fn skip_handoff_steps(&mut self, run_id: &Option<String>) {
        let Some(id) = run_id.as_deref() else {
            return;
        };
        let handoff_kinds = [
            StepKind::Commit,
            StepKind::Push,
            StepKind::CreatePullRequest,
            StepKind::ReviewPass,
            StepKind::PostReviewComment,
            StepKind::SendFeedback,
            StepKind::AfterOutcomeHooks,
        ];
        if let Some(run) = self.state.runs.get_mut(id) {
            for step in &mut run.steps {
                if handoff_kinds.contains(&step.kind) && !step.is_complete() {
                    step.mark_skipped();
                }
            }
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
    }

    // -----------------------------------------------------------------------

    pub(crate) fn update_saved_context_from_event(
        &mut self,
        event: &AgentEvent,
        model: Option<String>,
    ) {
        let repo_id = self.repo_id_for_issue(&event.issue_id);
        let context = self
            .state
            .saved_contexts
            .entry(event.issue_id.clone())
            .or_insert_with(|| AgentContextSnapshot {
                repo_id: repo_id.clone(),
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
        context.repo_id = repo_id;
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
        let repo_id = self.repo_id_for_issue(issue_id);
        let context = self
            .state
            .saved_contexts
            .entry(issue_id.to_string())
            .or_insert_with(|| AgentContextSnapshot {
                repo_id: repo_id.clone(),
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
        context.repo_id = repo_id;
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
            self.create_local_branch_deliverable(running).await;
            return Ok(());
        }
        info!(
            issue_identifier = %running.issue.identifier,
            workspace_path = %running.workspace_path.display(),
            agent = %running.agent_name,
            "starting automated handoff"
        );
        let committer = self
            .committer_for_issue(&running.issue.id)
            .ok_or_else(|| CoreError::Adapter("workspace committer is not configured".into()))?;
        let pull_request_manager = self
            .pull_request_manager_for_issue(&running.issue.id)
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
        let mut commit_message = render_issue_template_with_strings(
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
        if workflow.config.automation.co_authored_by {
            commit_message.push_str("\n\nCo-Authored-By: Polyphony <noreply@polyphony.to>");
        }
        // --- Commit + Push step ---
        let run_id = running.run_id.clone();
        self.mark_step_running(&run_id, polyphony_core::StepKind::Commit)
            .await;
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
            .await;
        let commit_result = match commit_result {
            Ok(result) => result,
            Err(error) => {
                self.fail_step(
                    &run_id,
                    polyphony_core::StepKind::Commit,
                    &error.to_string(),
                )
                .await;
                return Err(Error::Core(error));
            },
        };
        let Some(commit_result) = commit_result else {
            // Workspace is clean — skip commit/push/PR steps.
            self.skip_handoff_steps(&run_id).await;
            self.push_event(
                EventScope::Handoff,
                format!(
                    "{} handoff skipped because the workspace is clean",
                    running.issue.identifier
                ),
            );
            return Ok(());
        };
        self.complete_step(&run_id, polyphony_core::StepKind::Commit)
            .await;
        // Push is bundled with commit_and_push — auto-complete the Push step.
        self.mark_step_running(&run_id, polyphony_core::StepKind::Push)
            .await;
        self.complete_step(&run_id, polyphony_core::StepKind::Push)
            .await;
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
        // --- CreatePullRequest step ---
        self.mark_step_running(&run_id, polyphony_core::StepKind::CreatePullRequest)
            .await;
        let pr_title_copy = pr_title.clone();
        let pr_body_copy = pr_body.clone();
        let pull_request = match pull_request_manager
            .ensure_pull_request(&PullRequestRequest {
                repository,
                head_branch: branch_name.clone(),
                base_branch: base_branch.clone(),
                title: pr_title,
                body: pr_body,
                draft: workflow.config.automation.draft_pull_requests,
            })
            .await
        {
            Ok(pr) => pr,
            Err(error) => {
                self.fail_step(
                    &run_id,
                    polyphony_core::StepKind::CreatePullRequest,
                    &error.to_string(),
                )
                .await;
                return Err(Error::Core(error));
            },
        };
        self.complete_step(&run_id, polyphony_core::StepKind::CreatePullRequest)
            .await;
        info!(
            issue_identifier = %running.issue.identifier,
            pull_request_number = pull_request.number,
            pull_request_url = pull_request.url.as_deref().unwrap_or(""),
            "pull request ensured for handoff"
        );
        if let Some(run_id) = running.run_id.as_ref()
            && let Some(run) = self.state.runs.get_mut(run_id)
        {
            run.status = RunStatus::Delivered;
            let mut metadata = std::collections::HashMap::new();
            metadata.insert(
                "changed_files".into(),
                serde_json::Value::Number(commit_result.changed_files.into()),
            );
            if let Some(added) = commit_result.lines_added {
                metadata.insert(
                    "lines_added".into(),
                    serde_json::Value::Number(added.into()),
                );
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
            run.deliverable = Some(polyphony_core::Deliverable {
                kind: polyphony_core::DeliverableKind::GithubPullRequest,
                status: polyphony_core::DeliverableStatus::Open,
                url: pull_request.url.clone(),
                decision: polyphony_core::DeliverableDecision::Waiting,
                title: Some(pr_title_copy),
                description: Some(pr_body_copy),
                metadata,
            });
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }

        // --- ReviewPass + PostReviewComment steps (best-effort) ---
        self.mark_step_running(&run_id, polyphony_core::StepKind::ReviewPass)
            .await;
        let review_body = self
            .run_review_pass(workflow, running, &pull_request)
            .await?;
        if let Some(review_body) = review_body {
            self.complete_step(&run_id, polyphony_core::StepKind::ReviewPass)
                .await;
            self.mark_step_running(&run_id, polyphony_core::StepKind::PostReviewComment)
                .await;
            if let Some(commenter) = self.pull_request_commenter_for_issue(&running.issue.id) {
                let _ = commenter
                    .comment_on_pull_request(&pull_request, &review_body)
                    .await;
            }
            self.complete_step(&run_id, polyphony_core::StepKind::PostReviewComment)
                .await;
        } else {
            self.skip_step(&run_id, polyphony_core::StepKind::ReviewPass)
                .await;
            self.skip_step(&run_id, polyphony_core::StepKind::PostReviewComment)
                .await;
        }

        // --- SendFeedback step (best-effort) ---
        self.mark_step_running(&run_id, polyphony_core::StepKind::SendFeedback)
            .await;
        self.send_handoff_feedback(workflow, running, &pull_request, &commit_result)
            .await;
        self.complete_step(&run_id, polyphony_core::StepKind::SendFeedback)
            .await;

        self.push_event(
            EventScope::Handoff,
            format!(
                "{} opened PR #{} on {}",
                running.issue.identifier, pull_request.number, commit_result.branch_name
            ),
        );

        // --- AfterOutcomeHooks step ---
        self.mark_step_running(&run_id, polyphony_core::StepKind::AfterOutcomeHooks)
            .await;
        let manager = self.build_workspace_manager(workflow);
        manager
            .run_after_outcome_best_effort(&workflow.config.hooks, &running.workspace_path)
            .await;
        self.complete_step(&run_id, polyphony_core::StepKind::AfterOutcomeHooks)
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
            .agent_for_issue(&running.issue.id)
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
        let Some(feedback) = self.feedback_for_issue(&running.issue.id) else {
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

    /// Create a local branch deliverable for runs without GitHub automation.
    async fn create_local_branch_deliverable(&mut self, running: &RunningTask) {
        let Some(run_id) = running.run_id.clone() else {
            return;
        };

        // Check if the run exists and has no deliverable yet
        let has_deliverable = self
            .state
            .runs
            .get(&run_id)
            .is_some_and(|m| m.deliverable.is_some());
        if has_deliverable {
            return;
        }
        let run_exists = self.state.runs.contains_key(&run_id);
        if !run_exists {
            return;
        }

        // Detect the workspace branch and diff stats
        let workspace_path = &running.workspace_path;
        let (branch_name, diff_stats) = match detect_branch_info(workspace_path) {
            Ok(info) => info,
            Err(error) => {
                warn!(%error, "failed to detect branch info for local deliverable");
                return;
            },
        };

        let mut metadata = std::collections::HashMap::new();
        if let Some((added, removed, changed)) = diff_stats {
            metadata.insert("lines_added".into(), serde_json::json!(added));
            metadata.insert("lines_removed".into(), serde_json::json!(removed));
            metadata.insert("changed_files".into(), serde_json::json!(changed));
        }
        metadata.insert("branch".into(), serde_json::json!(branch_name));
        metadata.insert(
            "workspace_path".into(),
            serde_json::json!(workspace_path.display().to_string()),
        );

        let deliverable = polyphony_core::Deliverable {
            kind: polyphony_core::DeliverableKind::LocalBranch,
            status: polyphony_core::DeliverableStatus::Open,
            url: None,
            decision: polyphony_core::DeliverableDecision::Waiting,
            title: Some(format!("Branch: {branch_name}")),
            description: None,
            metadata,
        };

        if let Some(run) = self.state.runs.get_mut(&run_id) {
            run.deliverable = Some(deliverable);
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
        self.push_event(
            EventScope::Handoff,
            format!("{run_id} local branch deliverable created: {branch_name}"),
        );
    }

    /// Create a local branch deliverable from a workspace path, without needing a RunningTask.
    /// Used on restart when the pipeline completed but the deliverable wasn't created.
    pub(crate) async fn create_local_branch_deliverable_from_workspace(
        &mut self,
        run_id: &str,
        workspace_path: &Path,
    ) {
        let has_deliverable = self
            .state
            .runs
            .get(run_id)
            .is_some_and(|m| m.deliverable.is_some());
        if has_deliverable {
            return;
        }

        let (branch_name, diff_stats) = match detect_branch_info(workspace_path) {
            Ok(info) => info,
            Err(error) => {
                warn!(%error, "failed to detect branch info for local deliverable on resume");
                return;
            },
        };

        let mut metadata = std::collections::HashMap::new();
        if let Some((added, removed, changed)) = diff_stats {
            metadata.insert("lines_added".into(), serde_json::json!(added));
            metadata.insert("lines_removed".into(), serde_json::json!(removed));
            metadata.insert("changed_files".into(), serde_json::json!(changed));
        }
        metadata.insert("branch".into(), serde_json::json!(branch_name));
        metadata.insert(
            "workspace_path".into(),
            serde_json::json!(workspace_path.display().to_string()),
        );

        let deliverable = polyphony_core::Deliverable {
            kind: polyphony_core::DeliverableKind::LocalBranch,
            status: polyphony_core::DeliverableStatus::Open,
            url: None,
            decision: polyphony_core::DeliverableDecision::Waiting,
            title: Some(format!("Branch: {branch_name}")),
            description: None,
            metadata,
        };

        if let Some(run) = self.state.runs.get_mut(run_id) {
            run.deliverable = Some(deliverable);
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }
        self.push_event(
            EventScope::Handoff,
            format!("{run_id} local branch deliverable created on resume: {branch_name}"),
        );
    }

    pub(crate) async fn finalize_accepted_run(&mut self, run_id: &str) {
        self.close_run_issue(run_id).await;
        self.cleanup_run_workspace(run_id).await;
    }

    async fn close_run_issue(&mut self, run_id: &str) {
        let Some((issue_id, run_label, terminal_state)) =
            self.state.runs.get(run_id).and_then(|run| {
                run.issue_id.as_ref().map(|issue_id| {
                    (
                        issue_id.clone(),
                        Self::run_target_label(run),
                        self.workflow()
                            .config
                            .tracker
                            .terminal_states
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "closed".into()),
                    )
                })
            })
        else {
            return;
        };

        let request = polyphony_core::UpdateIssueRequest {
            id: issue_id.clone(),
            state: Some(terminal_state.clone()),
            ..Default::default()
        };
        match self
            .tracker_for_issue(&issue_id)
            .update_issue(&request)
            .await
        {
            Ok(_) => {
                self.push_event(
                    EventScope::Handoff,
                    format!("{run_label} issue marked {terminal_state}"),
                );
            },
            Err(error) => {
                warn!(%error, issue_id, "failed to close issue after merge");
                self.push_event(
                    EventScope::Handoff,
                    format!("{run_label} merged but failed to close issue: {error}"),
                );
            },
        }
    }

    async fn cleanup_run_workspace(&mut self, run_id: &str) {
        let Some((issue_identifier, branch_name, workspace_key, run_label)) =
            self.state.runs.get(run_id).and_then(|run| {
                run.issue_identifier.as_ref().map(|issue_identifier| {
                    let branch_name = run.deliverable.as_ref().and_then(|deliverable| {
                        deliverable
                            .metadata
                            .get("branch")
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                    });
                    (
                        issue_identifier.clone(),
                        branch_name,
                        run.workspace_key
                            .clone()
                            .unwrap_or_else(|| sanitize_workspace_key(issue_identifier)),
                        Self::run_target_label(run),
                    )
                })
            })
        else {
            return;
        };

        let workflow = self.workflow();
        let manager = self.build_workspace_manager(&workflow);
        match manager
            .cleanup_workspace(&issue_identifier, branch_name, &workflow.config.hooks)
            .await
        {
            Ok(()) => {
                self.state.worktree_keys.remove(&workspace_key);
                self.push_event(
                    EventScope::Handoff,
                    format!("{run_label} workspace cleaned up"),
                );
            },
            Err(error) => {
                warn!(%error, issue_identifier, "failed to clean up workspace after merge");
                self.push_event(
                    EventScope::Handoff,
                    format!("{run_label} merged but failed to clean up workspace: {error}"),
                );
            },
        }
    }

    /// Merge a deliverable — either a GitHub PR or a local branch.
    pub(crate) async fn merge_deliverable(&mut self, run_id: &str) {
        // Extract info needed for the merge before borrowing mutably.
        let merge_info = {
            let Some(run) = self.state.runs.get(run_id) else {
                self.push_event(
                    EventScope::Handoff,
                    format!("merge failed: run {run_id} not found"),
                );
                return;
            };
            let run_label = Self::run_target_label(run);
            let Some(deliverable) = &run.deliverable else {
                self.push_event(
                    EventScope::Handoff,
                    format!("{run_label} merge failed: no deliverable"),
                );
                return;
            };
            (
                deliverable.status == polyphony_core::DeliverableStatus::Merged,
                deliverable.kind,
                deliverable
                    .metadata
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                deliverable
                    .metadata
                    .get("workspace_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                deliverable.url.clone().unwrap_or_default(),
                run_label,
            )
        };
        let (already_merged, kind, branch, workspace, url, run_label) = merge_info;

        if already_merged {
            self.push_event(EventScope::Handoff, format!("{run_label} already merged"));
            self.finalize_accepted_run(run_id).await;
            return;
        }

        let result = match kind {
            polyphony_core::DeliverableKind::LocalBranch => {
                merge_local_branch(&workspace, &branch).await
            },
            polyphony_core::DeliverableKind::GithubPullRequest => {
                if let Some(pr_manager) = self
                    .state
                    .runs
                    .get(run_id)
                    .and_then(|run| run.issue_id.as_deref())
                    .and_then(|issue_id| self.pull_request_manager_for_issue(issue_id))
                {
                    merge_github_pr(pr_manager.as_ref(), &url).await
                } else {
                    polyphony_core::MergeResult {
                        success: false,
                        message: "pull request manager not configured".into(),
                        merged_sha: None,
                    }
                }
            },
            other => polyphony_core::MergeResult {
                success: false,
                message: format!("merge not supported for {other} deliverables"),
                merged_sha: None,
            },
        };

        // Now mutate the run with the result
        if let Some(run) = self.state.runs.get_mut(run_id) {
            if result.success {
                if let Some(deliverable) = run.deliverable.as_mut() {
                    deliverable.status = polyphony_core::DeliverableStatus::Merged;
                    deliverable.decision = polyphony_core::DeliverableDecision::Accepted;
                }
                run.status = polyphony_core::RunStatus::Delivered;
                run.updated_at = Utc::now();
            }
            if let Some(store) = &self.store {
                let _ = store.save_run(run).await;
            }
        }

        if result.success {
            self.push_event(
                EventScope::Handoff,
                format!("{run_label} merged: {}", result.message),
            );
            self.finalize_accepted_run(run_id).await;
        } else {
            self.push_event(
                EventScope::Handoff,
                format!("{run_label} merge failed: {}", result.message),
            );
        }
    }
}

/// Detect the current branch name and diff stats against the default branch.
fn detect_branch_info(
    workspace_path: &Path,
) -> Result<(String, Option<(usize, usize, usize)>), CoreError> {
    use std::process::Command;

    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace_path)
        .output()
        .map_err(|e| CoreError::Adapter(format!("git rev-parse failed: {e}")))?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        return Err(CoreError::Adapter("could not determine branch name".into()));
    }

    // Try to get diff stats against the merge base with main/master.
    // Check both committed changes AND uncommitted working tree changes.
    let default_branch = find_default_branch(workspace_path);
    let committed_stats = if let Some(base) = &default_branch {
        let merge_base = Command::new("git")
            .args(["merge-base", base, "HEAD"])
            .current_dir(workspace_path)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

        if let Some(merge_base_sha) = merge_base {
            let stat_output = Command::new("git")
                .args(["diff", "--stat", &merge_base_sha, "HEAD"])
                .current_dir(workspace_path)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

            stat_output.and_then(|stat| parse_diff_stat_summary(&stat))
        } else {
            None
        }
    } else {
        None
    };

    // If no committed changes, check for uncommitted working tree changes.
    // Agents like Codex may modify files without committing.
    let diff_stats = if committed_stats.is_some() {
        committed_stats
    } else {
        let working_tree = Command::new("git")
            .args(["diff", "--stat", "HEAD"])
            .current_dir(workspace_path)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

        working_tree.and_then(|stat| parse_diff_stat_summary(&stat))
    };

    Ok((branch, diff_stats))
}

fn find_default_branch(workspace_path: &Path) -> Option<String> {
    use std::process::Command;
    for candidate in &["main", "master"] {
        let status = Command::new("git")
            .args(["rev-parse", "--verify", candidate])
            .current_dir(workspace_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if status.success() {
            return Some((*candidate).to_string());
        }
    }
    None
}

fn parse_diff_stat_summary(stat: &str) -> Option<(usize, usize, usize)> {
    // Last line looks like: " 5 files changed, 120 insertions(+), 30 deletions(-)"
    let last_line = stat.lines().last()?;
    let mut files = 0;
    let mut insertions = 0;
    let mut deletions = 0;
    for part in last_line.split(',') {
        let part = part.trim();
        if part.contains("file") {
            files = part.split_whitespace().next()?.parse().ok()?;
        } else if part.contains("insertion") {
            insertions = part.split_whitespace().next()?.parse().ok()?;
        } else if part.contains("deletion") {
            deletions = part.split_whitespace().next()?.parse().ok()?;
        }
    }
    Some((insertions, deletions, files))
}

/// Merge a local branch into the default branch (main/master).
///
/// Linked worktrees cannot `git checkout main` because main is already checked
/// out in the parent repo. Instead, we resolve the **main repository path**
/// (where the default branch lives) and run the merge there.
async fn merge_local_branch(workspace_path: &str, branch: &str) -> polyphony_core::MergeResult {
    use tokio::process::Command;

    let workspace = PathBuf::from(workspace_path);
    if !workspace.exists() {
        return polyphony_core::MergeResult {
            success: false,
            message: format!("workspace path does not exist: {workspace_path}"),
            merged_sha: None,
        };
    }

    // Find the main repository path. In a linked worktree, `git rev-parse
    // --show-toplevel` returns the worktree root, but the default branch is
    // checked out in the main repo. Use `git worktree list --porcelain` to
    // find where `main`/`master` lives, falling back to the common git dir.
    let main_repo = resolve_main_repo_path(&workspace)
        .await
        .unwrap_or_else(|| workspace.clone());

    // Find the default branch
    let Some(default_branch) = find_default_branch(&main_repo) else {
        return polyphony_core::MergeResult {
            success: false,
            message: "cannot find main or master branch".into(),
            merged_sha: None,
        };
    };

    // Merge the branch from the main repo directory (where the default branch
    // is checked out). This avoids the "already checked out" error in worktrees.
    let merge = Command::new("git")
        .args([
            "merge",
            "--no-ff",
            branch,
            "-m",
            &format!("Merge branch '{branch}'"),
        ])
        .current_dir(&main_repo)
        .output()
        .await;

    match merge {
        Ok(output) if output.status.success() => {
            let sha = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&main_repo)
                .output()
                .await
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

            polyphony_core::MergeResult {
                success: true,
                message: format!("merged {branch} into {default_branch}"),
                merged_sha: sha,
            }
        },
        Ok(output) => {
            // Abort a failed merge so the repo isn't left in a dirty state
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&main_repo)
                .output()
                .await;
            let stderr = String::from_utf8_lossy(&output.stderr);
            polyphony_core::MergeResult {
                success: false,
                message: format!("merge failed: {stderr}"),
                merged_sha: None,
            }
        },
        Err(error) => polyphony_core::MergeResult {
            success: false,
            message: format!("git merge failed to execute: {error}"),
            merged_sha: None,
        },
    }
}

/// Resolve the path to the main repository (where the default branch is
/// checked out). For linked worktrees this is the parent repo, for regular
/// repos this returns the repo path itself.
async fn resolve_main_repo_path(workspace_path: &Path) -> Option<PathBuf> {
    use tokio::process::Command;

    // `git rev-parse --git-common-dir` gives us the shared .git directory.
    // From that we can derive the main repo toplevel.
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(workspace_path)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let common_path = if PathBuf::from(&common_dir).is_absolute() {
        PathBuf::from(&common_dir)
    } else {
        workspace_path.join(&common_dir)
    };
    // The common dir is typically `.git` — its parent is the repo root.
    // For bare repos or unusual layouts it might differ, but for standard
    // repos (which polyphony uses) this is reliable.
    common_path.parent().map(|p| p.to_path_buf())
}

/// Merge a GitHub pull request via the PR manager.
async fn merge_github_pr(
    _pr_manager: &dyn polyphony_core::PullRequestManager,
    url: &str,
) -> polyphony_core::MergeResult {
    // Extract PR number from URL (e.g. https://github.com/owner/repo/pull/123)
    let number = url.rsplit('/').next().and_then(|s| s.parse::<u64>().ok());

    let Some(_number) = number else {
        return polyphony_core::MergeResult {
            success: false,
            message: format!("cannot extract PR number from URL: {url}"),
            merged_sha: None,
        };
    };

    // TODO: Implement GitHub PR merge via pr_manager when the trait supports it.
    // For now, return an error directing the user to merge via GitHub.
    polyphony_core::MergeResult {
        success: false,
        message: format!("GitHub PR merge not yet implemented — merge via GitHub: {url}"),
        merged_sha: None,
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

        // Debounce: coalesce rapid filesystem events (Linux inotify can fire
        // multiple events for a single write) into one Refresh command.
        const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
        loop {
            // Wait for the first event.
            let event = notify_rx.recv().await;
            let Some(event) = event else {
                break;
            };
            match event {
                Ok(_) => {},
                Err(error) => {
                    warn!(%error, "workflow watch event failed");
                    continue;
                },
            }
            // Drain any additional events that arrive within the debounce window.
            let deadline = tokio::time::sleep(DEBOUNCE);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    biased;
                    next = notify_rx.recv() => {
                        match next {
                            Some(Ok(_)) => {},      // absorb, keep waiting
                            Some(Err(error)) => warn!(%error, "workflow watch event failed"),
                            None => return Ok(()),  // channel closed
                        }
                    }
                    _ = &mut deadline => break,
                }
            }
            info!(path = %workflow_path.display(), "workflow change detected");
            let _ = runtime_command_tx.send(RuntimeCommand::Refresh);
        }
        Ok(())
    }))
}
