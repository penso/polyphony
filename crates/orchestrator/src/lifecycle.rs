use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn startup_cleanup(&mut self) {
        let workflow = self.workflow();
        let terminal = workflow.config.tracker.terminal_states.clone();
        let issues = match self
            .tracker
            .fetch_issues_by_states(workflow.config.tracker.project_slug.as_deref(), &terminal)
            .await
        {
            Ok(issues) => issues,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                return;
            },
            Err(error) => {
                warn!(%error, "startup terminal cleanup skipped");
                return;
            },
        };
        let manager = self.build_workspace_manager(&workflow);
        for issue in issues {
            if let Err(error) = manager
                .cleanup_workspace(
                    &issue.identifier,
                    issue.branch_name.clone(),
                    &workflow.config.hooks,
                )
                .await
            {
                warn!(%error, issue_identifier = %issue.identifier, "terminal cleanup failed");
            }
        }

        // Scan remaining workspaces on disk and cache the keys.
        let existing = manager.list_workspaces().await;
        let running_keys: HashSet<String> = self
            .state
            .running
            .values()
            .map(|r| sanitize_workspace_key(&r.issue.identifier))
            .collect();
        self.state.worktree_keys.clear();
        for (key, _path) in &existing {
            self.state.worktree_keys.insert(key.clone());
            if !running_keys.contains(key) {
                info!(workspace_key = %key, "orphaned workspace detected at startup");
                self.push_event(
                    EventScope::Startup,
                    format!("orphaned workspace found: {key}"),
                );
                self.state.orphan_dispatch_keys.insert(key.clone());
            }
        }
    }

    pub(crate) async fn stop_running(&mut self, issue_id: &str, cleanup_workspace: bool) {
        let workflow = self.workflow();
        if let Some(running) = self.state.running.remove(issue_id) {
            running.handle.abort();
            self.release_issue(issue_id);
            if cleanup_workspace {
                let workspace_key = sanitize_workspace_key(&running.issue.identifier);
                let manager = self.build_workspace_manager(&workflow);
                if let Err(error) = manager
                    .cleanup_workspace(
                        &running.issue.identifier,
                        running.issue.branch_name.clone(),
                        &workflow.config.hooks,
                    )
                    .await
                {
                    warn!(%error, issue_identifier = %running.issue.identifier, "cleanup failed");
                } else {
                    self.state.worktree_keys.remove(&workspace_key);
                }
            }
            // Mark the associated movement as cancelled.
            if let Some(movement) = self
                .state
                .movements
                .values_mut()
                .find(|m| m.issue_id.as_deref() == Some(issue_id))
            {
                movement.status = MovementStatus::Cancelled;
                movement.updated_at = Utc::now();
            }
            self.push_event(
                EventScope::Reconcile,
                format!("stopped {}", running.issue.identifier),
            );
        }
    }

    pub(crate) async fn fail_running(
        &mut self,
        issue_id: &str,
        status: AttemptStatus,
        reason: &str,
    ) {
        let Some((issue_identifier, attempt, started_at, turns_completed)) =
            self.state.running.get(issue_id).map(|running| {
                running.handle.abort();
                (
                    running.issue.identifier.clone(),
                    running.attempt,
                    running.started_at,
                    running.turn_count,
                )
            })
        else {
            return;
        };
        let _ = self
            .finish_running(
                issue_id.to_string(),
                issue_identifier,
                attempt,
                started_at,
                AgentRunResult {
                    status,
                    turns_completed,
                    error: Some(reason.to_string()),
                    final_issue_state: None,
                },
            )
            .await;
    }

    pub(crate) async fn abort_all(&mut self) {
        let running_ids = self.state.running.keys().cloned().collect::<Vec<_>>();
        for issue_id in running_ids {
            self.stop_running(&issue_id, false).await;
        }
    }
}
