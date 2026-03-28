use crate::{prelude::*, *};

const SLOW_TRACKER_COMPONENT_FETCH_WARN_THRESHOLD_MS: u128 = 750;

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
        let primary_component = self.primary.component_key();
        let primary_started = Instant::now();
        let mut issues = self.primary.fetch_candidate_issues(query).await?;
        let primary_elapsed = primary_started.elapsed().as_millis();
        info!(
            component = %primary_component,
            elapsed_ms = primary_elapsed,
            count = issues.len(),
            "tracker component fetched candidate issues"
        );
        if primary_elapsed >= SLOW_TRACKER_COMPONENT_FETCH_WARN_THRESHOLD_MS {
            warn!(
                component = %primary_component,
                elapsed_ms = primary_elapsed,
                count = issues.len(),
                "tracker component candidate issue fetch was slow"
            );
        }
        let mut seen = issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect::<HashSet<_>>();
        for supplemental in &self.supplements {
            let supplemental_component = supplemental.tracker.component_key();
            let supplemental_started = Instant::now();
            let supplemental_issues = supplemental.tracker.fetch_candidate_issues(query).await?;
            let supplemental_elapsed = supplemental_started.elapsed().as_millis();
            info!(
                component = %supplemental_component,
                elapsed_ms = supplemental_elapsed,
                count = supplemental_issues.len(),
                "supplemental tracker fetched candidate issues"
            );
            if supplemental_elapsed >= SLOW_TRACKER_COMPONENT_FETCH_WARN_THRESHOLD_MS {
                warn!(
                    component = %supplemental_component,
                    elapsed_ms = supplemental_elapsed,
                    count = supplemental_issues.len(),
                    "supplemental tracker candidate issue fetch was slow"
                );
            }
            for issue in supplemental.namespace_issues(supplemental_issues) {
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

    async fn create_issue(
        &self,
        request: &polyphony_core::CreateIssueRequest,
    ) -> Result<polyphony_core::Issue, polyphony_core::Error> {
        self.primary.create_issue(request).await
    }

    async fn update_issue(
        &self,
        request: &polyphony_core::UpdateIssueRequest,
    ) -> Result<polyphony_core::Issue, polyphony_core::Error> {
        if let Some(supplemental) = self
            .supplements
            .iter()
            .find(|supplemental| supplemental.matches_issue_id(&request.id))
        {
            let mut request = request.clone();
            request.id = supplemental
                .strip_issue_id(&request.id)
                .ok_or_else(|| {
                    polyphony_core::Error::Adapter("invalid supplemental issue id".into())
                })?
                .to_string();
            return supplemental
                .tracker
                .update_issue(&request)
                .await
                .map(|issue| supplemental.namespace_issue(issue));
        }
        self.primary.update_issue(request).await
    }

    async fn comment_on_issue(
        &self,
        request: &polyphony_core::AddIssueCommentRequest,
    ) -> Result<polyphony_core::IssueComment, polyphony_core::Error> {
        if let Some(supplemental) = self
            .supplements
            .iter()
            .find(|supplemental| supplemental.matches_issue_id(&request.id))
        {
            let mut request = request.clone();
            request.id = supplemental
                .strip_issue_id(&request.id)
                .ok_or_else(|| {
                    polyphony_core::Error::Adapter("invalid supplemental issue id".into())
                })?
                .to_string();
            return supplemental.tracker.comment_on_issue(&request).await;
        }
        self.primary.comment_on_issue(request).await
    }

    async fn acknowledge_issue(
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
                .acknowledge_issue(&supplemental.denamespace_issue(issue))
                .await;
        }
        self.primary.acknowledge_issue(issue).await
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
        #[cfg(feature = "gitlab")]
        TrackerKind::Gitlab => {
            let (endpoint, project_path, token) =
                resolve_gitlab_integration(workflow, &workflow_root)?;
            Arc::new(polyphony_gitlab::GitlabIssueTracker::new(
                endpoint,
                token,
                project_path,
            )?)
        },
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
    let (pull_request_event_source, pull_request_manager, pull_request_commenter) =
        build_pull_request_components(workflow, &workflow_root)?;
    let committer: Option<Arc<dyn WorkspaceCommitter>> = workflow
        .config
        .automation
        .enabled
        .then_some(Arc::new(polyphony_git::GitWorkspaceCommitter::default())
            as Arc<dyn WorkspaceCommitter>);
    let tool_executor = polyphony_tools::RegistryToolExecutor::from_runtime_components(
        workflow,
        tracker.clone(),
        pull_request_commenter.clone(),
    )?;

    Ok(RuntimeComponents {
        tracker,
        pull_request_event_source,
        agent: Arc::new(polyphony_agents::AgentRegistryRuntime::new_with_tools(
            tool_executor,
        )),
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
    #[cfg(any(feature = "github", feature = "beads"))]
    let mut supplements = Vec::new();
    #[cfg(not(any(feature = "github", feature = "beads")))]
    let supplements: Vec<SupplementalIssueTracker> = Vec::new();

    #[cfg(not(any(feature = "github", feature = "beads")))]
    let _ = (workflow, workflow_root);

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

#[allow(unused_variables)]
fn build_pull_request_components(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_root: &Path,
) -> Result<
    (
        Option<Arc<dyn PullRequestEventSource>>,
        Option<Arc<dyn PullRequestManager>>,
        Option<Arc<dyn PullRequestCommenter>>,
    ),
    Error,
> {
    #[cfg(feature = "github")]
    if workflow.config.tracker.kind == TrackerKind::Github
        || polyphony_git::detect_github_remote(workflow_root).is_some()
    {
        return build_github_pull_request_components(
            workflow,
            workflow_root,
            github_token_from_env(),
        );
    }

    #[cfg(feature = "gitlab")]
    if workflow.config.tracker.kind == TrackerKind::Gitlab
        || polyphony_git::detect_gitlab_remote(workflow_root).is_some()
    {
        return build_gitlab_pull_request_components(workflow, workflow_root);
    }

    Ok((None, None, None))
}

#[cfg(feature = "gitlab")]
fn build_gitlab_pull_request_components(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_root: &Path,
) -> Result<
    (
        Option<Arc<dyn PullRequestEventSource>>,
        Option<Arc<dyn PullRequestManager>>,
        Option<Arc<dyn PullRequestCommenter>>,
    ),
    Error,
> {
    let (endpoint, project_path, token) = resolve_gitlab_integration(workflow, workflow_root)?;
    let Some(token) = token else {
        return Ok((None, None, None));
    };

    let event_source = if workflow.config.review_events.pr_reviews.enabled {
        Some(Arc::new(
            polyphony_gitlab::merge_requests::GitlabMergeRequestEventSource::new(
                endpoint.clone(),
                token.clone(),
                project_path.clone(),
            )?,
        ) as Arc<dyn PullRequestEventSource>)
    } else {
        None
    };

    let commenter = Some(Arc::new(
        polyphony_gitlab::merge_requests::GitlabPullRequestCommenter::new(
            endpoint.clone(),
            token.clone(),
            project_path.clone(),
        )?,
    ) as Arc<dyn PullRequestCommenter>);

    let manager = workflow
        .config
        .automation
        .enabled
        .then(|| {
            polyphony_gitlab::merge_requests::GitlabPullRequestManager::new(
                endpoint,
                token,
                project_path,
            )
            .map(|m| Arc::new(m) as Arc<dyn PullRequestManager>)
        })
        .transpose()?;

    Ok((event_source, manager, commenter))
}

#[cfg(feature = "gitlab")]
fn resolve_gitlab_integration(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_root: &Path,
) -> Result<(String, String, Option<String>), Error> {
    let gitlab_token = env::var("GITLAB_TOKEN").ok();

    if workflow.config.tracker.kind == TrackerKind::Gitlab {
        let project_path = workflow
            .config
            .tracker
            .project_slug
            .clone()
            .or_else(|| workflow.config.tracker.repository.clone())
            .ok_or_else(|| {
                Error::Config(
                    "tracker.project_slug or tracker.repository is required for gitlab".into(),
                )
            })?;
        let endpoint = workflow.config.tracker.endpoint.clone();
        let token = workflow.config.tracker.api_key.clone().or(gitlab_token);
        return Ok((endpoint, project_path, token));
    }

    // Auto-detect from git remote
    if let Some((endpoint, project_path)) = polyphony_git::detect_gitlab_remote(workflow_root) {
        return Ok((endpoint, project_path, gitlab_token));
    }

    Ok((String::new(), String::new(), None))
}

#[cfg(feature = "github")]
fn build_github_pull_request_components(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_root: &Path,
    github_token: Option<String>,
) -> Result<
    (
        Option<Arc<dyn PullRequestEventSource>>,
        Option<Arc<dyn PullRequestManager>>,
        Option<Arc<dyn PullRequestCommenter>>,
    ),
    Error,
> {
    let integration =
        resolve_github_pull_request_integration(workflow, workflow_root, github_token)?;

    let pull_request_event_source = if workflow.config.review_events.pr_reviews.enabled {
        match integration.as_ref() {
            Some((repository, token)) => Some(Arc::new(
                polyphony_github::GithubPullRequestReviewEventSource::new(
                    repository.clone(),
                    token.clone(),
                )?,
            ) as Arc<dyn PullRequestEventSource>),
            None => None,
        }
    } else {
        None
    };

    let (pull_request_manager, pull_request_commenter) = match integration {
        Some((repository, token)) => {
            let commenter = Some(Arc::new(polyphony_github::GithubPullRequestCommenter::new(
                token.clone(),
            )) as Arc<dyn PullRequestCommenter>);
            let manager = workflow
                .config
                .automation
                .enabled
                .then_some(Arc::new(polyphony_github::GithubPullRequestManager::new(
                    repository, token,
                )?) as Arc<dyn PullRequestManager>);
            (manager, commenter)
        },
        None => (None, None),
    };

    Ok((
        pull_request_event_source,
        pull_request_manager,
        pull_request_commenter,
    ))
}

#[cfg(feature = "github")]
fn resolve_github_pull_request_integration(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_root: &Path,
    github_token: Option<String>,
) -> Result<Option<(String, String)>, Error> {
    if workflow.config.tracker.kind == TrackerKind::Github {
        let repository = workflow.config.tracker.repository.clone().ok_or_else(|| {
            Error::Config(
                "tracker.repository is required for github pull request integrations".into(),
            )
        })?;
        let token = workflow
            .config
            .tracker
            .api_key
            .clone()
            .or(github_token)
            .ok_or_else(|| {
                Error::Config(
                    "tracker.api_key is required for github pull request integrations".into(),
                )
            })?;
        return Ok(Some((repository, token)));
    }

    let Some(repository) = polyphony_git::detect_github_remote(workflow_root) else {
        return Ok(None);
    };
    let Some(token) = github_token else {
        return Ok(None);
    };
    Ok(Some((repository, token)))
}

#[cfg(feature = "github")]
fn github_token_from_env() -> Option<String> {
    env::var("GITHUB_TOKEN")
        .ok()
        .or_else(|| env::var("GH_TOKEN").ok())
}

#[cfg(all(test, feature = "github", feature = "beads"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn loaded_workflow(front_matter: &str) -> polyphony_workflow::LoadedWorkflow {
        let definition =
            polyphony_workflow::parse_workflow(&format!("{front_matter}---\nPrompt\n")).unwrap();
        let config = polyphony_workflow::ServiceConfig::from_workflow(&definition).unwrap();
        polyphony_workflow::LoadedWorkflow {
            definition,
            config,
            path: PathBuf::from("/tmp/WORKFLOW.md"),
            agent_prompts: HashMap::new(),
        }
    }

    fn workspace_repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn supplemental_github_remote_enables_pr_review_components_for_beads_repos() {
        let repo_root = workspace_repo_root();
        let workflow = loaded_workflow(
            r#"---
tracker:
  kind: beads
review_events:
  pr_reviews:
    enabled: true
"#,
        );

        let (pull_request_event_source, pull_request_manager, pull_request_commenter) =
            build_github_pull_request_components(&workflow, &repo_root, Some("test-token".into()))
                .unwrap();

        assert!(pull_request_event_source.is_some());
        assert!(pull_request_commenter.is_some());
        assert!(pull_request_manager.is_none());
    }

    #[test]
    fn supplemental_github_remote_without_token_skips_pr_review_components() {
        let repo_root = workspace_repo_root();
        let workflow = loaded_workflow(
            r#"---
tracker:
  kind: beads
review_events:
  pr_reviews:
    enabled: true
"#,
        );

        let (pull_request_event_source, pull_request_manager, pull_request_commenter) =
            build_github_pull_request_components(&workflow, &repo_root, None).unwrap();

        assert!(pull_request_event_source.is_none());
        assert!(pull_request_commenter.is_none());
        assert!(pull_request_manager.is_none());
    }
}
