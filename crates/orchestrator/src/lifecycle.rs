use crate::{prelude::*, *};

const SLOW_STARTUP_CLEANUP_WARN_THRESHOLD: Duration = Duration::from_millis(750);

impl RuntimeService {
    pub(crate) async fn record_run_history(
        &mut self,
        run: PersistedRunRecord,
    ) -> Result<(), Error> {
        if let Some(store) = &self.store {
            store.record_run(&run).await?;
        }
        if let Some(workspace_path) = run.workspace_path.as_deref()
            && let Err(error) = append_workspace_run_record_artifact(workspace_path, &run).await
        {
            warn!(
                %error,
                workspace_path = %workspace_path.display(),
                issue_identifier = %run.issue_identifier,
                "persisting workspace run artifact failed"
            );
        }
        self.state.run_history.push_front(run);
        while self.state.run_history.len() > MAX_RUN_HISTORY {
            self.state.run_history.pop_back();
        }
        Ok(())
    }

    pub(crate) async fn startup_cleanup(&mut self) {
        let workflow = self.workflow();
        let terminal = workflow.config.tracker.terminal_states.clone();
        let cleanup_started = Instant::now();
        let terminal_fetch_started = Instant::now();
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
        let terminal_fetch_elapsed = terminal_fetch_started.elapsed();
        if terminal_fetch_elapsed >= SLOW_STARTUP_CLEANUP_WARN_THRESHOLD {
            warn!(
                elapsed_ms = terminal_fetch_elapsed.as_millis(),
                issue_count = issues.len(),
                "startup terminal issue fetch was slow"
            );
        }
        let terminal_issue_ids = issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect::<HashSet<_>>();
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
        let stale_accepted_movements = self
            .state
            .movements
            .values()
            .filter(|movement| {
                movement.issue_id.as_ref().is_some_and(|issue_id| {
                    !terminal_issue_ids.contains(issue_id)
                        && movement.deliverable.as_ref().is_some_and(|deliverable| {
                            deliverable.status == polyphony_core::DeliverableStatus::Merged
                                && deliverable.decision
                                    == polyphony_core::DeliverableDecision::Accepted
                        })
                })
            })
            .map(|movement| movement.id.clone())
            .collect::<Vec<_>>();
        for movement_id in stale_accepted_movements {
            self.finalize_accepted_movement(&movement_id).await;
        }

        // Scan remaining workspaces on disk and cache the keys.
        let workspace_scan_started = Instant::now();
        let existing = manager.list_workspaces().await;
        let workspace_scan_elapsed = workspace_scan_started.elapsed();
        if workspace_scan_elapsed >= SLOW_STARTUP_CLEANUP_WARN_THRESHOLD {
            warn!(
                elapsed_ms = workspace_scan_elapsed.as_millis(),
                workspace_count = existing.len(),
                "startup workspace scan was slow"
            );
        }
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
        let cleanup_elapsed = cleanup_started.elapsed();
        if cleanup_elapsed >= SLOW_STARTUP_CLEANUP_WARN_THRESHOLD {
            warn!(
                elapsed_ms = cleanup_elapsed.as_millis(),
                worktree_count = self.state.worktree_keys.len(),
                "startup cleanup exceeded the slow-start threshold"
            );
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
                if let Some(store) = &self.store {
                    let _ = store.save_movement(movement).await;
                }
            }
            let finished_at = Utc::now();
            let outcome = AgentRunResult {
                status: AttemptStatus::CancelledByReconciliation,
                turns_completed: running.turn_count,
                error: Some("stopped by orchestrator reconciliation".into()),
                final_issue_state: None,
            };
            self.finalize_saved_context(issue_id, &running.issue.identifier, &running, &outcome);
            if let Some(context) = self.state.saved_contexts.get(issue_id)
                && let Err(error) =
                    persist_workspace_saved_context_artifact(&running.workspace_path, context).await
            {
                warn!(
                    %error,
                    workspace_path = %running.workspace_path.display(),
                    issue_identifier = %running.issue.identifier,
                    "persisting workspace saved context failed"
                );
            }
            let run = build_persisted_run_record(
                &running,
                outcome.status,
                finished_at,
                outcome.error.clone(),
                self.state.saved_contexts.get(issue_id).cloned(),
            );
            if let Err(error) = self.record_run_history(run).await {
                warn!(%error, issue_identifier = %running.issue.identifier, "persisting cancelled run failed");
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

    /// Stop a running agent by user request. Does not clean up workspace so the
    /// user can inspect the work done so far.
    pub(crate) async fn stop_running_by_user(&mut self, issue_id: &str) {
        if let Some(running) = self.state.running.remove(issue_id) {
            running.handle.abort();
            self.release_issue(issue_id);
            // Mark the associated movement as cancelled.
            if let Some(movement) = self
                .state
                .movements
                .values_mut()
                .find(|m| m.issue_id.as_deref() == Some(issue_id))
            {
                movement.status = MovementStatus::Cancelled;
                movement.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    let _ = store.save_movement(movement).await;
                }
            }
            let finished_at = Utc::now();
            let outcome = AgentRunResult {
                status: AttemptStatus::CancelledByUser,
                turns_completed: running.turn_count,
                error: Some("stopped by user".into()),
                final_issue_state: None,
            };
            self.finalize_saved_context(issue_id, &running.issue.identifier, &running, &outcome);
            if let Some(context) = self.state.saved_contexts.get(issue_id)
                && let Err(error) =
                    persist_workspace_saved_context_artifact(&running.workspace_path, context).await
            {
                warn!(
                    %error,
                    workspace_path = %running.workspace_path.display(),
                    issue_identifier = %running.issue.identifier,
                    "persisting workspace saved context failed"
                );
            }
            let run = build_persisted_run_record(
                &running,
                outcome.status,
                finished_at,
                outcome.error.clone(),
                self.state.saved_contexts.get(issue_id).cloned(),
            );
            if let Err(error) = self.record_run_history(run).await {
                warn!(%error, issue_identifier = %running.issue.identifier, "persisting stopped run failed");
            }
            self.push_event(
                EventScope::Dispatch,
                format!("user stopped {}", running.issue.identifier),
            );
        }
    }

    pub(crate) async fn abort_all(&mut self) {
        let running_ids = self.state.running.keys().cloned().collect::<Vec<_>>();
        for issue_id in running_ids {
            self.stop_running(&issue_id, false).await;
        }
        // Drain pending retries so nothing restarts when the mode changes back.
        let retrying_ids = self.state.retrying.keys().cloned().collect::<Vec<_>>();
        for issue_id in retrying_ids {
            self.state.retrying.remove(&issue_id);
            self.release_issue(&issue_id);
        }
    }
}
