use crate::{prelude::*, *};

pub(crate) struct EmptyTracker;

#[async_trait]
impl IssueTracker for EmptyTracker {
    fn component_key(&self) -> String {
        "tracker:none".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issues_by_ids(
        &self,
        _issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        _issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        Ok(Vec::new())
    }
}

struct CompositeTracker {
    primary: Arc<dyn IssueTracker>,
    supplements: Vec<SupplementalIssueTracker>,
}

struct SupplementalIssueTracker {
    tracker: Arc<dyn IssueTracker>,
    id_prefix: String,
    identifier_prefix: Option<String>,
}

impl SupplementalIssueTracker {
    fn matches_issue_id(&self, issue_id: &str) -> bool {
        issue_id.starts_with(&self.id_prefix)
    }

    fn strip_issue_id<'a>(&self, issue_id: &'a str) -> Option<&'a str> {
        issue_id.strip_prefix(&self.id_prefix)
    }

    fn namespace_issue(&self, issue: polyphony_core::Issue) -> polyphony_core::Issue {
        polyphony_core::Issue {
            id: format!("{}{}", self.id_prefix, issue.id),
            identifier: namespace_issue_identifier(
                self.identifier_prefix.as_deref(),
                &issue.identifier,
            ),
            blocked_by: issue
                .blocked_by
                .into_iter()
                .map(|blocker| polyphony_core::BlockerRef {
                    id: blocker.id.map(|id| format!("{}{}", self.id_prefix, id)),
                    identifier: blocker.identifier.map(|identifier| {
                        namespace_issue_identifier(self.identifier_prefix.as_deref(), &identifier)
                    }),
                    state: blocker.state,
                })
                .collect(),
            parent_id: issue
                .parent_id
                .map(|id| format!("{}{}", self.id_prefix, id)),
            ..issue
        }
    }

    fn denamespace_issue(&self, issue: &polyphony_core::Issue) -> polyphony_core::Issue {
        let mut issue = issue.clone();
        if let Some(stripped) = self.strip_issue_id(&issue.id) {
            issue.id = stripped.to_string();
        }
        issue.parent_id = issue
            .parent_id
            .as_deref()
            .and_then(|id| self.strip_issue_id(id))
            .map(str::to_string);
        issue.blocked_by = issue
            .blocked_by
            .into_iter()
            .map(|blocker| polyphony_core::BlockerRef {
                id: blocker
                    .id
                    .as_deref()
                    .and_then(|id| self.strip_issue_id(id))
                    .map(str::to_string),
                identifier: blocker.identifier,
                state: blocker.state,
            })
            .collect();
        issue
    }

    fn strip_issue_ids(&self, issue_ids: &[String]) -> Vec<String> {
        issue_ids
            .iter()
            .filter_map(|issue_id| self.strip_issue_id(issue_id))
            .map(str::to_string)
            .collect()
    }

    fn namespace_issues(&self, issues: Vec<polyphony_core::Issue>) -> Vec<polyphony_core::Issue> {
        issues
            .into_iter()
            .map(|issue| self.namespace_issue(issue))
            .collect()
    }
}

#[async_trait]
impl IssueTracker for CompositeTracker {
    fn component_key(&self) -> String {
        self.primary.component_key()
    }

    async fn fetch_candidate_issues(
        &self,
        query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        let mut issues = self.primary.fetch_candidate_issues(query).await?;
        let mut seen = issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect::<HashSet<_>>();
        for supplemental in &self.supplements {
            for issue in supplemental
                .namespace_issues(supplemental.tracker.fetch_candidate_issues(query).await?)
            {
                if seen.insert(issue.id.clone()) {
                    issues.push(issue);
                }
            }
        }
        Ok(issues)
    }

    async fn fetch_issues_by_states(
        &self,
        project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        let mut issues = self
            .primary
            .fetch_issues_by_states(project_slug, states)
            .await?;
        let mut seen = issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect::<HashSet<_>>();
        for supplemental in &self.supplements {
            for issue in supplemental.namespace_issues(
                supplemental
                    .tracker
                    .fetch_issues_by_states(project_slug, states)
                    .await?,
            ) {
                if seen.insert(issue.id.clone()) {
                    issues.push(issue);
                }
            }
        }
        Ok(issues)
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        let primary_ids = issue_ids
            .iter()
            .filter(|issue_id| {
                !self
                    .supplements
                    .iter()
                    .any(|s| s.matches_issue_id(issue_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut issues = if primary_ids.is_empty() {
            Vec::new()
        } else {
            self.primary.fetch_issues_by_ids(&primary_ids).await?
        };
        for supplemental in &self.supplements {
            let ids = supplemental.strip_issue_ids(issue_ids);
            if ids.is_empty() {
                continue;
            }
            issues.extend(
                supplemental
                    .namespace_issues(supplemental.tracker.fetch_issues_by_ids(&ids).await?),
            );
        }
        Ok(issues)
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        let primary_ids = issue_ids
            .iter()
            .filter(|issue_id| {
                !self
                    .supplements
                    .iter()
                    .any(|s| s.matches_issue_id(issue_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut updates = if primary_ids.is_empty() {
            Vec::new()
        } else {
            self.primary.fetch_issue_states_by_ids(&primary_ids).await?
        };
        for supplemental in &self.supplements {
            let ids = supplemental.strip_issue_ids(issue_ids);
            if ids.is_empty() {
                continue;
            }
            updates.extend(
                supplemental
                    .tracker
                    .fetch_issue_states_by_ids(&ids)
                    .await?
                    .into_iter()
                    .map(|update| polyphony_core::IssueStateUpdate {
                        id: format!("{}{}", supplemental.id_prefix, update.id),
                        identifier: namespace_issue_identifier(
                            supplemental.identifier_prefix.as_deref(),
                            &update.identifier,
                        ),
                        state: update.state,
                        updated_at: update.updated_at,
                    }),
            );
        }
        Ok(updates)
    }

    async fn fetch_budget(
        &self,
    ) -> Result<Option<polyphony_core::BudgetSnapshot>, polyphony_core::Error> {
        self.primary.fetch_budget().await
    }

    async fn ensure_issue_workflow_tracking(
        &self,
        issue: &polyphony_core::Issue,
    ) -> Result<(), polyphony_core::Error> {
        if let Some(supplemental) = self
            .supplements
            .iter()
            .find(|supplemental| supplemental.matches_issue_id(&issue.id))
        {
            return supplemental
                .tracker
                .ensure_issue_workflow_tracking(&supplemental.denamespace_issue(issue))
                .await;
        }
        self.primary.ensure_issue_workflow_tracking(issue).await
    }

    async fn update_issue_workflow_status(
        &self,
        issue: &polyphony_core::Issue,
        status: &str,
    ) -> Result<(), polyphony_core::Error> {
        if let Some(supplemental) = self
            .supplements
            .iter()
            .find(|supplemental| supplemental.matches_issue_id(&issue.id))
        {
            return supplemental
                .tracker
                .update_issue_workflow_status(&supplemental.denamespace_issue(issue), status)
                .await;
        }
        self.primary
            .update_issue_workflow_status(issue, status)
            .await
    }
}

fn namespace_issue_identifier(prefix: Option<&str>, identifier: &str) -> String {
    let Some(prefix) = prefix else {
        return identifier.to_string();
    };
    if identifier.starts_with(prefix) {
        return identifier.to_string();
    }
    if identifier.starts_with('#') {
        return format!("{prefix}{identifier}");
    }
    format!("{prefix}:{identifier}")
}

#[allow(unused_variables)]
pub(crate) fn build_runtime_components(
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<RuntimeComponents, Error> {
    let workflow_root = workflow_root_dir(&workflow.path)?;
    let primary_tracker: Arc<dyn IssueTracker> = match workflow.config.tracker.kind {
        TrackerKind::None => Arc::new(EmptyTracker),
        #[cfg(feature = "linear")]
        TrackerKind::Linear => {
            let api_key =
                workflow.config.tracker.api_key.clone().ok_or_else(|| {
                    Error::Config("tracker.api_key is required for linear".into())
                })?;
            Arc::new(polyphony_linear::LinearTracker::new(
                workflow.config.tracker.endpoint.clone(),
                api_key,
                workflow.config.tracker.team_id.clone(),
            )?)
        },
        #[cfg(feature = "github")]
        TrackerKind::Github => Arc::new(polyphony_github::GithubIssueTracker::new(
            workflow
                .config
                .tracker
                .repository
                .clone()
                .ok_or_else(|| Error::Config("tracker.repository is required".into()))?,
            workflow.config.tracker.api_key.clone(),
            workflow.config.tracker.project_owner.clone(),
            workflow.config.tracker.project_number,
            workflow.config.tracker.project_status_field.clone(),
        )?),
        #[cfg(feature = "beads")]
        TrackerKind::Beads => Arc::new(polyphony_beads::BeadsTracker::new(workflow_root.clone())?),
        other => {
            return Err(Error::Config(format!(
                "unsupported tracker.kind `{other}` for this build"
            )));
        },
    };
    let tracker = build_runtime_tracker(workflow, &workflow_root, primary_tracker)?;

    let feedback = {
        let registry = polyphony_feedback::FeedbackRegistry::from_config(&workflow.config.feedback);
        (!registry.is_empty()).then_some(Arc::new(registry))
    };
    #[cfg(feature = "github")]
    let pull_request_trigger_source: Option<Arc<dyn PullRequestTriggerSource>> =
        if workflow.config.tracker.kind == TrackerKind::Github {
            let repository = workflow.config.tracker.repository.clone().ok_or_else(|| {
                Error::Config("tracker.repository is required for github PR review triggers".into())
            })?;
            let token = workflow.config.tracker.api_key.clone().ok_or_else(|| {
                Error::Config("tracker.api_key is required for github PR review triggers".into())
            })?;
            Some(
                Arc::new(polyphony_github::GithubPullRequestReviewTriggerSource::new(
                    repository, token,
                )?) as Arc<dyn PullRequestTriggerSource>,
            )
        } else {
            None
        };
    #[cfg(not(feature = "github"))]
    let pull_request_trigger_source: Option<Arc<dyn PullRequestTriggerSource>> = None;
    let committer: Option<Arc<dyn WorkspaceCommitter>> =
        workflow.config.automation.enabled.then_some(
            Arc::new(polyphony_git::GitWorkspaceCommitter) as Arc<dyn WorkspaceCommitter>,
        );
    #[cfg(feature = "github")]
    let (pull_request_manager, pull_request_commenter) = if workflow.config.automation.enabled
        && workflow.config.tracker.kind == TrackerKind::Github
    {
        let repository = workflow.config.tracker.repository.clone().ok_or_else(|| {
            Error::Config("tracker.repository is required for github automation".into())
        })?;
        let token = workflow.config.tracker.api_key.clone().ok_or_else(|| {
            Error::Config("tracker.api_key is required for github automation".into())
        })?;
        (
            Some(Arc::new(polyphony_github::GithubPullRequestManager::new(
                repository.clone(),
                token.clone(),
            )?) as Arc<dyn PullRequestManager>),
            Some(
                Arc::new(polyphony_github::GithubPullRequestCommenter::new(token))
                    as Arc<dyn PullRequestCommenter>,
            ),
        )
    } else {
        (None, None)
    };
    #[cfg(not(feature = "github"))]
    let (pull_request_manager, pull_request_commenter): (
        Option<Arc<dyn PullRequestManager>>,
        Option<Arc<dyn PullRequestCommenter>>,
    ) = (None, None);

    Ok(RuntimeComponents {
        tracker,
        pull_request_trigger_source,
        agent: Arc::new(polyphony_agents::AgentRegistryRuntime::new()),
        committer,
        pull_request_manager,
        pull_request_commenter,
        feedback,
    })
}

pub(crate) fn build_runtime_tracker(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_root: &Path,
    primary_tracker: Arc<dyn IssueTracker>,
) -> Result<Arc<dyn IssueTracker>, Error> {
    let mut supplements = Vec::new();

    #[cfg(feature = "github")]
    if workflow.config.tracker.kind != TrackerKind::Github
        && let Some(repository) = polyphony_git::detect_github_remote(workflow_root)
    {
        supplements.push(SupplementalIssueTracker {
            tracker: Arc::new(polyphony_github::GithubIssueTracker::new(
                repository.clone(),
                github_token_from_env(),
                None,
                None,
                None,
            )?),
            id_prefix: format!("{GITHUB_SUPPLEMENTAL_PREFIX}{repository}:"),
            identifier_prefix: Some(repository),
        });
    }

    #[cfg(feature = "beads")]
    if workflow.config.tracker.kind != TrackerKind::Beads && workflow_root.join(".beads").is_dir() {
        supplements.push(SupplementalIssueTracker {
            tracker: Arc::new(polyphony_beads::BeadsTracker::new(
                workflow_root.to_path_buf(),
            )?),
            id_prefix: BEADS_SUPPLEMENTAL_PREFIX.into(),
            identifier_prefix: None,
        });
    }

    if supplements.is_empty() {
        Ok(primary_tracker)
    } else {
        Ok(Arc::new(CompositeTracker {
            primary: primary_tracker,
            supplements,
        }))
    }
}

fn github_token_from_env() -> Option<String> {
    env::var("GITHUB_TOKEN")
        .ok()
        .or_else(|| env::var("GH_TOKEN").ok())
}
