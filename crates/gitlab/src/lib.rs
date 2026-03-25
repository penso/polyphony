use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use chrono::Utc;
use graphql_client::GraphQLQuery;
use polyphony_core::{
    AddIssueCommentRequest, BudgetSnapshot, CreateIssueRequest, Error as CoreError, Issue,
    IssueComment, IssueStateUpdate, IssueTracker, TrackerConnectionState, TrackerConnectionStatus,
    TrackerQuery, UpdateIssueRequest,
};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

mod convert;
pub mod merge_requests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

use crate::convert::*;

mod gitlab_graphql_scalars {
    pub type Time = String;
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/issues.graphql",
    custom_scalars_module = "crate::gitlab_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize, Clone"
)]
pub struct FetchProjectIssues;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/issues.graphql",
    custom_scalars_module = "crate::gitlab_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize, Clone"
)]
pub struct FetchIssueByIid;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/issues.graphql",
    custom_scalars_module = "crate::gitlab_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize, Clone"
)]
pub struct FetchCurrentUser;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/merge_requests.graphql",
    custom_scalars_module = "crate::gitlab_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize, Clone"
)]
pub struct FetchOpenMergeRequests;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/merge_requests.graphql",
    custom_scalars_module = "crate::gitlab_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize, Clone"
)]
pub struct FetchMergeRequestByIid;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/merge_requests.graphql",
    custom_scalars_module = "crate::gitlab_graphql_scalars",
    response_derives = "Debug, Serialize, Deserialize, Clone"
)]
pub struct CreateMergeRequestNote;

#[derive(Debug)]
pub struct GitlabIssueTracker {
    http: reqwest::Client,
    graphql_url: String,
    rest_base_url: String,
    token: Option<String>,
    project_path: String,
    encoded_project_path: String,
    request_count: AtomicU64,
    last_rate_limit: Mutex<Option<convert::CapturedRateLimit>>,
    viewer_cache: Mutex<ViewerCache>,
}

#[derive(Debug, Clone, Default)]
enum ViewerCache {
    #[default]
    Unknown,
    Unavailable,
    Known(String),
}

impl GitlabIssueTracker {
    pub fn new(
        endpoint: String,
        token: Option<String>,
        project_path: String,
    ) -> Result<Self, CoreError> {
        if project_path.is_empty() {
            return Err(CoreError::Adapter(
                "gitlab project path is required (e.g. 'namespace/project')".into(),
            ));
        }
        let base = if endpoint.is_empty() {
            "https://gitlab.com"
        } else {
            endpoint.trim_end_matches('/')
        };
        let graphql_url = format!("{base}/api/graphql");
        let encoded = urlencoding::encode(&project_path).into_owned();
        let rest_base_url = format!("{base}/api/v4");

        Ok(Self {
            http: reqwest::Client::new(),
            graphql_url,
            rest_base_url,
            token,
            encoded_project_path: encoded,
            project_path,
            request_count: AtomicU64::new(0),
            last_rate_limit: Mutex::new(None),
            viewer_cache: Mutex::new(ViewerCache::Unknown),
        })
    }

    async fn graphql<R: serde::de::DeserializeOwned>(
        &self,
        body: &graphql_client::QueryBody<impl serde::Serialize>,
    ) -> Result<(R, HeaderMap), CoreError> {
        self.track_request();
        let mut request = self.http.post(&self.graphql_url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .json(body)
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        if let Some(signal) = gitlab_rate_limit_signal_from_response("tracker:gitlab", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }

        let headers = response.headers().clone();
        if let Some(captured) = capture_rate_limit_headers(&headers)
            && let Ok(mut guard) = self.last_rate_limit.lock()
        {
            *guard = Some(captured);
        }

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(CoreError::Adapter(format!(
                "gitlab graphql {status}: {body_text}"
            )));
        }

        #[derive(Deserialize)]
        struct GraphQlResponse<T> {
            data: Option<T>,
            errors: Option<Vec<GraphQlError>>,
        }
        #[derive(Deserialize)]
        struct GraphQlError {
            message: String,
        }

        let parsed: GraphQlResponse<R> = response
            .json()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        if let Some(errors) = parsed.errors
            && !errors.is_empty()
        {
            let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
            return Err(CoreError::Adapter(format!(
                "gitlab graphql errors: {}",
                messages.join("; ")
            )));
        }

        parsed
            .data
            .map(|data| (data, headers))
            .ok_or_else(|| CoreError::Adapter("gitlab graphql returned no data".into()))
    }

    async fn fetch_all_issues(&self, opened: bool) -> Result<Vec<Issue>, CoreError> {
        let mut all_issues = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let state = if opened {
                Some(fetch_project_issues::IssuableSearchableField::opened)
            } else {
                Some(fetch_project_issues::IssuableSearchableField::closed)
            };
            let variables = fetch_project_issues::Variables {
                full_path: self.project_path.clone(),
                state,
                after: cursor.clone(),
            };
            let body = FetchProjectIssues::build_query(variables);
            let (data, _): (fetch_project_issues::ResponseData, _) = self.graphql(&body).await?;

            let project = data.project.ok_or_else(|| {
                CoreError::Adapter(format!("gitlab project '{}' not found", self.project_path))
            })?;
            let issues_conn = project
                .issues
                .ok_or_else(|| CoreError::Adapter("gitlab returned no issues connection".into()))?;

            if let Some(nodes) = &issues_conn.nodes {
                for node in nodes {
                    all_issues.push(list_node_to_issue(node, &self.project_path));
                }
            }

            if issues_conn.page_info.has_next_page {
                cursor = issues_conn.page_info.end_cursor;
            } else {
                break;
            }
        }
        Ok(all_issues)
    }

    async fn fetch_issue_with_notes(&self, iid: &str) -> Result<Issue, CoreError> {
        let variables = fetch_issue_by_iid::Variables {
            full_path: self.project_path.clone(),
            iid: iid.to_string(),
        };
        let body = FetchIssueByIid::build_query(variables);
        let (data, _): (fetch_issue_by_iid::ResponseData, _) = self.graphql(&body).await?;

        let project = data.project.ok_or_else(|| {
            CoreError::Adapter(format!("gitlab project '{}' not found", self.project_path))
        })?;
        let issue = project
            .issue
            .ok_or_else(|| CoreError::Adapter(format!("gitlab issue #{iid} not found")))?;
        Ok(to_issue(&issue, &self.project_path))
    }

    fn track_request(&self) {
        self.request_count.fetch_add(1, Ordering::Relaxed);
    }

    fn rest_url(&self, path: &str) -> String {
        format!(
            "{}/projects/{}/{}",
            self.rest_base_url, self.encoded_project_path, path
        )
    }

    async fn rest_request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, CoreError> {
        self.track_request();
        let mut req = self.http.request(method, self.rest_url(path));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        Ok(req)
    }

    async fn rest_json<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, CoreError> {
        let mut req = self.rest_request(method, path).await?;
        if let Some(body) = body {
            req = req.json(body);
        }
        let response = req
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        if let Some(signal) = gitlab_rate_limit_signal_from_response("tracker:gitlab", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(CoreError::Adapter(format!(
                "gitlab rest {status}: {body_text}"
            )));
        }
        response
            .json()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn project_path(&self) -> &str {
        &self.project_path
    }
}

#[async_trait]
impl IssueTracker for GitlabIssueTracker {
    fn component_key(&self) -> String {
        "tracker:gitlab".into()
    }

    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        let mut issues = Vec::new();
        if wants_open_states(&query.active_states) {
            issues.extend(self.fetch_all_issues(true).await?);
        }
        if wants_closed_states(&query.active_states) {
            issues.extend(self.fetch_all_issues(false).await?);
        }
        let active_lower: Vec<String> = query
            .active_states
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        issues.retain(|issue| active_lower.contains(&issue.state.to_ascii_lowercase()));
        Ok(issues)
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let mut issues = Vec::new();
        if wants_open_states(states) {
            issues.extend(self.fetch_all_issues(true).await?);
        }
        if wants_closed_states(states) {
            issues.extend(self.fetch_all_issues(false).await?);
        }
        let states_lower: Vec<String> = states.iter().map(|s| s.to_ascii_lowercase()).collect();
        issues.retain(|issue| states_lower.contains(&issue.state.to_ascii_lowercase()));
        Ok(issues)
    }

    async fn fetch_issues_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>, CoreError> {
        let mut issues = Vec::new();
        for iid in issue_ids {
            match self.fetch_issue_with_notes(iid).await {
                Ok(issue) => issues.push(issue),
                Err(e) => {
                    debug!(iid, error = %e, "failed to fetch gitlab issue, skipping");
                },
            }
        }
        Ok(issues)
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        let issues = self.fetch_issues_by_ids(issue_ids).await?;
        Ok(issues.iter().map(issue_to_state_update).collect())
    }

    async fn create_issue(&self, request: &CreateIssueRequest) -> Result<Issue, CoreError> {
        #[derive(Serialize)]
        struct Body {
            title: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            labels: Option<String>,
        }
        let labels = if request.labels.is_empty() {
            None
        } else {
            Some(request.labels.join(","))
        };
        let body = Body {
            title: request.title.clone(),
            description: request.description.clone(),
            labels,
        };
        let created: serde_json::Value = self
            .rest_json(reqwest::Method::POST, "issues", Some(&body))
            .await?;
        let iid = created["iid"]
            .as_u64()
            .ok_or_else(|| CoreError::Adapter("missing iid in response".into()))?
            .to_string();
        self.fetch_issue_with_notes(&iid).await
    }

    async fn update_issue(&self, request: &UpdateIssueRequest) -> Result<Issue, CoreError> {
        #[derive(Serialize)]
        struct Body {
            #[serde(skip_serializing_if = "Option::is_none")]
            title: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            state_event: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            labels: Option<String>,
        }
        let state_event = request.state.as_ref().map(|s| {
            if is_terminalish_state(s) {
                "close".to_string()
            } else {
                "reopen".to_string()
            }
        });
        let labels = request.labels.as_ref().map(|l| l.join(","));
        let body = Body {
            title: request.title.clone(),
            description: request.description.clone(),
            state_event,
            labels,
        };
        let path = format!("issues/{}", request.id);
        let _: serde_json::Value = self
            .rest_json(reqwest::Method::PUT, &path, Some(&body))
            .await?;
        self.fetch_issue_with_notes(&request.id).await
    }

    async fn comment_on_issue(
        &self,
        request: &AddIssueCommentRequest,
    ) -> Result<IssueComment, CoreError> {
        #[derive(Serialize)]
        struct Body {
            body: String,
        }
        #[derive(Deserialize)]
        struct NoteResponse {
            id: u64,
            body: String,
            created_at: String,
            updated_at: String,
        }
        let path = format!("issues/{}/notes", request.id);
        let note: NoteResponse = self
            .rest_json(
                reqwest::Method::POST,
                &path,
                Some(&Body {
                    body: request.body.clone(),
                }),
            )
            .await?;
        Ok(IssueComment {
            id: note.id.to_string(),
            body: note.body,
            author: None,
            url: None,
            created_at: parse_gitlab_time(&note.created_at),
            updated_at: parse_gitlab_time(&note.updated_at),
        })
    }

    async fn fetch_pull_request_state(
        &self,
        _repository: &str,
        number: u64,
    ) -> Result<Option<String>, CoreError> {
        let variables = fetch_merge_request_by_iid::Variables {
            full_path: self.project_path.clone(),
            iid: number.to_string(),
        };
        let body = FetchMergeRequestByIid::build_query(variables);
        let result = self
            .graphql::<fetch_merge_request_by_iid::ResponseData>(&body)
            .await;
        match result {
            Ok((data, _)) => {
                let state = data
                    .project
                    .and_then(|p| p.merge_request)
                    .map(|mr| mr.state);
                Ok(state)
            },
            Err(e) => {
                debug!(number, error = %e, "failed to fetch gitlab MR state");
                Ok(None)
            },
        }
    }

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        let guard = self
            .last_rate_limit
            .lock()
            .map_err(|_| CoreError::Adapter("rate limit lock poisoned".into()))?;
        let Some(captured) = guard.as_ref() else {
            return Ok(None);
        };
        Ok(Some(BudgetSnapshot {
            component: self.component_key(),
            captured_at: Utc::now(),
            credits_remaining: Some(captured.remaining as f64),
            credits_total: Some(captured.limit as f64),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: captured.reset_at,
            raw: None,
        }))
    }

    async fn fetch_connection_status(&self) -> Result<Option<TrackerConnectionStatus>, CoreError> {
        {
            let guard = self
                .viewer_cache
                .lock()
                .map_err(|_| CoreError::Adapter("viewer cache lock poisoned".into()))?;
            match &*guard {
                ViewerCache::Known(username) => {
                    return Ok(Some(TrackerConnectionStatus {
                        state: TrackerConnectionState::Connected,
                        label: Some(username.clone()),
                        detail: None,
                    }));
                },
                ViewerCache::Unavailable => {
                    return Ok(Some(TrackerConnectionStatus {
                        state: TrackerConnectionState::Disconnected,
                        label: None,
                        detail: Some("invalid or missing token".into()),
                    }));
                },
                ViewerCache::Unknown => {},
            }
        }

        let body = FetchCurrentUser::build_query(fetch_current_user::Variables);
        match self
            .graphql::<fetch_current_user::ResponseData>(&body)
            .await
        {
            Ok((data, _)) => {
                if let Some(user) = data.current_user {
                    info!(username = %user.username, "gitlab connection verified");
                    let username = user.username.clone();
                    if let Ok(mut guard) = self.viewer_cache.lock() {
                        *guard = ViewerCache::Known(username.clone());
                    }
                    Ok(Some(TrackerConnectionStatus {
                        state: TrackerConnectionState::Connected,
                        label: Some(username),
                        detail: None,
                    }))
                } else {
                    if let Ok(mut guard) = self.viewer_cache.lock() {
                        *guard = ViewerCache::Unavailable;
                    }
                    Ok(Some(TrackerConnectionStatus {
                        state: TrackerConnectionState::Disconnected,
                        label: None,
                        detail: Some("no authenticated user".into()),
                    }))
                }
            },
            Err(e) => {
                debug!(error = %e, "gitlab connection check failed");
                Ok(Some(TrackerConnectionStatus {
                    state: TrackerConnectionState::Disconnected,
                    label: None,
                    detail: Some(e.to_string()),
                }))
            },
        }
    }
}
