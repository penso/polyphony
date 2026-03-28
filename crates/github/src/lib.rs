use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use graphql_client::GraphQLQuery;
use octocrab::{
    Octocrab,
    models::issues::{Comment as GithubComment, Issue as GithubIssue},
};
use polyphony_core::{
    AddIssueCommentRequest, BudgetSnapshot, CreateIssueRequest, Error as CoreError, Issue,
    IssueComment, IssueStateUpdate, IssueTracker, TrackerConnectionStatus, TrackerQuery,
    UpdateIssueRequest,
};
use reqwest::{StatusCode, header::HeaderMap};
use serde::{Deserialize, de::DeserializeOwned};
use thiserror::Error;
use tracing::{debug, info};

#[derive(Debug, Error)]
pub enum Error {
    #[error("github error: {0}")]
    Github(String),
}

mod convert;
mod prelude;
mod pull_requests;
mod review_events;

#[cfg(test)]
mod tests;

use crate::convert::*;
pub use crate::{
    pull_requests::{GithubPullRequestCommenter, GithubPullRequestManager},
    review_events::GithubPullRequestReviewEventSource,
};

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/comment_pr.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct ResolvePullRequestId;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/comment_pr.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct AddCommentToPullRequest;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/project_workflow.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct ResolveProjectIssueContext;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/project_workflow.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct ResolveProjectStatusField;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/project_workflow.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct AddIssueToProject;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/project_workflow.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct UpdateIssueProjectStatus;

mod github_graphql_scalars {
    pub type DateTime = chrono::DateTime<chrono::Utc>;
    pub type GitObjectID = String;
    #[allow(clippy::upper_case_acronyms)]
    pub type URI = String;
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/pull_request_events.graphql",
    custom_scalars_module = "crate::github_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct FetchPullRequestEvents;

#[derive(Debug)]
pub struct GithubIssueTracker {
    crab: Octocrab,
    http: reqwest::Client,
    token: Option<String>,
    owner: String,
    repo: String,
    project: Option<GithubProjectConfig>,
    request_count: AtomicU64,
    last_rate_limit: Mutex<Option<CapturedRateLimit>>,
    viewer_login: Mutex<ViewerLoginCache>,
}

#[derive(Debug, Clone)]
struct CapturedRateLimit {
    remaining: u64,
    limit: u64,
    reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct GithubProjectConfig {
    owner: String,
    number: u32,
    status_field_name: String,
}

#[derive(Debug, Clone, Default)]
enum ViewerLoginCache {
    #[default]
    Unknown,
    Unavailable,
    Known(String),
}

#[derive(Debug, Deserialize)]
struct GithubViewerResponse {
    login: String,
}

enum ViewerLoginFetch {
    RetryLater,
    Unavailable,
    Known(String),
}

impl GithubIssueTracker {
    pub fn new(
        repository: String,
        token: Option<String>,
        project_owner: Option<String>,
        project_number: Option<u32>,
        project_status_field: Option<String>,
    ) -> Result<Self, CoreError> {
        let (owner, repo) = split_repo(&repository)?;
        let mut builder = Octocrab::builder();
        if let Some(token) = token.clone() {
            builder = builder.personal_token(token);
        }
        let crab = builder
            .build()
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let project = project_number.map(|number| GithubProjectConfig {
            owner: project_owner.unwrap_or_else(|| owner.clone()),
            number,
            status_field_name: project_status_field.unwrap_or_else(|| "Status".into()),
        });
        Ok(Self {
            crab,
            http: reqwest::Client::new(),
            token,
            owner,
            repo,
            project,
            request_count: AtomicU64::new(0),
            last_rate_limit: Mutex::new(None),
            viewer_login: Mutex::new(ViewerLoginCache::Unknown),
        })
    }

    async fn all_issues(
        &self,
        state: octocrab::params::State,
    ) -> Result<Vec<GithubIssue>, CoreError> {
        self.track_request();
        let mut page = self
            .crab
            .issues(&self.owner, &self.repo)
            .list()
            .state(state)
            .per_page(100)
            .send()
            .await
            .map_err(map_github_error)?;
        let mut issues = page.take_items();
        while let Some(next) = self
            .crab
            .get_page::<GithubIssue>(&page.next)
            .await
            .map_err(map_github_error)?
        {
            self.track_request();
            page = next;
            issues.extend(page.take_items());
        }
        Ok(issues)
    }

    async fn issue_by_number(&self, number: u64) -> Result<GithubIssue, CoreError> {
        self.track_request();
        self.crab
            .issues(&self.owner, &self.repo)
            .get(number)
            .await
            .map_err(map_github_error)
    }

    async fn comments_for_issue(&self, number: u64) -> Result<Vec<GithubComment>, CoreError> {
        self.track_request();
        let mut page = self
            .crab
            .issues(&self.owner, &self.repo)
            .list_comments(number)
            .per_page(100)
            .send()
            .await
            .map_err(map_github_error)?;
        let mut comments = page.take_items();
        while let Some(next) = self
            .crab
            .get_page::<GithubComment>(&page.next)
            .await
            .map_err(map_github_error)?
        {
            self.track_request();
            page = next;
            comments.extend(page.take_items());
        }
        Ok(comments)
    }

    async fn normalize_issue(&self, issue: GithubIssue) -> Result<Issue, CoreError> {
        let comments = if issue.comments > 0 {
            self.comments_for_issue(issue.number).await?
        } else {
            Vec::new()
        };
        Ok(to_issue(issue, comments))
    }

    fn track_request(&self) {
        self.request_count.fetch_add(1, Ordering::Relaxed);
    }

    fn capture_rate_limit_headers(&self, headers: &HeaderMap) {
        let remaining = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let limit = headers
            .get("x-ratelimit-limit")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if let (Some(remaining), Some(limit)) = (remaining, limit) {
            let reset_at = parse_rate_limit_reset(headers);
            if let Ok(mut guard) = self.last_rate_limit.lock() {
                *guard = Some(CapturedRateLimit {
                    remaining,
                    limit,
                    reset_at,
                });
            }
        }
    }

    async fn connection_status(&self) -> TrackerConnectionStatus {
        let Some(token) = &self.token else {
            return TrackerConnectionStatus::disconnected("no token");
        };

        let cached = self
            .viewer_login
            .lock()
            .ok()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        match cached {
            ViewerLoginCache::Known(login) => return TrackerConnectionStatus::connected(login),
            ViewerLoginCache::Unavailable => {
                return TrackerConnectionStatus::disconnected("invalid token");
            },
            ViewerLoginCache::Unknown => {},
        }

        match self.fetch_viewer_login(token).await {
            ViewerLoginFetch::Known(login) => {
                if let Ok(mut guard) = self.viewer_login.lock() {
                    *guard = ViewerLoginCache::Known(login.clone());
                }
                TrackerConnectionStatus::connected(login)
            },
            ViewerLoginFetch::Unavailable => {
                if let Ok(mut guard) = self.viewer_login.lock() {
                    *guard = ViewerLoginCache::Unavailable;
                }
                TrackerConnectionStatus::disconnected("invalid token")
            },
            ViewerLoginFetch::RetryLater => TrackerConnectionStatus::unknown(),
        }
    }

    async fn fetch_viewer_login(&self, token: &str) -> ViewerLoginFetch {
        self.track_request();
        let response = match self
            .http
            .get("https://api.github.com/user")
            .bearer_auth(token)
            .header("User-Agent", "polyphony")
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return ViewerLoginFetch::RetryLater,
        };
        self.capture_rate_limit_headers(response.headers());
        if github_rate_limit_signal_from_response("tracker:github", &response).is_some() {
            return ViewerLoginFetch::RetryLater;
        }
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return ViewerLoginFetch::Unavailable;
        }
        if !status.is_success() {
            return ViewerLoginFetch::RetryLater;
        }
        match response.json::<GithubViewerResponse>().await {
            Ok(viewer) if !viewer.login.is_empty() => ViewerLoginFetch::Known(viewer.login),
            Ok(_) => ViewerLoginFetch::Unavailable,
            Err(_) => ViewerLoginFetch::RetryLater,
        }
    }

    async fn graphql<ResponseData, QueryBody>(
        &self,
        body: QueryBody,
    ) -> Result<graphql_client::Response<ResponseData>, CoreError>
    where
        ResponseData: DeserializeOwned,
        QueryBody: serde::Serialize,
    {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| CoreError::Adapter("github token is required".into()))?;
        let response = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(token)
            .header("User-Agent", "polyphony")
            .json(&body)
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        self.track_request();
        self.capture_rate_limit_headers(response.headers());
        if let Some(signal) = github_rate_limit_signal_from_response("tracker:github", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        let payload = response
            .json::<graphql_client::Response<ResponseData>>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github graphql status {}",
                status
            )));
        }
        if let Some(errors) = &payload.errors {
            return Err(CoreError::Adapter(format!(
                "github graphql errors: {errors:?}"
            )));
        }
        Ok(payload)
    }

    async fn project_context(&self, issue: &Issue) -> Result<Option<ProjectContext>, CoreError> {
        let Some(project) = &self.project else {
            return Ok(None);
        };
        let issue_number = issue
            .id
            .parse::<u64>()
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let response = self
            .graphql::<resolve_project_issue_context::ResponseData, _>(
                ResolveProjectIssueContext::build_query(resolve_project_issue_context::Variables {
                    owner: self.owner.clone(),
                    repo: self.repo.clone(),
                    number: issue_number as i64,
                    project_owner: project.owner.clone(),
                    project_number: project.number as i64,
                }),
            )
            .await?;
        let data = response
            .data
            .ok_or_else(|| CoreError::Adapter("github project context missing data".into()))?;
        let issue_node_id = data
            .repository
            .as_ref()
            .and_then(|repo| repo.issue.as_ref())
            .map(|issue| issue.id.clone())
            .ok_or_else(|| CoreError::Adapter("github issue node id not found".into()))?;
        let project_id = project_id_from_context(&data)
            .ok_or_else(|| CoreError::Adapter("github project id not found".into()))?;
        Ok(Some(ProjectContext {
            issue_node_id,
            project_id,
            status_field_name: project.status_field_name.clone(),
        }))
    }

    async fn ensure_project_item(
        &self,
        context: &ProjectContext,
    ) -> Result<Option<String>, CoreError> {
        let response = self
            .graphql::<add_issue_to_project::ResponseData, _>(AddIssueToProject::build_query(
                add_issue_to_project::Variables {
                    project_id: context.project_id.clone(),
                    content_id: context.issue_node_id.clone(),
                },
            ))
            .await?;
        let data = response
            .data
            .ok_or_else(|| CoreError::Adapter("github add project item missing data".into()))?;
        Ok(data
            .add_project_v2_item_by_id
            .and_then(|payload| payload.item)
            .map(|item| item.id))
    }

    async fn resolve_status_field(
        &self,
        project_id: &str,
        field_name: &str,
        status: &str,
    ) -> Result<(String, String), CoreError> {
        let response = self
            .graphql::<resolve_project_status_field::ResponseData, _>(
                ResolveProjectStatusField::build_query(resolve_project_status_field::Variables {
                    project_id: project_id.to_string(),
                }),
            )
            .await?;
        let data = response
            .data
            .ok_or_else(|| CoreError::Adapter("github project fields missing data".into()))?;
        let nodes = project_field_nodes(&data)
            .ok_or_else(|| CoreError::Adapter("github project fields not found".into()))?;
        let (field_id, option_id) = find_status_field_option(nodes, field_name, status)
            .ok_or_else(|| {
                CoreError::Adapter(format!(
                    "github project status option `{status}` not found in field `{field_name}`"
                ))
            })?;
        Ok((field_id, option_id))
    }
}

#[derive(Debug, Clone)]
struct ProjectContext {
    issue_node_id: String,
    project_id: String,
    status_field_name: String,
}

#[async_trait]
impl IssueTracker for GithubIssueTracker {
    fn component_key(&self) -> String {
        "tracker:github".into()
    }

    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        if !wants_open_states(&query.active_states) {
            return Ok(Vec::new());
        }
        debug!(
            repository = %format!("{}/{}", self.owner, self.repo),
            "fetching GitHub candidate issues"
        );
        let mut normalized = Vec::new();
        for issue in self.all_issues(octocrab::params::State::Open).await? {
            if issue.pull_request.is_none() {
                normalized.push(self.normalize_issue(issue).await?);
            }
        }
        debug!(
            repository = %format!("{}/{}", self.owner, self.repo),
            issues = normalized.len(),
            "fetched GitHub candidate issues"
        );
        Ok(normalized)
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        let mut issues_by_id = BTreeMap::new();
        let wants_open = wants_open_states(states);
        let wants_closed = wants_closed_states(states);
        if wants_open {
            for issue in self.all_issues(octocrab::params::State::Open).await? {
                if issue.pull_request.is_none() {
                    issues_by_id.insert(issue.number, self.normalize_issue(issue).await?);
                }
            }
        }
        if wants_closed {
            for issue in self.all_issues(octocrab::params::State::Closed).await? {
                if issue.pull_request.is_none() {
                    issues_by_id.insert(issue.number, self.normalize_issue(issue).await?);
                }
            }
        }
        Ok(issues_by_id.into_values().collect())
    }

    async fn fetch_issues_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>, CoreError> {
        debug!(
            repository = %format!("{}/{}", self.owner, self.repo),
            issue_count = issue_ids.len(),
            "fetching GitHub issues by id"
        );
        let mut issues = Vec::new();
        for issue_id in issue_ids {
            let number = issue_id
                .parse::<u64>()
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            let issue = self.issue_by_number(number).await?;
            if issue.pull_request.is_none() {
                issues.push(self.normalize_issue(issue).await?);
            }
        }
        Ok(issues)
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        debug!(
            repository = %format!("{}/{}", self.owner, self.repo),
            issue_count = issue_ids.len(),
            "fetching GitHub issue states by id"
        );
        let mut updates = Vec::new();
        for issue_id in issue_ids {
            let number = issue_id
                .parse::<u64>()
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            let issue = self.issue_by_number(number).await?;
            updates.push(IssueStateUpdate {
                id: issue.number.to_string(),
                identifier: format!("#{}", issue.number),
                state: normalize_issue_state(&issue),
                updated_at: Some(issue.updated_at.with_timezone(&Utc)),
            });
        }
        Ok(updates)
    }

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        let Some(token) = &self.token else {
            return Ok(None);
        };
        self.track_request();
        let response = self
            .http
            .get("https://api.github.com/rate_limit")
            .bearer_auth(token)
            .header("User-Agent", "polyphony")
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        self.capture_rate_limit_headers(response.headers());
        let captured = self
            .last_rate_limit
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let requests = self.request_count.load(Ordering::Relaxed);
        let (remaining, total, reset_at) = match captured {
            Some(rl) => (
                Some(rl.remaining as f64),
                Some(rl.limit as f64),
                rl.reset_at,
            ),
            None => (None, None, None),
        };
        let raw = serde_json::json!({ "requests": requests });
        Ok(Some(BudgetSnapshot {
            component: "tracker:github".into(),
            captured_at: Utc::now(),
            credits_remaining: remaining,
            credits_total: total,
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at,
            raw: Some(raw),
        }))
    }

    async fn fetch_connection_status(&self) -> Result<Option<TrackerConnectionStatus>, CoreError> {
        Ok(Some(self.connection_status().await))
    }

    async fn ensure_issue_workflow_tracking(&self, issue: &Issue) -> Result<(), CoreError> {
        info!(
            repository = %format!("{}/{}", self.owner, self.repo),
            issue_identifier = %issue.identifier,
            "ensuring GitHub workflow tracking"
        );
        let Some(context) = self.project_context(issue).await? else {
            return Ok(());
        };
        let _ = self.ensure_project_item(&context).await?;
        Ok(())
    }

    async fn update_issue_workflow_status(
        &self,
        issue: &Issue,
        status: &str,
    ) -> Result<(), CoreError> {
        info!(
            repository = %format!("{}/{}", self.owner, self.repo),
            issue_identifier = %issue.identifier,
            workflow_status = status,
            "updating GitHub workflow status"
        );
        let Some(context) = self.project_context(issue).await? else {
            return Ok(());
        };
        let Some(item_id) = self.ensure_project_item(&context).await? else {
            return Ok(());
        };
        let (field_id, option_id) = self
            .resolve_status_field(&context.project_id, &context.status_field_name, status)
            .await?;
        self.graphql::<update_issue_project_status::ResponseData, _>(
            UpdateIssueProjectStatus::build_query(update_issue_project_status::Variables {
                project_id: context.project_id.clone(),
                item_id,
                field_id,
                option_id,
            }),
        )
        .await?;
        Ok(())
    }

    async fn create_issue(&self, request: &CreateIssueRequest) -> Result<Issue, CoreError> {
        self.track_request();
        let issues = self.crab.issues(&self.owner, &self.repo);
        let mut builder = issues.create(&request.title);
        if let Some(ref desc) = request.description {
            builder = builder.body(desc);
        }
        if !request.labels.is_empty() {
            builder = builder.labels(request.labels.clone());
        }
        let created = builder.send().await.map_err(map_github_error)?;
        Ok(to_issue(created, Vec::new()))
    }

    async fn update_issue(&self, request: &UpdateIssueRequest) -> Result<Issue, CoreError> {
        let number = request
            .id
            .parse::<u64>()
            .map_err(|error| CoreError::Adapter(format!("invalid issue number: {error}")))?;
        self.track_request();
        let issues = self.crab.issues(&self.owner, &self.repo);
        let mut builder = issues.update(number);
        if let Some(ref title) = request.title {
            builder = builder.title(title);
        }
        if let Some(ref desc) = request.description {
            builder = builder.body(desc);
        }
        if let Some(ref state) = request.state {
            let gh_state = match state.to_ascii_lowercase().as_str() {
                "open" => octocrab::models::IssueState::Open,
                "closed" => octocrab::models::IssueState::Closed,
                _ => {
                    return Err(CoreError::Adapter(format!(
                        "unsupported GitHub issue state: {state}"
                    )));
                },
            };
            builder = builder.state(gh_state);
        }
        if let Some(ref labels) = request.labels {
            builder = builder.labels(labels);
        }
        let updated = builder.send().await.map_err(map_github_error)?;
        Ok(to_issue(updated, Vec::new()))
    }

    async fn comment_on_issue(
        &self,
        request: &AddIssueCommentRequest,
    ) -> Result<IssueComment, CoreError> {
        let number = request
            .id
            .parse::<u64>()
            .map_err(|error| CoreError::Adapter(format!("invalid issue number: {error}")))?;
        self.track_request();
        let comment = self
            .crab
            .issues(&self.owner, &self.repo)
            .create_comment(number, &request.body)
            .await
            .map_err(map_github_error)?;
        Ok(github_comment(comment))
    }

    async fn acknowledge_issue(&self, issue: &Issue) -> Result<(), CoreError> {
        let number = issue
            .id
            .parse::<u64>()
            .map_err(|error| CoreError::Adapter(format!("invalid issue number: {error}")))?;
        let Some(token) = &self.token else {
            return Ok(());
        };
        self.track_request();
        let response = self
            .http
            .post(format!(
                "https://api.github.com/repos/{}/{}/issues/{number}/reactions",
                self.owner, self.repo,
            ))
            .bearer_auth(token)
            .header("User-Agent", "polyphony")
            .header("Accept", "application/vnd.github+json")
            .json(&serde_json::json!({ "content": "eyes" }))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:reactions", &response)
        {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        // 200 = reaction already existed, 201 = newly created — both are fine.
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github add reaction failed with status {status}"
            )));
        }
        info!(
            issue_identifier = %issue.identifier,
            "added eyes reaction to acknowledge issue"
        );
        Ok(())
    }

    async fn fetch_pull_request_state(
        &self,
        _repository: &str,
        number: u64,
    ) -> Result<Option<String>, CoreError> {
        self.track_request();
        let response = self
            .http
            .get(format!(
                "https://api.github.com/repos/{}/{}/pulls/{number}",
                self.owner, self.repo,
            ))
            .bearer_auth(self.token.as_deref().unwrap_or(""))
            .header("User-Agent", "polyphony")
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:pulls", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github fetch PR state failed with status {status}"
            )));
        }
        #[derive(Deserialize)]
        struct PrState {
            state: String,
            merged: bool,
        }
        let pr = response
            .json::<PrState>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if pr.merged {
            Ok(Some("merged".into()))
        } else {
            Ok(Some(pr.state))
        }
    }
}
