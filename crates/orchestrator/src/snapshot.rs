use crate::{prelude::*, *};

impl RuntimeService {
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
        self.claim_issue(issue_id.clone(), IssueClaimState::RetryQueued);
        self.state.retrying.insert(issue_id.clone(), RetryEntry {
            row: RetryRow {
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
            message,
        });
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
            if self.state.issue_snapshot_loaded || !self.state.visible_issues.is_empty() {
                &self.state.visible_issues
            } else {
                &self.state.bootstrapped_visible_issues
            };
        let visible_issues = issue_rows
            .iter()
            .map(|row| {
                let mut row = row.clone();
                let key = sanitize_workspace_key(&row.issue_identifier);
                row.has_workspace = self.state.worktree_keys.contains(&key);
                row
            })
            .collect::<Vec<_>>();
        let mut visible_triggers = visible_issues
            .iter()
            .map(|row| self.issue_trigger_row(tracker_kind, row))
            .collect::<Vec<_>>();
        if self.state.pull_request_snapshot_loaded
            || !self.state.visible_review_triggers.is_empty()
            || !self.state.visible_comment_triggers.is_empty()
            || !self.state.visible_conflict_triggers.is_empty()
            || !self.state.discarded_triggers.is_empty()
        {
            visible_triggers.extend(
                self.state
                    .visible_review_triggers
                    .values()
                    .cloned()
                    .map(PullRequestTrigger::Review)
                    .map(|trigger| self.pull_request_trigger_row(&trigger)),
            );
            visible_triggers.extend(
                self.state
                    .visible_comment_triggers
                    .values()
                    .cloned()
                    .map(PullRequestTrigger::Comment)
                    .map(|trigger| self.pull_request_trigger_row(&trigger)),
            );
            visible_triggers.extend(
                self.state
                    .visible_conflict_triggers
                    .values()
                    .cloned()
                    .map(PullRequestTrigger::Conflict)
                    .map(|trigger| self.pull_request_trigger_row(&trigger)),
            );
            visible_triggers.extend(
                self.state
                    .discarded_triggers
                    .values()
                    .map(|entry| entry.row.clone()),
            );
        } else {
            visible_triggers.extend(
                self.state
                    .bootstrapped_visible_triggers
                    .iter()
                    .filter(|trigger| trigger.kind != VisibleTriggerKind::Issue)
                    .cloned(),
            );
        }
        RuntimeSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts {
                running: self.state.running.len(),
                retrying: self.state.retrying.len(),
                movements: self.state.movements.len(),
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
                budget_poll_interval_ms: 60_000,
                model_discovery_interval_ms: 300_000,
                last_tracker_poll_at: self.state.last_tracker_poll_at,
                last_budget_poll_at: self.state.last_budget_poll_at,
                last_model_discovery_at: self.state.last_model_discovery_at,
            },
            visible_issues,
            visible_triggers,
            running: self
                .state
                .running
                .values()
                .map(|running| RunningRow {
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
                })
                .collect(),
            agent_history: self
                .state
                .run_history
                .iter()
                .map(PersistedRunRecord::to_history_row)
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
            saved_contexts: self.state.saved_contexts.values().cloned().collect(),
            recent_events: self.state.recent_events.iter().cloned().collect(),
            movements: self
                .state
                .movements
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
                    MovementRow {
                        id: m.id.clone(),
                        kind: m.kind,
                        issue_identifier: m.issue_identifier.clone(),
                        title: m.title.clone(),
                        status: m.status,
                        task_count,
                        tasks_completed,
                        has_deliverable: m.deliverable.is_some(),
                        review_target: m.review_target.clone(),
                        workspace_key: m.workspace_key.clone(),
                        workspace_path: m.workspace_path.clone(),
                        created_at: m.created_at,
                    }
                })
                .collect(),
            tasks: self
                .state
                .tasks
                .values()
                .flat_map(|tasks| {
                    tasks.iter().map(|t| TaskRow {
                        id: t.id.clone(),
                        movement_id: t.movement_id.clone(),
                        title: t.title.clone(),
                        category: t.category,
                        status: t.status,
                        ordinal: t.ordinal,
                        agent_name: t.agent_name.clone(),
                        turns_completed: t.turns_completed,
                        total_tokens: t.tokens.total_tokens,
                    })
                })
                .collect(),
            loading: self.state.loading.clone(),
            dispatch_mode: self.state.dispatch_mode,
            tracker_kind,
            tracker_connection: self.state.tracker_connection.clone().or_else(|| {
                (tracker_kind == TrackerKind::Github).then(TrackerConnectionStatus::unknown)
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
        }
    }

    pub(crate) fn restore_cache(&mut self, cached: CachedSnapshot) {
        if self.state.visible_issues.is_empty() {
            self.state.visible_issues = cached.visible_issues;
        }
        if self.state.visible_issues.is_empty() && !cached.visible_triggers.is_empty() {
            self.state.visible_issues = cached
                .visible_triggers
                .iter()
                .filter(|trigger| trigger.kind == VisibleTriggerKind::Issue)
                .map(|trigger| VisibleIssueRow {
                    issue_id: trigger.trigger_id.clone(),
                    issue_identifier: trigger.identifier.clone(),
                    title: trigger.title.clone(),
                    state: trigger.status.clone(),
                    priority: trigger.priority,
                    labels: trigger.labels.clone(),
                    description: trigger.description.clone(),
                    url: trigger.url.clone(),
                    author: trigger.author.clone(),
                    parent_id: trigger.parent_id.clone(),
                    updated_at: trigger.updated_at,
                    created_at: trigger.created_at,
                    has_workspace: trigger.has_workspace,
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
        if self.state.agent_catalogs.is_empty() {
            for catalog in cached.agent_catalogs {
                self.state
                    .agent_catalogs
                    .insert(catalog.agent_name.clone(), catalog);
            }
        }
        self.state.from_cache = true;
        self.state.cached_at = cached.saved_at;
    }

    pub(crate) async fn save_cache(&self) {
        if let Some(cache) = &self.cache {
            let cached = CachedSnapshot {
                saved_at: Some(Utc::now()),
                visible_issues: self.state.visible_issues.clone(),
                visible_triggers: self.snapshot().visible_triggers,
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
            self.state.bootstrapped_visible_issues = snapshot.visible_issues;
            self.state.bootstrapped_visible_triggers = snapshot.visible_triggers;
            self.state.agent_catalogs = snapshot
                .agent_catalogs
                .into_iter()
                .map(|catalog| (catalog.agent_name.clone(), catalog))
                .collect();
            self.state.tracker_connection = snapshot.tracker_connection;
            self.state.dispatch_mode = snapshot.dispatch_mode;
            self.state.totals = snapshot.codex_totals;
            self.state.rate_limits = snapshot.rate_limits;
            self.state.ended_runtime_seconds = self.state.totals.seconds_running;
        }
        self.state.recent_events = bootstrap.recent_events.into_iter().collect();
        self.state.budgets = bootstrap.budgets;
        self.state.saved_contexts = bootstrap.saved_contexts;
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
        self.state.movements = bootstrap.movements;
        for (_task_id, task) in bootstrap.tasks {
            self.state
                .tasks
                .entry(task.movement_id.clone())
                .or_default()
                .push(task);
        }
        for tasks in self.state.tasks.values_mut() {
            tasks.sort_by_key(|task| task.ordinal);
        }
        self.state.reviewed_pull_request_heads = bootstrap.reviewed_pull_request_heads;
        self.state.run_history = bootstrap.run_history.into_iter().collect();
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
            .map(|at| Utc::now().signed_duration_since(at).num_seconds() >= 60)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.state.last_budget_poll_at = Some(Utc::now());

        match self.tracker.fetch_budget().await {
            Ok(Some(snapshot)) => self.record_budget(snapshot).await,
            Ok(None) => {},
            Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
            Err(error) => warn!(%error, "tracker budget poll failed"),
        }

        let workflow = self.workflow();
        let all_agents = match workflow.config.all_agents() {
            Ok(agents) => agents,
            Err(error) => {
                warn!(%error, "agent budget poll skipped due to invalid agent configuration");
                return;
            },
        };
        match self.agent.fetch_budgets(&all_agents).await {
            Ok(snapshots) => {
                for snapshot in snapshots {
                    self.record_budget(snapshot).await;
                }
            },
            Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
            Err(error) => warn!(%error, "agent budget poll failed"),
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
        let workflow = self.workflow();
        let all_agents = match workflow.config.all_agents() {
            Ok(agents) => agents,
            Err(error) => {
                warn!(%error, "agent model discovery skipped due to invalid agent configuration");
                return;
            },
        };
        match self.agent.discover_models(&all_agents).await {
            Ok(catalogs) => {
                self.state.agent_catalogs = catalogs
                    .into_iter()
                    .map(|catalog| (catalog.agent_name.clone(), catalog))
                    .collect();
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
            },
            Err(CoreError::RateLimited(signal)) => self.register_throttle(*signal),
            Err(error) => warn!(%error, "agent model discovery failed"),
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
