use crate::{prelude::*, *};

impl RuntimeService {
    fn rebuild_issue_repo_map(&mut self) {
        for row in &self.state.tracker_issues {
            if !row.repo_id.is_empty() {
                self.state
                    .issue_repo_map
                    .insert(row.issue_id.clone(), row.repo_id.clone());
            }
        }
        for row in &self.state.bootstrapped_tracker_issues {
            if !row.repo_id.is_empty() {
                self.state
                    .issue_repo_map
                    .insert(row.issue_id.clone(), row.repo_id.clone());
            }
        }
        for retry in self.state.retrying.values() {
            if !retry.row.repo_id.is_empty() {
                self.state
                    .issue_repo_map
                    .insert(retry.row.issue_id.clone(), retry.row.repo_id.clone());
            }
        }
        for context in self.state.saved_contexts.values() {
            if !context.repo_id.is_empty() {
                self.state
                    .issue_repo_map
                    .insert(context.issue_id.clone(), context.repo_id.clone());
            }
        }
    }

    pub(crate) fn should_dispatch(&self, workflow: &LoadedWorkflow, issue: &Issue) -> bool {
        if issue.id.is_empty()
            || issue.identifier.is_empty()
            || issue.title.is_empty()
            || issue.state.is_empty()
        {
            return false;
        }
        if self.state.running.contains_key(&issue.id) || self.is_claimed(&issue.id) {
            return false;
        }
        let state = issue.normalized_state();
        if !workflow.config.is_active_state(&issue.state)
            || workflow.config.is_terminal_state(&issue.state)
        {
            return false;
        }
        if state == "todo" {
            for blocker in &issue.blocked_by {
                let blocker_state = blocker
                    .state
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !blocker_state.is_empty() && !workflow.config.is_terminal_state(&blocker_state) {
                    return false;
                }
            }
        }
        true
    }

    /// Find an existing run for the given issue, preferring active ones.
    /// Returns the run ID if one exists, preventing duplicate runs
    /// when the same issue is re-dispatched (e.g. via retry or continuation).
    pub(crate) fn find_existing_run_for_issue(&self, issue_id: &str) -> Option<String> {
        let mut best: Option<(&str, bool)> = None;
        for (id, m) in &self.state.runs {
            if m.issue_id.as_deref() != Some(issue_id) {
                continue;
            }
            let is_active = !matches!(
                m.status,
                RunStatus::Delivered | RunStatus::Failed | RunStatus::Cancelled
            );
            match best {
                None => best = Some((id, is_active)),
                Some((_, false)) if is_active => best = Some((id, true)),
                _ => {},
            }
        }
        best.map(|(id, _)| id.to_string())
    }

    pub(crate) fn has_available_slot(&self, workflow: &LoadedWorkflow, state: &str) -> bool {
        if self.state.running.len() >= workflow.config.agent.max_concurrent_agents {
            return false;
        }
        let normalized = state.to_ascii_lowercase();
        if let Some(limit) = workflow.config.state_concurrency_limit(state) {
            let count = self
                .state
                .running
                .values()
                .filter(|entry| entry.issue.normalized_state() == normalized)
                .count();
            count < limit
        } else {
            true
        }
    }

    pub(crate) fn schedule_retry(
        &mut self,
        issue_id: String,
        issue_identifier: String,
        attempt: u32,
        error: Option<String>,
        continuation: bool,
        max_retry_backoff_ms: u64,
    ) {
        let immediate_retry = continuation || is_rate_limited_error(error.as_deref());
        let delay_ms = if immediate_retry {
            1_000
        } else {
            let exponent = attempt.saturating_sub(1).min(10);
            let delay = 10_000u64.saturating_mul(2u64.saturating_pow(exponent));
            delay.min(max_retry_backoff_ms)
        };
        let retry_repo_id = self.repo_id_for_issue(&issue_id);
        self.claim_issue(issue_id.clone(), IssueClaimState::RetryQueued);
        self.state.retrying.insert(issue_id.clone(), RetryEntry {
            row: RetryRow {
                repo_id: retry_repo_id,
                issue_id,
                issue_identifier: issue_identifier.clone(),
                attempt,
                due_at: Utc::now() + chrono::Duration::milliseconds(delay_ms as i64),
                error: error.clone(),
            },
            due_at: Instant::now() + Duration::from_millis(delay_ms),
        });
        self.push_event(
            EventScope::Retry,
            format!(
                "{} retry attempt={} delay_ms={} {}",
                issue_identifier,
                attempt,
                delay_ms,
                error.unwrap_or_default()
            ),
        );
    }

    pub(crate) fn next_retry_deadline(&self) -> Option<Instant> {
        self.state.retrying.values().map(|entry| entry.due_at).min()
    }

    pub(crate) fn push_event(&mut self, scope: EventScope, message: String) {
        self.state.recent_events.push_front(RuntimeEvent {
            at: Utc::now(),
            scope,
            message: truncate_runtime_event_message(message),
        });
        while self.state.recent_events.len() > MAX_RECENT_EVENTS {
            self.state.recent_events.pop_back();
        }
    }

    pub(crate) async fn emit_snapshot(&mut self) -> Result<(), Error> {
        let snapshot = self.snapshot();
        let _ = self.snapshot_tx.send(snapshot.clone());
        if let Some(store) = &self.store {
            store.save_snapshot(&snapshot).await?;
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> RuntimeSnapshot {
        let pending_user_interactions = self
            .user_interactions
            .lock()
            .ok()
            .map(|interactions| interactions.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut pending_user_interactions = pending_user_interactions;
        pending_user_interactions.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let live_seconds: f64 = self
            .state
            .running
            .values()
            .map(|running| {
                Utc::now()
                    .signed_duration_since(running.started_at)
                    .to_std()
                    .unwrap_or_default()
                    .as_secs_f64()
            })
            .sum();
        let mut totals = self.state.totals.clone();
        totals.seconds_running = self.state.ended_runtime_seconds + live_seconds;
        let tracker_kind = self.workflow_rx.borrow().config.tracker.kind;
        let issue_rows =
            if self.state.tracker_issue_snapshot_loaded || !self.state.tracker_issues.is_empty() {
                &self.state.tracker_issues
            } else {
                &self.state.bootstrapped_tracker_issues
            };
        let tracker_issues = issue_rows
            .iter()
            .map(|row| {
                let mut row = self.resolved_tracker_issue_row(
                    self.workflow_for_issue(&row.issue_id).config.tracker.kind,
                    row,
                );
                let key = sanitize_workspace_key(&row.issue_identifier);
                row.has_workspace = self.state.worktree_keys.contains(&key);
                row
            })
            .collect::<Vec<_>>();
        let mut inbox_items = tracker_issues
            .iter()
            .map(|row| {
                self.issue_inbox_item_row(
                    self.workflow_for_issue(&row.issue_id).config.tracker.kind,
                    row,
                )
            })
            .collect::<Vec<_>>();
        let mut approved_inbox_keys = self
            .state
            .approved_inbox_keys
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        approved_inbox_keys.sort();
        if self.state.pull_request_snapshot_loaded
            || !self.state.visible_review_events.is_empty()
            || !self.state.visible_comment_events.is_empty()
            || !self.state.visible_conflict_events.is_empty()
            || !self.state.discarded_inbox_items.is_empty()
        {
            inbox_items.extend(
                self.state
                    .visible_review_events
                    .values()
                    .cloned()
                    .map(PullRequestEvent::Review)
                    .map(|event| self.pull_request_inbox_item_row(&event)),
            );
            inbox_items.extend(
                self.state
                    .visible_comment_events
                    .values()
                    .cloned()
                    .map(PullRequestEvent::Comment)
                    .map(|event| self.pull_request_inbox_item_row(&event)),
            );
            inbox_items.extend(
                self.state
                    .visible_conflict_events
                    .values()
                    .cloned()
                    .map(PullRequestEvent::Conflict)
                    .map(|event| self.pull_request_inbox_item_row(&event)),
            );
            inbox_items.extend(
                self.state
                    .discarded_inbox_items
                    .values()
                    .map(|entry| entry.row.clone()),
            );
        } else {
            inbox_items.extend(
                self.state
                    .bootstrapped_inbox_items
                    .iter()
                    .filter(|item| item.kind != InboxItemKind::Issue)
                    .cloned(),
            );
        }
        let mut snapshot_repo_ids: Vec<String> = self.repos.keys().cloned().collect();
        snapshot_repo_ids.sort();
        let mut snapshot_repo_registrations = self
            .repos
            .values()
            .map(|ctx| ctx.registration.clone())
            .collect::<Vec<_>>();
        snapshot_repo_registrations.sort_by(|left, right| left.repo_id.cmp(&right.repo_id));
        RuntimeSnapshot {
            repo_ids: snapshot_repo_ids,
            repo_registrations: snapshot_repo_registrations,
            generated_at: Utc::now(),
            counts: SnapshotCounts {
                running: self.state.running.len(),
                retrying: self.state.retrying.len(),
                runs: self.state.runs.len(),
                tasks_pending: self
                    .state
                    .tasks
                    .values()
                    .flat_map(|t| t.iter())
                    .filter(|t| t.status == TaskStatus::Pending)
                    .count(),
                tasks_in_progress: self
                    .state
                    .tasks
                    .values()
                    .flat_map(|t| t.iter())
                    .filter(|t| t.status == TaskStatus::InProgress)
                    .count(),
                tasks_completed: self
                    .state
                    .tasks
                    .values()
                    .flat_map(|t| t.iter())
                    .filter(|t| t.status == TaskStatus::Completed)
                    .count(),
                worktrees: self.state.worktree_keys.len(),
            },
            cadence: RuntimeCadence {
                tracker_poll_interval_ms: self.workflow_rx.borrow().config.polling.interval_ms,
                budget_poll_interval_ms: 300_000,
                model_discovery_interval_ms: 300_000,
                last_tracker_poll_at: self.state.last_tracker_poll_at,
                last_budget_poll_at: self.state.last_budget_poll_at,
                last_model_discovery_at: self.state.last_model_discovery_at,
            },
            tracker_issues,
            inbox_items,
            approved_inbox_keys,
            running: self
                .state
                .running
                .values()
                .map(|running| RunningAgentRow {
                    repo_id: self.repo_id_for_issue(&running.issue.id),
                    issue_id: running.issue.id.clone(),
                    issue_identifier: running.issue.identifier.clone(),
                    agent_name: running.agent_name.clone(),
                    model: running.model.clone(),
                    state: running.issue.state.clone(),
                    max_turns: running.max_turns,
                    session_id: running.session_id.clone(),
                    thread_id: running.thread_id.clone(),
                    turn_id: running.turn_id.clone(),
                    codex_app_server_pid: running.codex_app_server_pid.clone(),
                    turn_count: running.turn_count,
                    last_event: running.last_event.clone(),
                    last_message: running.last_message.clone(),
                    started_at: running.started_at,
                    last_event_at: running.last_event_at,
                    tokens: running.tokens.clone(),
                    workspace_path: running.workspace_path.clone(),
                    attempt: running.attempt,
                    recent_log: running.recent_log.iter().cloned().collect(),
                })
                .collect(),
            agent_run_history: self
                .state
                .agent_run_history
                .iter()
                .cloned()
                .map(persisted_agent_run_metadata)
                .map(|run| run.to_agent_run_history_row())
                .collect(),
            retrying: self
                .state
                .retrying
                .values()
                .map(|entry| entry.row.clone())
                .collect(),
            codex_totals: totals,
            rate_limits: self.state.rate_limits.clone(),
            throttles: self
                .state
                .throttles
                .values()
                .map(|entry| entry.window.clone())
                .collect(),
            budgets: self.state.budgets.values().cloned().collect(),
            agent_catalogs: self.state.agent_catalogs.values().cloned().collect(),
            saved_contexts: self
                .state
                .saved_contexts
                .values()
                .cloned()
                .map(saved_context_metadata)
                .collect(),
            recent_events: self.state.recent_events.iter().cloned().collect(),
            pending_user_interactions,
            runs: self
                .state
                .runs
                .values()
                .map(|m| {
                    let tasks = self.state.tasks.get(&m.id);
                    let task_count = tasks.map(|t| t.len()).unwrap_or(0);
                    let tasks_completed = tasks
                        .map(|t| {
                            t.iter()
                                .filter(|t| t.status == TaskStatus::Completed)
                                .count()
                        })
                        .unwrap_or(0);
                    RunRow {
                        repo_id: m
                            .issue_id
                            .as_deref()
                            .map(|id| self.repo_id_for_issue(id))
                            .unwrap_or_default(),
                        id: m.id.clone(),
                        kind: m.kind,
                        issue_identifier: m.issue_identifier.clone(),
                        title: m.title.clone(),
                        status: m.status,
                        task_count,
                        tasks_completed,
                        deliverable: m.deliverable.clone(),
                        has_deliverable: m.deliverable.is_some(),
                        review_target: m.review_target.clone(),
                        workspace_key: m.workspace_key.clone(),
                        workspace_path: m.workspace_path.clone(),
                        created_at: m.created_at,
                        cancel_reason: m.cancel_reason.clone(),
                        steps: m.steps.clone(),
                        activity_log: m.activity_log.clone(),
                    }
                })
                .collect(),
            tasks: self
                .state
                .tasks
                .values()
                .flat_map(|tasks| {
                    tasks.iter().map(|t| TaskRow {
                        repo_id: self
                            .state
                            .runs
                            .get(&t.run_id)
                            .and_then(|m| m.issue_id.as_deref())
                            .map(|id| self.repo_id_for_issue(id))
                            .unwrap_or_default(),
                        id: t.id.clone(),
                        run_id: t.run_id.clone(),
                        title: t.title.clone(),
                        description: t.description.clone(),
                        activity_log: t.activity_log.clone(),
                        category: t.category,
                        status: t.status,
                        ordinal: t.ordinal,
                        agent_name: t.agent_name.clone(),
                        turns_completed: t.turns_completed,
                        total_tokens: t.tokens.total_tokens,
                        started_at: t.started_at,
                        finished_at: t.finished_at,
                        error: t.error.clone(),
                        created_at: t.created_at,
                        updated_at: t.updated_at,
                    })
                })
                .collect(),
            loading: self.state.loading.clone(),
            dispatch_mode: self.state.dispatch_mode,
            tracker_kind,
            tracker_connection: self.state.tracker_connection.clone().or_else(|| {
                (tracker_kind == TrackerKind::Github || tracker_kind == TrackerKind::Gitlab)
                    .then(TrackerConnectionStatus::unknown)
            }),
            from_cache: self.state.from_cache,
            cached_at: self.state.cached_at,
            agent_profile_names: self
                .workflow_rx
                .borrow()
                .config
                .agents
                .profiles
                .keys()
                .cloned()
                .collect(),
            agent_profiles: self
                .workflow_rx
                .borrow()
                .config
                .agents
                .profiles
                .iter()
                .map(|(name, profile)| AgentProfileSummary {
                    name: name.clone(),
                    kind: profile.kind.clone(),
                    description: profile.description.clone(),
                    source: profile.source,
                })
                .collect(),
            heartbeat: self.state.heartbeat_status.clone(),
        }
    }

    pub(crate) fn restore_cache(&mut self, cached: CachedSnapshot) {
        if self.state.tracker_issues.is_empty() {
            self.state.tracker_issues = cached.tracker_issues;
        }
        if self.state.tracker_issues.is_empty() && !cached.inbox_items.is_empty() {
            self.state.tracker_issues = cached
                .inbox_items
                .iter()
                .filter(|item| item.kind == InboxItemKind::Issue)
                .map(|item| TrackerIssueRow {
                    repo_id: item.repo_id.clone(),
                    issue_id: item.item_id.clone(),
                    issue_identifier: item.identifier.clone(),
                    title: item.title.clone(),
                    state: item.status.clone(),
                    approval_state: item.approval_state,
                    priority: item.priority,
                    labels: item.labels.clone(),
                    description: item.description.clone(),
                    url: item.url.clone(),
                    author: item.author.clone(),
                    parent_id: item.parent_id.clone(),
                    updated_at: item.updated_at,
                    created_at: item.created_at,
                    has_workspace: item.has_workspace,
                })
                .collect();
        }
        if self.state.budgets.is_empty() {
            for budget in cached.budgets {
                self.state.budgets.insert(budget.component.clone(), budget);
            }
        }
        if self.state.tracker_connection.is_none() {
            self.state.tracker_connection = cached.tracker_connection;
        }
        if self.state.approved_inbox_keys.is_empty() {
            self.state.approved_inbox_keys = cached.approved_inbox_keys.into_iter().collect();
        }
        if self.state.agent_catalogs.is_empty() {
            for catalog in cached.agent_catalogs {
                self.state
                    .agent_catalogs
                    .insert(catalog.agent_name.clone(), catalog);
            }
        }
        self.rebuild_issue_repo_map();
        self.state.from_cache = true;
        self.state.cached_at = cached.saved_at;
    }

    pub(crate) async fn save_cache(&self) {
        if let Some(cache) = &self.cache {
            let mut approved_inbox_keys = self
                .state
                .approved_inbox_keys
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            approved_inbox_keys.sort();
            let cached = CachedSnapshot {
                saved_at: Some(Utc::now()),
                tracker_issues: self.state.tracker_issues.clone(),
                inbox_items: self.snapshot().inbox_items,
                approved_inbox_keys,
                budgets: self.state.budgets.values().cloned().collect(),
                agent_catalogs: self.state.agent_catalogs.values().cloned().collect(),
                tracker_connection: self.state.tracker_connection.clone(),
            };
            if let Err(e) = cache.save(&cached).await {
                warn!(%e, "cache save failed");
            }
        }
    }

    pub(crate) fn restore_bootstrap(&mut self, bootstrap: polyphony_core::StoreBootstrap) {
        if let Some(snapshot) = bootstrap.snapshot {
            self.state.bootstrapped_tracker_issues = snapshot.tracker_issues;
            self.state.bootstrapped_inbox_items = snapshot.inbox_items;
            self.state.agent_catalogs = snapshot
                .agent_catalogs
                .into_iter()
                .map(|catalog| (catalog.agent_name.clone(), catalog))
                .collect();
            self.state.tracker_connection = snapshot.tracker_connection;
            self.state.dispatch_mode = snapshot.dispatch_mode;
            self.state.approved_inbox_keys = snapshot.approved_inbox_keys.into_iter().collect();
            self.state.totals = snapshot.codex_totals;
            self.state.rate_limits = snapshot.rate_limits;
            self.state.ended_runtime_seconds = self.state.totals.seconds_running;
            self.state.bootstrap_restored = true;
        }
        self.state.recent_events = bootstrap.recent_events.into_iter().collect();
        while self.state.recent_events.len() > MAX_RECENT_EVENTS {
            self.state.recent_events.pop_back();
        }
        self.state.budgets = bootstrap.budgets;
        self.state.saved_contexts = bootstrap.saved_contexts;
        for context in self.state.saved_contexts.values_mut() {
            compact_saved_context_in_place(context);
        }
        self.state.throttles = bootstrap
            .throttles
            .into_iter()
            .map(|(component, window)| {
                let due_at = window
                    .until
                    .signed_duration_since(Utc::now())
                    .to_std()
                    .map(|delta| Instant::now() + delta)
                    .unwrap_or_else(|_| Instant::now());
                (component, ActiveThrottle { window, due_at })
            })
            .collect();
        for (issue_id, row) in bootstrap.retrying {
            let due_at = row
                .due_at
                .signed_duration_since(Utc::now())
                .to_std()
                .map(|delta| Instant::now() + delta)
                .unwrap_or_else(|_| Instant::now());
            self.claim_issue(issue_id.clone(), IssueClaimState::RetryQueued);
            self.state
                .retrying
                .insert(issue_id, RetryEntry { row, due_at });
        }
        self.state.runs = bootstrap.runs;
        for (_task_id, task) in bootstrap.tasks {
            self.state
                .tasks
                .entry(task.run_id.clone())
                .or_default()
                .push(task);
        }
        for tasks in self.state.tasks.values_mut() {
            tasks.sort_by_key(|task| task.ordinal);
        }
        self.state.reviewed_pull_request_heads = bootstrap.reviewed_pull_request_heads;
        self.state.agent_run_history = bootstrap.agent_run_history.into_iter().collect();
        while self.state.agent_run_history.len() > MAX_RUN_HISTORY {
            self.state.agent_run_history.pop_back();
        }
        for run in &self.state.agent_run_history {
            let Some(workspace_path) = run.workspace_path.as_deref() else {
                continue;
            };
            let needs_reload = self
                .state
                .saved_contexts
                .get(&run.issue_id)
                .is_none_or(|context| context.transcript.is_empty());
            if !needs_reload {
                continue;
            }
            match load_workspace_saved_context_artifact(workspace_path) {
                Ok(Some(context)) => {
                    self.state
                        .saved_contexts
                        .insert(run.issue_id.clone(), context);
                },
                Ok(None) => {},
                Err(error) => {
                    warn!(
                        %error,
                        workspace_path = %workspace_path.display(),
                        issue_identifier = %run.issue_identifier,
                        "loading workspace saved context artifact failed"
                    );
                },
            }
        }
        self.rebuild_issue_repo_map();
    }

    pub(crate) fn register_throttle(&mut self, signal: RateLimitSignal) {
        let until = signal
            .reset_at
            .or_else(|| {
                signal
                    .retry_after_ms
                    .map(|ms| Utc::now() + chrono::Duration::milliseconds(ms as i64))
            })
            .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(60));
        let due_at = until
            .signed_duration_since(Utc::now())
            .to_std()
            .map(|delta| Instant::now() + delta)
            .unwrap_or_else(|_| Instant::now() + Duration::from_secs(1));
        self.state
            .throttles
            .insert(signal.component.clone(), ActiveThrottle {
                window: ThrottleWindow {
                    component: signal.component.clone(),
                    until,
                    reason: signal.reason.clone(),
                },
                due_at,
            });
        self.push_event(
            EventScope::Throttle,
            format!(
                "{} limited until {} ({})",
                signal.component, until, signal.reason
            ),
        );
    }

    pub(crate) fn is_throttled(&mut self, component: &str) -> bool {
        match self.state.throttles.get(component) {
            Some(throttle) if throttle.due_at > Instant::now() => true,
            Some(_) => {
                self.state.throttles.remove(component);
                false
            },
            None => false,
        }
    }

    pub(crate) async fn poll_budgets(&mut self) {
        let due = self
            .state
            .last_budget_poll_at
            .map(|at| Utc::now().signed_duration_since(at).num_seconds() >= 300)
            .unwrap_or(true);
        if !due {
            return;
        }
        // Clean up expired budget throttles
        self.state
            .throttles
            .retain(|_, t| t.due_at > Instant::now());
        self.state.last_budget_poll_at = Some(Utc::now());

        let tracker_components = if self.repos.is_empty() {
            vec![self.tracker.clone()]
        } else {
            self.repos
                .values()
                .map(|ctx| ctx.tracker.clone())
                .collect::<Vec<_>>()
        };
        let mut seen_tracker_components = HashSet::new();
        for tracker in tracker_components {
            let component_key = tracker.component_key();
            if !seen_tracker_components.insert(component_key.clone()) {
                continue;
            }
            match tracker.fetch_budget().await {
                Ok(Some(snapshot)) => self.record_budget(snapshot).await,
                Ok(None) => {},
                Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
                Err(error) => {
                    warn!(%error, component = %component_key, "tracker budget poll failed")
                },
            }
        }

        let agent_components = if self.repos.is_empty() {
            vec![(self.agent.clone(), self.workflow())]
        } else {
            self.repos
                .values()
                .map(|ctx| (ctx.agent.clone(), ctx.workflow.clone()))
                .collect::<Vec<_>>()
        };
        let mut grouped_agents = std::collections::HashMap::<
            String,
            (Arc<dyn AgentRuntime>, Vec<polyphony_core::AgentDefinition>),
        >::new();
        for (agent_runtime, workflow) in agent_components {
            let component_key = agent_runtime.component_key();
            let entry = grouped_agents
                .entry(component_key)
                .or_insert_with(|| (agent_runtime.clone(), Vec::new()));
            for agent in workflow.config.all_agents() {
                if entry.1.iter().all(|candidate| candidate.name != agent.name) {
                    entry.1.push(agent);
                }
            }
        }
        for (component_key, (agent_runtime, agents)) in grouped_agents {
            let agents: Vec<_> = agents
                .into_iter()
                .filter(|a| {
                    let throttle_key = format!("budget:{}", a.kind);
                    self.state
                        .throttles
                        .get(&throttle_key)
                        .is_none_or(|t| t.due_at <= Instant::now())
                })
                .collect();
            match agent_runtime.fetch_budgets(&agents).await {
                Ok(poll_result) => {
                    for snapshot in poll_result.snapshots {
                        self.record_budget(snapshot).await;
                    }
                    for signal in poll_result.throttles {
                        self.register_throttle(signal);
                    }
                },
                Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
                Err(error) => warn!(%error, component = %component_key, "agent budget poll failed"),
            }
        }
    }

    pub(crate) async fn refresh_tracker_connection(&mut self, force: bool) {
        let due = force
            || self
                .state
                .last_tracker_connection_poll_at
                .map(|at| Utc::now().signed_duration_since(at).num_seconds() >= 3_600)
                .unwrap_or(true);
        if !due {
            return;
        }
        self.state.last_tracker_connection_poll_at = Some(Utc::now());
        let workflow = self.workflow();
        let tracker_token = (workflow.config.tracker.kind == TrackerKind::Github)
            .then(|| workflow.config.tracker.api_key.clone())
            .flatten();
        let github_token = tracker_token
            .or_else(|| env::var("GITHUB_TOKEN").ok())
            .or_else(|| env::var("GH_TOKEN").ok());

        self.state.tracker_connection = Some(match github_token {
            Some(token) => self.fetch_github_connection_status(&token).await,
            None => TrackerConnectionStatus::disconnected("no token"),
        });
    }

    pub(crate) async fn fetch_github_connection_status(
        &self,
        token: &str,
    ) -> TrackerConnectionStatus {
        debug!("checking github viewer identity");
        let client = reqwest::Client::new();
        let response = match client
            .get("https://api.github.com/user")
            .bearer_auth(token)
            .header("User-Agent", "polyphony")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                warn!(%error, "github viewer identity request failed");
                return TrackerConnectionStatus::unknown();
            },
        };

        match response.status() {
            StatusCode::OK => match response.json::<GithubViewerIdentity>().await {
                Ok(viewer) if !viewer.login.is_empty() => {
                    info!(login = %viewer.login, "github viewer identity resolved");
                    TrackerConnectionStatus::connected(viewer.login)
                },
                Ok(_) => TrackerConnectionStatus::unknown(),
                Err(error) => {
                    warn!(%error, "github viewer identity decode failed");
                    TrackerConnectionStatus::unknown()
                },
            },
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                warn!("github viewer identity rejected token");
                TrackerConnectionStatus::disconnected("invalid token")
            },
            status => {
                warn!(%status, "github viewer identity returned unexpected status");
                TrackerConnectionStatus::unknown()
            },
        }
    }

    pub(crate) async fn refresh_agent_catalogs(&mut self) {
        let due = self
            .state
            .last_model_discovery_at
            .map(|at| Utc::now().signed_duration_since(at).num_seconds() >= 300)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.state.last_model_discovery_at = Some(Utc::now());
        let agent_components = if self.repos.is_empty() {
            vec![(self.agent.clone(), self.workflow())]
        } else {
            self.repos
                .values()
                .map(|ctx| (ctx.agent.clone(), ctx.workflow.clone()))
                .collect::<Vec<_>>()
        };
        let mut discovered_catalogs = std::collections::HashMap::new();
        for (agent_runtime, workflow) in agent_components {
            let component_key = agent_runtime.component_key();
            match agent_runtime
                .discover_models(&workflow.config.all_agents())
                .await
            {
                Ok(catalogs) => {
                    for catalog in catalogs {
                        discovered_catalogs.insert(catalog.agent_name.clone(), catalog);
                    }
                },
                Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
                Err(error) => {
                    warn!(%error, component = %component_key, "agent model discovery failed")
                },
            }
        }
        if !discovered_catalogs.is_empty() {
            self.state.agent_catalogs = discovered_catalogs;
            for running in self.state.running.values_mut() {
                if let Some(selected_model) = self
                    .state
                    .agent_catalogs
                    .get(&running.agent_name)
                    .and_then(|catalog| catalog.selected_model.clone())
                {
                    running.model = Some(selected_model);
                }
            }
        }
    }

    pub(crate) async fn record_budget(&mut self, snapshot: BudgetSnapshot) {
        self.state
            .budgets
            .insert(snapshot.component.clone(), snapshot.clone());
        if let Some(store) = &self.store
            && let Err(error) = store.record_budget(&snapshot).await
        {
            warn!(%error, "persisting budget snapshot failed");
        }
    }
}
