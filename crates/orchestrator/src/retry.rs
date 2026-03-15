use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn process_due_retries(&mut self) {
        if self.workflow_reload_error().is_some() {
            return;
        }
        if self.workflow().config.validate().is_err() {
            return;
        }
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

    pub(crate) async fn handle_retry(&mut self, issue_id: String) {
        let Some(retry) = self.state.retrying.remove(&issue_id) else {
            return;
        };
        let workflow = self.workflow();
        if !workflow.config.has_dispatch_agents() {
            return;
        }
        if let Some(trigger) = self
            .state
            .pull_request_retry_triggers
            .get(&issue_id)
            .cloned()
        {
            if !self.has_available_slot(&workflow, "review") {
                self.schedule_retry(
                    issue_id,
                    retry.row.issue_identifier,
                    retry.row.attempt + 1,
                    Some("no available orchestrator slots".into()),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
                return;
            }
            if let Err(error) = self
                .dispatch_pull_request_trigger(workflow.clone(), trigger, Some(retry.row.attempt))
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
            return;
        }
        let query = workflow.config.tracker_query();
        let issues = match self.tracker.fetch_candidate_issues(&query).await {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                return;
            },
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
            },
        };
        let Some(issue) = issues
            .into_iter()
            .find(|issue| issue.id == retry.row.issue_id)
        else {
            self.release_issue(&issue_id);
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
        let skip_workspace_sync = should_skip_workspace_sync_for_retry(retry.row.error.as_deref());
        if skip_workspace_sync {
            info!(
                issue_identifier = %issue.identifier,
                attempt = retry.row.attempt,
                "retrying issue without workspace sync after provider rate limit"
            );
        }
        if let Err(error) = self
            .dispatch_issue(
                workflow.clone(),
                issue,
                Some(retry.row.attempt),
                retry.row.error.is_some(),
                None,
                skip_workspace_sync,
            )
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

    pub(crate) async fn handle_message(
        &mut self,
        message: OrchestratorMessage,
    ) -> Result<(), Error> {
        match message {
            OrchestratorMessage::AgentEvent(event) => {
                let mut running_model = None;
                if let Some(running) = self.state.running.get_mut(&event.issue_id) {
                    running.session_id = event
                        .session_id
                        .clone()
                        .or_else(|| running.session_id.clone());
                    running.thread_id = event
                        .thread_id
                        .clone()
                        .or_else(|| running.thread_id.clone());
                    running.turn_id = event.turn_id.clone().or_else(|| running.turn_id.clone());
                    running.codex_app_server_pid = event
                        .codex_app_server_pid
                        .clone()
                        .or_else(|| running.codex_app_server_pid.clone());
                    running.last_event = Some(format!("{:?}", event.kind));
                    running.last_message = event.message.clone();
                    running.last_event_at = Some(event.at);
                    if matches!(event.kind, AgentEventKind::TurnStarted) {
                        running.turn_count += 1;
                    }
                    if let Some(usage) = event.usage.clone() {
                        apply_usage_delta(&mut self.state.totals, running, usage);
                    }
                    if let Some(rate_limits) = event.rate_limits.clone() {
                        running.rate_limits = Some(rate_limits.clone());
                        self.state.rate_limits = Some(rate_limits);
                    }
                    running_model = running.model.clone();
                }
                self.update_saved_context_from_event(&event, running_model);
                self.push_event(
                    EventScope::Agent,
                    format!(
                        "{} {}",
                        event.issue_identifier,
                        event.message.unwrap_or_else(|| format!("{:?}", event.kind))
                    ),
                );
                self.emit_snapshot().await?;
            },
            OrchestratorMessage::RateLimited(signal) => {
                self.register_throttle(signal);
                self.emit_snapshot().await?;
            },
            OrchestratorMessage::WorkerFinished {
                issue_id,
                issue_identifier,
                attempt,
                started_at,
                outcome,
            } => {
                self.finish_running(issue_id, issue_identifier, attempt, started_at, outcome)
                    .await?;
            },
        }
        Ok(())
    }
}
