use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use {
    async_trait::async_trait,
    chrono::{DateTime, Utc},
    graphql_client::GraphQLQuery,
    octocrab::{
        Octocrab,
        models::{
            Author, AuthorAssociation,
            issues::{Comment as GithubComment, Issue as GithubIssue},
        },
    },
    polyphony_core::{
        BudgetSnapshot, Error as CoreError, Issue, IssueAuthor, IssueComment, IssueStateUpdate,
        IssueTracker, PullRequestCommenter, PullRequestManager, PullRequestRef, PullRequestRequest,
        RateLimitSignal, TrackerQuery,
    },
    reqwest::{
        Response, StatusCode,
        header::{HeaderMap, RETRY_AFTER},
    },
    serde::{Deserialize, Serialize, de::DeserializeOwned},
    thiserror::Error,
    tracing::{debug, info},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("github error: {0}")]
    Github(String),
}

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
}

#[derive(Debug, Clone)]
pub struct GithubPullRequestCommenter {
    client: reqwest::Client,
    token: String,
}

impl GithubPullRequestCommenter {
    pub fn new(token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            token,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GithubPullRequestManager {
    client: reqwest::Client,
    token: String,
    owner: String,
    repo: String,
}

impl GithubPullRequestManager {
    pub fn new(repository: String, token: String) -> Result<Self, CoreError> {
        let (owner, repo) = split_repo(&repository)?;
        Ok(Self {
            client: reqwest::Client::new(),
            token,
            owner,
            repo,
        })
    }

    async fn existing_pull_request(
        &self,
        head_branch: &str,
    ) -> Result<Option<PullRequestRef>, CoreError> {
        let head = format!("{}:{head_branch}", self.owner);
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls",
            self.owner, self.repo
        );
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .query(&[("state", "open"), ("head", head.as_str())])
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:pulls", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github existing pull request lookup failed with status {status}"
            )));
        }
        let pulls = response
            .json::<Vec<GithubPullRequestResponse>>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        Ok(pulls.into_iter().next().map(|pull| PullRequestRef {
            repository: format!("{}/{}", self.owner, self.repo),
            number: pull.number,
            url: Some(pull.html_url),
        }))
    }
}

#[async_trait]
impl PullRequestCommenter for GithubPullRequestCommenter {
    fn component_key(&self) -> String {
        "github:graphql".into()
    }

    async fn comment_on_pull_request(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
    ) -> Result<(), CoreError> {
        info!(
            repository = %pull_request.repository,
            pull_request_number = pull_request.number,
            comment_len = body.len(),
            "posting GitHub pull request comment"
        );
        let (owner, name) = split_repo(&pull_request.repository)?;
        let number = i64::try_from(pull_request.number)
            .map_err(|error| CoreError::Adapter(error.to_string()))?;

        let response = self
            .client
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&ResolvePullRequestId::build_query(
                resolve_pull_request_id::Variables {
                    owner: owner.clone(),
                    name: name.clone(),
                    number,
                },
            ))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let payload: graphql_client::Response<resolve_pull_request_id::ResponseData> = response
            .json()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let subject_id = payload
            .data
            .and_then(|data| data.repository)
            .and_then(|repo| repo.pull_request)
            .map(|pr| pr.id)
            .ok_or_else(|| CoreError::Adapter("pull request node id not found".into()))?;

        let response = self
            .client
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&AddCommentToPullRequest::build_query(
                add_comment_to_pull_request::Variables {
                    subject_id,
                    body: body.to_string(),
                },
            ))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let payload: graphql_client::Response<add_comment_to_pull_request::ResponseData> = response
            .json()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if payload.errors.is_some() {
            return Err(CoreError::Adapter(
                "github graphql comment mutation failed".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl PullRequestManager for GithubPullRequestManager {
    fn component_key(&self) -> String {
        "github:pulls".into()
    }

    async fn ensure_pull_request(
        &self,
        request: &PullRequestRequest,
    ) -> Result<PullRequestRef, CoreError> {
        info!(
            repository = %request.repository,
            head_branch = %request.head_branch,
            base_branch = %request.base_branch,
            draft = request.draft,
            "ensuring GitHub pull request"
        );
        let (owner, repo) = split_repo(&request.repository)?;
        if owner != self.owner || repo != self.repo {
            return Err(CoreError::Adapter(format!(
                "pull request repository mismatch: expected {}/{} got {}",
                self.owner, self.repo, request.repository
            )));
        }
        if let Some(existing) = self.existing_pull_request(&request.head_branch).await? {
            debug!(
                repository = %request.repository,
                head_branch = %request.head_branch,
                pull_request_number = existing.number,
                "reusing existing GitHub pull request"
            );
            return Ok(existing);
        }

        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls",
            self.owner, self.repo
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&CreatePullRequestBody {
                title: request.title.clone(),
                head: request.head_branch.clone(),
                base: request.base_branch.clone(),
                body: request.body.clone(),
                draft: request.draft,
            })
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:pulls", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github create pull request failed with status {status}"
            )));
        }
        let pull = response
            .json::<GithubPullRequestResponse>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        info!(
            repository = %request.repository,
            pull_request_number = pull.number,
            "created GitHub pull request"
        );
        Ok(PullRequestRef {
            repository: request.repository.clone(),
            number: pull.number,
            url: Some(pull.html_url),
        })
    }

    async fn merge_pull_request(&self, pull_request: &PullRequestRef) -> Result<(), CoreError> {
        info!(
            repository = %pull_request.repository,
            pull_request_number = pull_request.number,
            "merging GitHub pull request"
        );
        let (owner, repo) = split_repo(&pull_request.repository)?;
        let url = format!(
            "https://api.github.com/repos/{owner}/{repo}/pulls/{}/merge",
            pull_request.number
        );
        let response = self
            .client
            .put(url)
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&serde_json::json!({ "merge_method": "squash" }))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:pulls", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github merge pull request failed with status {status}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct GithubPullRequestResponse {
    number: u64,
    html_url: String,
}

#[derive(Debug, Serialize)]
struct CreatePullRequestBody {
    title: String,
    head: String,
    base: String,
    body: String,
    draft: bool,
}

fn to_issue(issue: GithubIssue, comments: Vec<GithubComment>) -> Issue {
    let state = normalize_issue_state(&issue);
    Issue {
        id: issue.number.to_string(),
        identifier: format!("#{}", issue.number),
        title: issue.title,
        description: issue.body,
        priority: None,
        state,
        branch_name: Some(format!("issue-{}", issue.number)),
        url: Some(issue.html_url.to_string()),
        author: Some(github_author(
            &issue.user,
            issue.author_association.as_ref(),
        )),
        labels: issue
            .labels
            .into_iter()
            .map(|label| label.name.to_ascii_lowercase())
            .collect(),
        comments: comments.into_iter().map(github_comment).collect(),
        blocked_by: Vec::new(),
        created_at: Some(issue.created_at.with_timezone(&Utc)),
        updated_at: Some(issue.updated_at.with_timezone(&Utc)),
    }
}

fn github_comment(comment: GithubComment) -> IssueComment {
    IssueComment {
        id: comment.id.to_string(),
        body: comment.body.unwrap_or_default(),
        author: Some(github_author(
            &comment.user,
            comment.author_association.as_ref(),
        )),
        url: Some(comment.html_url.to_string()),
        created_at: Some(comment.created_at.with_timezone(&Utc)),
        updated_at: comment.updated_at.map(|value| value.with_timezone(&Utc)),
    }
}

fn github_author(author: &Author, association: Option<&AuthorAssociation>) -> IssueAuthor {
    IssueAuthor {
        id: Some(author.id.to_string()),
        username: Some(author.login.clone()),
        display_name: author.name.clone().or_else(|| Some(author.login.clone())),
        role: association.map(github_role),
        trust_level: association.map(github_trust_level),
        url: Some(author.html_url.to_string()),
    }
}

fn github_role(association: &AuthorAssociation) -> String {
    match association {
        AuthorAssociation::Owner => "owner".into(),
        AuthorAssociation::Member => "member".into(),
        AuthorAssociation::Collaborator => "collaborator".into(),
        AuthorAssociation::Contributor => "contributor".into(),
        AuthorAssociation::FirstTimer => "first_timer".into(),
        AuthorAssociation::FirstTimeContributor => "first_time_contributor".into(),
        AuthorAssociation::Mannequin => "mannequin".into(),
        AuthorAssociation::None => "none".into(),
        AuthorAssociation::Other(value) => value.to_ascii_lowercase(),
        _ => "unknown".into(),
    }
}

fn github_trust_level(association: &AuthorAssociation) -> String {
    match association {
        AuthorAssociation::Owner => "trusted_owner".into(),
        AuthorAssociation::Member => "trusted_member".into(),
        AuthorAssociation::Collaborator => "trusted_collaborator".into(),
        AuthorAssociation::Contributor => "external_contributor".into(),
        AuthorAssociation::FirstTimer => "outsider".into(),
        AuthorAssociation::FirstTimeContributor => "outsider".into(),
        AuthorAssociation::Mannequin => "unknown".into(),
        AuthorAssociation::None => "outsider".into(),
        AuthorAssociation::Other(_) => "unknown".into(),
        _ => "unknown".into(),
    }
}

fn normalize_issue_state(issue: &GithubIssue) -> String {
    if issue.state == octocrab::models::IssueState::Open {
        "Todo".into()
    } else {
        "Done".into()
    }
}

fn wants_open_states(states: &[String]) -> bool {
    states.iter().any(|state| !is_terminalish_state(state))
}

fn wants_closed_states(states: &[String]) -> bool {
    states.iter().any(|state| is_terminalish_state(state))
}

fn is_terminalish_state(state: &str) -> bool {
    matches!(
        state.to_ascii_lowercase().as_str(),
        "done" | "closed" | "cancelled" | "canceled" | "duplicate"
    )
}

fn split_repo(repository: &str) -> Result<(String, String), CoreError> {
    let mut parts = repository.split('/');
    let owner = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::Adapter("invalid repository slug".into()))?;
    let repo = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::Adapter("invalid repository slug".into()))?;
    Ok((owner.to_string(), repo.to_string()))
}

fn project_id_from_context(data: &resolve_project_issue_context::ResponseData) -> Option<String> {
    data.organization
        .as_ref()
        .and_then(|org| org.project_v2.as_ref())
        .map(|project| project.id.clone())
        .or_else(|| {
            data.user
                .as_ref()
                .and_then(|user| user.project_v2.as_ref())
                .map(|project| project.id.clone())
        })
}

fn project_field_nodes(
    data: &resolve_project_status_field::ResponseData,
) -> Option<
    &[Vec<
        Option<resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes>,
    >],
> {
    match data.node.as_ref()? {
        resolve_project_status_field::ResolveProjectStatusFieldNode::ProjectV2(project) => {
            Some(project.fields.nodes.as_slice())
        },
        _ => None,
    }
}

fn find_status_field_option(
    nodes: &[Vec<
        Option<resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes>,
    >],
    field_name: &str,
    status: &str,
) -> Option<(String, String)> {
    for group in nodes {
        for node in group {
            let Some(node) = node else {
                continue;
            };
            match node {
                resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes::ProjectV2SingleSelectField(field) => {
                    if !field.name.eq_ignore_ascii_case(field_name) {
                        continue;
                    }
                    for option in &field.options {
                        if option.name.eq_ignore_ascii_case(status) {
                            return Some((field.id.clone(), option.id.clone()));
                        }
                    }
                }
                resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes::ProjectV2Field(_)
                | resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes::ProjectV2IterationField(_) => {}
            }
        }
    }
    None
}

fn map_github_error(error: octocrab::Error) -> CoreError {
    if let octocrab::Error::GitHub { source, .. } = &error
        && (source.status_code.as_u16() == 403 || source.status_code.as_u16() == 429)
    {
        return CoreError::RateLimited(Box::new(RateLimitSignal {
            component: "tracker:github".into(),
            reason: github_rate_limit_reason(source.status_code, Some(source.message.as_str())),
            limited_at: Utc::now(),
            retry_after_ms: source
                .message
                .to_ascii_lowercase()
                .contains("secondary rate limit")
                .then_some(60_000),
            reset_at: None,
            status_code: Some(source.status_code.as_u16()),
            raw: None,
        }));
    }
    CoreError::Adapter(error.to_string())
}

fn github_rate_limit_signal_from_response(
    component: &str,
    response: &Response,
) -> Option<RateLimitSignal> {
    github_rate_limit_signal(component, response.status(), response.headers(), None)
}

fn github_rate_limit_signal(
    component: &str,
    status: StatusCode,
    headers: &HeaderMap,
    message: Option<&str>,
) -> Option<RateLimitSignal> {
    if status.as_u16() != 403 && status.as_u16() != 429 {
        return None;
    }

    let retry_after_ms = parse_retry_after_ms(headers)
        .or_else(|| (!is_primary_rate_limit(headers)).then_some(60_000));
    Some(RateLimitSignal {
        component: component.into(),
        reason: github_rate_limit_reason(status, message),
        limited_at: Utc::now(),
        retry_after_ms,
        reset_at: parse_rate_limit_reset(headers),
        status_code: Some(status.as_u16()),
        raw: None,
    })
}

fn github_rate_limit_reason(status: StatusCode, message: Option<&str>) -> String {
    match message.map(str::trim).filter(|message| !message.is_empty()) {
        Some(message) => format!("github api {status}: {message}"),
        None => format!("github api {status}"),
    }
}

fn parse_retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1_000))
}

fn parse_rate_limit_reset(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let reset_epoch = headers
        .get("x-ratelimit-reset")?
        .to_str()
        .ok()?
        .parse::<i64>()
        .ok()?;
    DateTime::from_timestamp(reset_epoch, 0)
}

fn is_primary_rate_limit(headers: &HeaderMap) -> bool {
    headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == "0")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use {
        super::{
            StatusCode, find_status_field_option, github_rate_limit_signal, parse_rate_limit_reset,
            parse_retry_after_ms, project_id_from_context, resolve_project_issue_context,
            resolve_project_status_field,
        },
        chrono::{TimeZone, Utc},
        reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER},
    };

    #[test]
    fn project_id_prefers_org_then_user() {
        let data = resolve_project_issue_context::ResponseData {
            repository: None,
            organization: Some(resolve_project_issue_context::ResolveProjectIssueContextOrganization {
                project_v2: Some(resolve_project_issue_context::ResolveProjectIssueContextOrganizationProjectV2 {
                    id: "ORG_PROJECT".into(),
                }),
            }),
            user: Some(resolve_project_issue_context::ResolveProjectIssueContextUser {
                project_v2: Some(resolve_project_issue_context::ResolveProjectIssueContextUserProjectV2 {
                    id: "USER_PROJECT".into(),
                }),
            }),
        };
        assert_eq!(
            project_id_from_context(&data).as_deref(),
            Some("ORG_PROJECT")
        );
    }

    #[test]
    fn finds_status_option_case_insensitively() {
        let nodes = vec![vec![Some(
            resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes::ProjectV2SingleSelectField(
                resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectField {
                    id: "field-1".into(),
                    name: "Status".into(),
                    options: vec![
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-1".into(),
                            name: "Todo".into(),
                        },
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-2".into(),
                            name: "In Progress".into(),
                        },
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-3".into(),
                            name: "Human Review".into(),
                        },
                    ],
                },
            ),
        )]];

        assert_eq!(
            find_status_field_option(&nodes, "status", "human review"),
            Some(("field-1".into(), "opt-3".into()))
        );
    }

    #[test]
    fn retry_after_header_is_converted_to_milliseconds() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));

        assert_eq!(parse_retry_after_ms(&headers), Some(12_000));
    }

    #[test]
    fn reset_header_is_converted_to_utc_timestamp() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1710000000"));

        assert_eq!(
            parse_rate_limit_reset(&headers),
            Utc.timestamp_opt(1_710_000_000, 0).single()
        );
    }

    #[test]
    fn secondary_rate_limit_without_headers_falls_back_to_one_minute() {
        let signal = github_rate_limit_signal(
            "tracker:github",
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            None,
        )
        .unwrap();

        assert_eq!(signal.retry_after_ms, Some(60_000));
        assert!(signal.reset_at.is_none());
    }

    #[test]
    fn primary_rate_limit_uses_reset_header_instead_of_guessing_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1710000000"));

        let signal =
            github_rate_limit_signal("tracker:github", StatusCode::FORBIDDEN, &headers, None)
                .unwrap();

        assert_eq!(signal.retry_after_ms, None);
        assert_eq!(
            signal.reset_at,
            Utc.timestamp_opt(1_710_000_000, 0).single()
        );
    }
}
