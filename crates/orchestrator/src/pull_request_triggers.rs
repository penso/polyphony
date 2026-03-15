use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) async fn poll_pull_request_triggers(
        &mut self,
        workflow: LoadedWorkflow,
        allow_dispatch: bool,
    ) {
        let Some(source) = self.pull_request_trigger_source.clone() else {
            return;
        };
        let source_component_key = source.component_key();
        if self.is_throttled(&source_component_key) {
            return;
        }
        let triggers = match source.fetch_triggers().await {
            Ok(triggers) => triggers,
            Err(CoreError::RateLimited(signal)) => {
                self.register_throttle(*signal);
                return;
            },
            Err(error) => {
                self.push_event(
                    EventScope::Tracker,
                    format!("pull request trigger fetch failed: {error}"),
                );
                warn!(%error, "pull request trigger fetch failed");
                return;
            },
        };
        let previous_triggers = self
            .state
            .visible_review_triggers
            .values()
            .cloned()
            .map(PullRequestTrigger::Review)
            .chain(
                self.state
                    .visible_comment_triggers
                    .values()
                    .cloned()
                    .map(PullRequestTrigger::Comment),
            )
            .chain(
                self.state
                    .visible_conflict_triggers
                    .values()
                    .cloned()
                    .map(PullRequestTrigger::Conflict),
            )
            .collect::<Vec<_>>();
        self.state.visible_review_triggers.clear();
        self.state.visible_comment_triggers.clear();
        self.state.visible_conflict_triggers.clear();
        for trigger in &triggers {
            match trigger {
                PullRequestTrigger::Review(trigger) => {
                    self.state
                        .visible_review_triggers
                        .insert(trigger.dedupe_key(), trigger.clone());
                },
                PullRequestTrigger::Comment(trigger) => {
                    self.state
                        .visible_comment_triggers
                        .insert(trigger.dedupe_key(), trigger.clone());
                },
                PullRequestTrigger::Conflict(trigger) => {
                    self.state
                        .visible_conflict_triggers
                        .insert(trigger.dedupe_key(), trigger.clone());
                },
            }
            self.state.discarded_triggers.remove(&trigger.dedupe_key());
        }
        let mut seen_trigger_keys = HashSet::new();
        for trigger in triggers {
            seen_trigger_keys.insert(trigger.dedupe_key());
            if let Some(reason) = self.pull_request_trigger_suppression(&workflow, &trigger) {
                self.record_review_trigger_suppression(trigger.dedupe_key(), reason);
                continue;
            }
            self.clear_review_trigger_suppression(&trigger.dedupe_key());
            if matches!(trigger, PullRequestTrigger::Conflict(_)) {
                continue;
            }
            if !allow_dispatch {
                continue;
            }
            if !self.has_available_slot(&workflow, "review") {
                break;
            }
            if let Err(error) = self
                .dispatch_pull_request_trigger(workflow.clone(), trigger.clone(), None)
                .await
            {
                self.state
                    .pull_request_retry_triggers
                    .insert(trigger.synthetic_issue_id(), trigger.clone());
                self.schedule_retry(
                    trigger.synthetic_issue_id(),
                    trigger.display_identifier(),
                    1,
                    Some(error.to_string()),
                    false,
                    workflow.config.agent.max_retry_backoff_ms,
                );
            }
        }
        self.state
            .review_trigger_suppressions
            .retain(|key, _| seen_trigger_keys.contains(key));
        for trigger in previous_triggers {
            if seen_trigger_keys.contains(&trigger.dedupe_key()) {
                continue;
            }
            if self.issue_is_actionable(&trigger.synthetic_issue_id()) {
                continue;
            }
            self.record_discarded_trigger(self.pull_request_trigger_row(&trigger));
        }
        self.prune_discarded_triggers();
    }

    pub(crate) fn pull_request_trigger_suppression(
        &self,
        workflow: &LoadedWorkflow,
        trigger: &PullRequestTrigger,
    ) -> Option<ReviewTriggerSuppression> {
        let (issue_id, labels, updated_at, is_draft) = match trigger {
            PullRequestTrigger::Review(trigger) => (
                trigger.synthetic_issue_id(),
                &trigger.labels,
                trigger.updated_at,
                trigger.is_draft,
            ),
            PullRequestTrigger::Comment(trigger) => (
                trigger.synthetic_issue_id(),
                &trigger.labels,
                trigger.updated_at.or(trigger.created_at),
                trigger.is_draft,
            ),
            PullRequestTrigger::Conflict(trigger) => (
                trigger.synthetic_issue_id(),
                &trigger.labels,
                trigger.updated_at.or(trigger.created_at),
                trigger.is_draft,
            ),
        };
        if !workflow.config.review_triggers.pr_reviews.include_drafts && is_draft {
            return Some(ReviewTriggerSuppression::Draft);
        }
        if self.state.running.contains_key(&issue_id) || self.is_claimed(&issue_id) {
            return Some(ReviewTriggerSuppression::AlreadyRunning);
        }

        if matches!(
            trigger,
            PullRequestTrigger::Review(_) | PullRequestTrigger::Comment(_)
        ) {
            let reviewed_key = review_target_key(&trigger.review_target());
            if let Some(reviewed) = self.state.reviewed_pull_request_heads.get(&reviewed_key) {
                let reviewed_after_update = updated_at
                    .map(|updated_at| reviewed.reviewed_at >= updated_at)
                    .unwrap_or(true);
                if reviewed_after_update {
                    return Some(ReviewTriggerSuppression::AlreadyReviewed);
                }
            }
        }

        if let Some(author) = pull_request_trigger_author(trigger) {
            if workflow
                .config
                .review_triggers
                .pr_reviews
                .ignore_authors
                .iter()
                .any(|candidate| candidate == author)
            {
                return Some(ReviewTriggerSuppression::IgnoredAuthor {
                    author: author.to_string(),
                });
            }
            if workflow
                .config
                .review_triggers
                .pr_reviews
                .ignore_bot_authors
                && is_probably_bot_author(author)
            {
                return Some(ReviewTriggerSuppression::BotAuthor {
                    author: author.to_string(),
                });
            }
        }

        if let Some(label) = labels.iter().find(|label| {
            workflow
                .config
                .review_triggers
                .pr_reviews
                .ignore_labels
                .contains(label)
        }) {
            return Some(ReviewTriggerSuppression::IgnoredLabel {
                label: label.clone(),
            });
        }
        if !workflow
            .config
            .review_triggers
            .pr_reviews
            .only_labels
            .is_empty()
            && !labels.iter().any(|label| {
                workflow
                    .config
                    .review_triggers
                    .pr_reviews
                    .only_labels
                    .contains(label)
            })
        {
            return Some(ReviewTriggerSuppression::MissingLabels {
                labels: workflow
                    .config
                    .review_triggers
                    .pr_reviews
                    .only_labels
                    .clone(),
            });
        }
        let updated_at = updated_at?;
        let debounce = chrono::Duration::seconds(
            workflow.config.review_triggers.pr_reviews.debounce_seconds as i64,
        );
        let remaining = debounce - Utc::now().signed_duration_since(updated_at);
        (remaining > chrono::Duration::zero()).then_some(ReviewTriggerSuppression::Debounced {
            remaining_seconds: remaining.num_seconds(),
        })
    }

    pub(crate) fn record_review_trigger_suppression(
        &mut self,
        key: String,
        suppression: ReviewTriggerSuppression,
    ) {
        let changed = self.state.review_trigger_suppressions.get(&key) != Some(&suppression);
        self.state
            .review_trigger_suppressions
            .insert(key.clone(), suppression.clone());
        if !changed {
            return;
        }
        let subject = self
            .visible_pull_request_trigger(&key)
            .map(|trigger| pull_request_trigger_subject(&trigger))
            .unwrap_or_else(|| "pull request trigger".into());
        let message = match suppression {
            ReviewTriggerSuppression::Draft => format!("suppressed {subject}: draft"),
            ReviewTriggerSuppression::AlreadyRunning => {
                format!("suppressed {subject}: already running")
            },
            ReviewTriggerSuppression::AlreadyReviewed => {
                format!("suppressed {subject}: already reviewed")
            },
            ReviewTriggerSuppression::IgnoredAuthor { author } => {
                format!("suppressed {subject}: ignored author {author}")
            },
            ReviewTriggerSuppression::BotAuthor { author } => {
                format!("suppressed {subject}: bot author {author}")
            },
            ReviewTriggerSuppression::IgnoredLabel { label } => {
                format!("suppressed {subject}: ignored label {label}")
            },
            ReviewTriggerSuppression::MissingLabels { labels } => {
                format!(
                    "suppressed {subject}: missing required labels {}",
                    labels.join(", ")
                )
            },
            ReviewTriggerSuppression::Debounced { remaining_seconds } => {
                format!(
                    "suppressed {subject}: debounce {}s remaining",
                    remaining_seconds.max(0)
                )
            },
        };
        self.push_event(EventScope::Tracker, message);
    }

    pub(crate) fn clear_review_trigger_suppression(&mut self, key: &str) {
        if self.state.review_trigger_suppressions.remove(key).is_some()
            && let Some(trigger) = self.visible_pull_request_trigger(key)
        {
            self.push_event(
                EventScope::Tracker,
                format!(
                    "{} ready: {}",
                    pull_request_trigger_kind_label(&trigger),
                    trigger.display_identifier()
                ),
            );
        }
    }

    pub(crate) fn pull_request_trigger_status(&self, trigger: &PullRequestTrigger) -> String {
        let issue_id = trigger.synthetic_issue_id();
        if self.state.running.contains_key(&issue_id) {
            return "running".into();
        }
        if self.state.retrying.contains_key(&issue_id) {
            return "retrying".into();
        }
        match self
            .state
            .review_trigger_suppressions
            .get(&trigger.dedupe_key())
        {
            Some(ReviewTriggerSuppression::Draft) => "draft".into(),
            Some(ReviewTriggerSuppression::AlreadyRunning) => "running".into(),
            Some(ReviewTriggerSuppression::AlreadyReviewed) => "reviewed".into(),
            Some(ReviewTriggerSuppression::IgnoredAuthor { .. }) => "ignored_author".into(),
            Some(ReviewTriggerSuppression::BotAuthor { .. }) => "ignored_bot".into(),
            Some(ReviewTriggerSuppression::IgnoredLabel { .. }) => "ignored_label".into(),
            Some(ReviewTriggerSuppression::MissingLabels { .. }) => "waiting_label".into(),
            Some(ReviewTriggerSuppression::Debounced { .. }) => "debouncing".into(),
            None => "ready".into(),
        }
    }

    pub(crate) async fn dispatch_pull_request_trigger(
        &mut self,
        workflow: LoadedWorkflow,
        trigger: PullRequestTrigger,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        match trigger {
            PullRequestTrigger::Review(trigger) => {
                self.dispatch_pull_request_review(workflow, trigger, attempt)
                    .await
            },
            PullRequestTrigger::Comment(trigger) => {
                self.dispatch_pull_request_comment_review(workflow, trigger, attempt)
                    .await
            },
            PullRequestTrigger::Conflict(trigger) => Err(Error::Core(CoreError::Adapter(format!(
                "pull request conflict trigger dispatch is not implemented yet for {}",
                trigger.display_identifier()
            )))),
        }
    }
}
