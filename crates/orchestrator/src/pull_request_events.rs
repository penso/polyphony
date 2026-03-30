use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn poll_pull_request_events(
        &mut self,
        sources: Vec<(String, LoadedWorkflow, Arc<dyn PullRequestEventSource>)>,
    ) {
        if sources.is_empty() {
            self.state.pull_request_snapshot_loaded = true;
            self.state.visible_review_events.clear();
            self.state.visible_comment_events.clear();
            self.state.visible_conflict_events.clear();
            return;
        }
        let previous_events = self
            .state
            .visible_review_events
            .values()
            .cloned()
            .map(PullRequestEvent::Review)
            .chain(
                self.state
                    .visible_comment_events
                    .values()
                    .cloned()
                    .map(PullRequestEvent::Comment),
            )
            .chain(
                self.state
                    .visible_conflict_events
                    .values()
                    .cloned()
                    .map(PullRequestEvent::Conflict),
            )
            .collect::<Vec<_>>();
        let mut fetches = tokio::task::JoinSet::new();
        for (repo_id, workflow, source) in sources {
            let source_component_key = source.component_key();
            if self.is_throttled(&source_component_key) {
                continue;
            }
            fetches.spawn(async move {
                let result = source.fetch_events().await;
                (repo_id, workflow, source_component_key, result)
            });
        }
        let mut events = Vec::new();
        while let Some(result) = fetches.join_next().await {
            let Ok((repo_id, workflow, source_component_key, result)) = result else {
                continue;
            };
            match result {
                Ok(repo_events) => {
                    for event in repo_events {
                        if !repo_id.is_empty() {
                            self.state
                                .issue_repo_map
                                .insert(event.synthetic_issue_id(), repo_id.clone());
                        }
                        events.push((repo_id.clone(), workflow.clone(), event));
                    }
                },
                Err(CoreError::RateLimited(signal)) => {
                    self.register_throttle(*signal);
                },
                Err(error) => {
                    self.push_event(
                        EventScope::Tracker,
                        format!(
                            "pull request event fetch failed for {source_component_key}: {error}"
                        ),
                    );
                    warn!(%error, component = %source_component_key, "pull request event fetch failed");
                },
            }
        }
        self.state.pull_request_snapshot_loaded = true;
        self.state.visible_review_events.clear();
        self.state.visible_comment_events.clear();
        self.state.visible_conflict_events.clear();
        for (_, _, event) in &events {
            match event {
                PullRequestEvent::Review(event) => {
                    self.state
                        .visible_review_events
                        .insert(event.dedupe_key(), event.clone());
                },
                PullRequestEvent::Comment(event) => {
                    self.state
                        .visible_comment_events
                        .insert(event.dedupe_key(), event.clone());
                },
                PullRequestEvent::Conflict(event) => {
                    self.state
                        .visible_conflict_events
                        .insert(event.dedupe_key(), event.clone());
                },
            }
            self.state.discarded_inbox_items.remove(&event.dedupe_key());
        }
        let mut seen_event_keys = HashSet::new();
        for (_, workflow, event) in events {
            seen_event_keys.insert(event.dedupe_key());
            if let Some(reason) = self.pull_request_event_suppression(&workflow, &event) {
                self.record_review_event_suppression(event.dedupe_key(), reason);
                continue;
            }
            self.clear_review_event_suppression(&event.dedupe_key());
            if matches!(event, PullRequestEvent::Conflict(_)) {
                continue;
            }
            let allow_dispatch = match self.state.dispatch_mode {
                polyphony_core::DispatchMode::Manual | polyphony_core::DispatchMode::Stop => false,
                polyphony_core::DispatchMode::Automatic
                | polyphony_core::DispatchMode::Nightshift => true,
                polyphony_core::DispatchMode::Idle => {
                    match self.idle_dispatch_allowed_for_pr_reviews(&workflow) {
                        Ok(allowed) => allowed,
                        Err(error) => {
                            self.push_event(
                                EventScope::Dispatch,
                                format!("idle PR review gate failed: {error}"),
                            );
                            false
                        },
                    }
                },
            };
            if !allow_dispatch {
                continue;
            }
            if !self.has_available_slot(&workflow, "review") {
                break;
            }
            if let Err(error) = self
                .dispatch_pull_request_event(workflow.clone(), event.clone(), None, None)
                .await
            {
                self.state
                    .pull_request_retry_events
                    .insert(event.synthetic_issue_id(), event.clone());
                self.schedule_retry(
                    event.synthetic_issue_id(),
                    event.display_identifier(),
                    1,
                    Some(error.to_string()),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            }
        }
        self.state
            .review_event_suppressions
            .retain(|key, _| seen_event_keys.contains(key));
        for event in previous_events {
            if seen_event_keys.contains(&event.dedupe_key()) {
                continue;
            }
            if self.issue_is_actionable(&event.synthetic_issue_id()) {
                continue;
            }
            self.record_discarded_inbox_item(self.pull_request_inbox_item_row(&event));
        }
        self.prune_discarded_inbox_items();
    }

    pub(crate) fn pull_request_event_suppression(
        &self,
        workflow: &LoadedWorkflow,
        event: &PullRequestEvent,
    ) -> Option<ReviewEventSuppression> {
        let (issue_id, labels, updated_at, is_draft) = match event {
            PullRequestEvent::Review(event) => (
                event.synthetic_issue_id(),
                &event.labels,
                event.updated_at,
                event.is_draft,
            ),
            PullRequestEvent::Comment(event) => (
                event.synthetic_issue_id(),
                &event.labels,
                event.updated_at.or(event.created_at),
                event.is_draft,
            ),
            PullRequestEvent::Conflict(event) => (
                event.synthetic_issue_id(),
                &event.labels,
                event.updated_at.or(event.created_at),
                event.is_draft,
            ),
        };
        if self.pull_request_event_approval_state(event) == DispatchApprovalState::Waiting {
            return Some(ReviewEventSuppression::AwaitingApproval);
        }
        if !workflow.config.review_events.pr_reviews.include_drafts && is_draft {
            return Some(ReviewEventSuppression::Draft);
        }
        if self.state.running.contains_key(&issue_id) || self.is_claimed(&issue_id) {
            return Some(ReviewEventSuppression::AlreadyRunning);
        }

        if matches!(
            event,
            PullRequestEvent::Review(_) | PullRequestEvent::Comment(_)
        ) {
            let reviewed_key = review_target_key(&event.review_target());
            if let Some(reviewed) = self.state.reviewed_pull_request_heads.get(&reviewed_key) {
                let reviewed_after_update = updated_at
                    .map(|updated_at| reviewed.reviewed_at >= updated_at)
                    .unwrap_or(true);
                if reviewed_after_update {
                    return Some(ReviewEventSuppression::AlreadyReviewed);
                }
            }
        }

        if let Some(author) = pull_request_event_author(event) {
            if workflow
                .config
                .review_events
                .pr_reviews
                .ignore_authors
                .iter()
                .any(|candidate| candidate == author)
            {
                return Some(ReviewEventSuppression::IgnoredAuthor {
                    author: author.to_string(),
                });
            }
            if workflow.config.review_events.pr_reviews.ignore_bot_authors
                && is_probably_bot_author(author)
            {
                return Some(ReviewEventSuppression::BotAuthor {
                    author: author.to_string(),
                });
            }
        }

        if let Some(label) = labels.iter().find(|label| {
            workflow
                .config
                .review_events
                .pr_reviews
                .ignore_labels
                .contains(label)
        }) {
            return Some(ReviewEventSuppression::IgnoredLabel {
                label: label.clone(),
            });
        }
        if !workflow
            .config
            .review_events
            .pr_reviews
            .only_labels
            .is_empty()
            && !labels.iter().any(|label| {
                workflow
                    .config
                    .review_events
                    .pr_reviews
                    .only_labels
                    .contains(label)
            })
        {
            return Some(ReviewEventSuppression::MissingLabels {
                labels: workflow.config.review_events.pr_reviews.only_labels.clone(),
            });
        }
        let updated_at = updated_at?;
        let debounce = chrono::Duration::seconds(
            workflow.config.review_events.pr_reviews.debounce_seconds as i64,
        );
        let remaining = debounce - Utc::now().signed_duration_since(updated_at);
        (remaining > chrono::Duration::zero()).then_some(ReviewEventSuppression::Debounced {
            remaining_seconds: remaining.num_seconds(),
        })
    }

    pub(crate) fn record_review_event_suppression(
        &mut self,
        key: String,
        suppression: ReviewEventSuppression,
    ) {
        let changed = self.state.review_event_suppressions.get(&key) != Some(&suppression);
        self.state
            .review_event_suppressions
            .insert(key.clone(), suppression.clone());
        if !changed {
            return;
        }
        let subject = self
            .visible_pull_request_event(&key)
            .map(|event| pull_request_event_subject(&event))
            .unwrap_or_else(|| "pull request event".into());
        let message = match suppression {
            ReviewEventSuppression::AwaitingApproval => {
                format!("suppressed {subject}: awaiting approval")
            },
            ReviewEventSuppression::Draft => format!("suppressed {subject}: draft"),
            ReviewEventSuppression::AlreadyRunning => {
                format!("suppressed {subject}: already running")
            },
            ReviewEventSuppression::AlreadyReviewed => {
                format!("suppressed {subject}: already reviewed")
            },
            ReviewEventSuppression::IgnoredAuthor { author } => {
                format!("suppressed {subject}: ignored author {author}")
            },
            ReviewEventSuppression::BotAuthor { author } => {
                format!("suppressed {subject}: bot author {author}")
            },
            ReviewEventSuppression::IgnoredLabel { label } => {
                format!("suppressed {subject}: ignored label {label}")
            },
            ReviewEventSuppression::MissingLabels { labels } => {
                format!(
                    "suppressed {subject}: missing required labels {}",
                    labels.join(", ")
                )
            },
            ReviewEventSuppression::Debounced { remaining_seconds } => {
                format!(
                    "suppressed {subject}: debounce {}s remaining",
                    remaining_seconds.max(0)
                )
            },
        };
        self.push_event(EventScope::Tracker, message);
    }

    pub(crate) fn clear_review_event_suppression(&mut self, key: &str) {
        if self.state.review_event_suppressions.remove(key).is_some()
            && let Some(event) = self.visible_pull_request_event(key)
        {
            self.push_event(
                EventScope::Tracker,
                format!(
                    "{} ready: {}",
                    pull_request_event_kind_label(&event),
                    event.display_identifier()
                ),
            );
        }
    }

    pub(crate) fn pull_request_event_status(&self, event: &PullRequestEvent) -> String {
        let issue_id = event.synthetic_issue_id();
        if self.state.running.contains_key(&issue_id) {
            return "running".into();
        }
        if self.state.retrying.contains_key(&issue_id) {
            return "retrying".into();
        }
        match self
            .state
            .review_event_suppressions
            .get(&event.dedupe_key())
        {
            Some(ReviewEventSuppression::AwaitingApproval) => "waiting_approval".into(),
            Some(ReviewEventSuppression::Draft) => "draft".into(),
            Some(ReviewEventSuppression::AlreadyRunning) => "running".into(),
            Some(ReviewEventSuppression::AlreadyReviewed) => "reviewed".into(),
            Some(ReviewEventSuppression::IgnoredAuthor { .. }) => "ignored_author".into(),
            Some(ReviewEventSuppression::BotAuthor { .. }) => "ignored_bot".into(),
            Some(ReviewEventSuppression::IgnoredLabel { .. }) => "ignored_label".into(),
            Some(ReviewEventSuppression::MissingLabels { .. }) => "waiting_label".into(),
            Some(ReviewEventSuppression::Debounced { .. }) => "debouncing".into(),
            None => "ready".into(),
        }
    }

    pub(crate) async fn dispatch_pull_request_event(
        &mut self,
        workflow: LoadedWorkflow,
        event: PullRequestEvent,
        attempt: Option<u32>,
        directives: Option<&str>,
    ) -> Result<(), Error> {
        match event {
            PullRequestEvent::Review(event) => {
                self.dispatch_pull_request_review(workflow, event, attempt, directives)
                    .await
            },
            PullRequestEvent::Comment(event) => {
                self.dispatch_pull_request_comment_review(workflow, event, attempt, directives)
                    .await
            },
            PullRequestEvent::Conflict(event) => Err(Error::Core(CoreError::Adapter(format!(
                "pull request conflict event dispatch is not implemented yet for {}",
                event.display_identifier()
            )))),
        }
    }
}
