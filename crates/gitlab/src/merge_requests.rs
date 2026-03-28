use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use graphql_client::GraphQLQuery;
use polyphony_core::{
    DispatchApprovalState, Error as CoreError, PullRequestCommenter, PullRequestEvent,
    PullRequestEventSource, PullRequestManager, PullRequestRef, PullRequestRequest,
    PullRequestReviewComment, PullRequestReviewEvent, ReviewProviderKind, ReviewVerdict,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::{
    FetchMergeRequestByIid, FetchOpenMergeRequests, convert::*, fetch_merge_request_by_iid,
    fetch_open_merge_requests,
};

// ---------------------------------------------------------------------------
// Shared GitLab MR client
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct GitlabMergeRequestClient {
    http: reqwest::Client,
    graphql_url: String,
    rest_base_url: String,
    token: String,
    project_path: String,
    encoded_project_path: String,
    request_count: AtomicU64,
    #[allow(dead_code)]
    last_rate_limit: Mutex<Option<CapturedRateLimit>>,
}

impl GitlabMergeRequestClient {
    pub fn new(endpoint: String, token: String, project_path: String) -> Result<Self, CoreError> {
        if project_path.is_empty() {
            return Err(CoreError::Adapter("gitlab project path is required".into()));
        }
        let base = if endpoint.is_empty() {
            "https://gitlab.com"
        } else {
            endpoint.trim_end_matches('/')
        };
        Ok(Self {
            http: reqwest::Client::new(),
            graphql_url: format!("{base}/api/graphql"),
            rest_base_url: format!("{base}/api/v4"),
            token,
            encoded_project_path: urlencoding::encode(&project_path).into_owned(),
            project_path,
            request_count: AtomicU64::new(0),
            last_rate_limit: Mutex::new(None),
        })
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

    async fn graphql<R: serde::de::DeserializeOwned>(
        &self,
        body: &graphql_client::QueryBody<impl serde::Serialize>,
    ) -> Result<R, CoreError> {
        self.track_request();
        let response = self
            .http
            .post(&self.graphql_url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        if let Some(signal) = gitlab_rate_limit_signal_from_response("gitlab:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Adapter(format!(
                "gitlab graphql {status}: {text}"
            )));
        }

        #[derive(Deserialize)]
        struct Resp<T> {
            data: Option<T>,
            errors: Option<Vec<GqlErr>>,
        }
        #[derive(Deserialize)]
        struct GqlErr {
            message: String,
        }

        let parsed: Resp<R> = response
            .json()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        if let Some(errors) = parsed.errors
            && !errors.is_empty()
        {
            let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
            return Err(CoreError::Adapter(format!(
                "gitlab graphql errors: {}",
                msgs.join("; ")
            )));
        }

        parsed
            .data
            .ok_or_else(|| CoreError::Adapter("gitlab graphql returned no data".into()))
    }
}

// ---------------------------------------------------------------------------
// PullRequestCommenter
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct GitlabPullRequestCommenter {
    client: GitlabMergeRequestClient,
}

impl GitlabPullRequestCommenter {
    pub fn new(endpoint: String, token: String, project_path: String) -> Result<Self, CoreError> {
        Ok(Self {
            client: GitlabMergeRequestClient::new(endpoint, token, project_path)?,
        })
    }

    async fn find_existing_note_with_marker(
        &self,
        mr_iid: u64,
        marker: &str,
    ) -> Result<Option<u64>, CoreError> {
        let body = FetchMergeRequestByIid::build_query(fetch_merge_request_by_iid::Variables {
            full_path: self.client.project_path.clone(),
            iid: mr_iid.to_string(),
        });
        let data: fetch_merge_request_by_iid::ResponseData = self.client.graphql(&body).await?;
        let notes = data
            .project
            .and_then(|p| p.merge_request)
            .and_then(|mr| mr.notes)
            .and_then(|n| n.nodes);
        let Some(notes) = notes else {
            return Ok(None);
        };
        for note in &notes {
            if !note.system && note.body.contains(marker) {
                // Extract numeric ID from gid://gitlab/Note/12345
                let numeric_id = note
                    .id
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.parse::<u64>().ok());
                return Ok(numeric_id);
            }
        }
        Ok(None)
    }

    async fn update_note(&self, mr_iid: u64, note_id: u64, body: &str) -> Result<(), CoreError> {
        self.client.track_request();
        let url = self
            .client
            .rest_url(&format!("merge_requests/{mr_iid}/notes/{note_id}"));
        let response = self
            .client
            .http
            .put(url)
            .bearer_auth(&self.client.token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        if let Some(signal) = gitlab_rate_limit_signal_from_response("gitlab:rest", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "gitlab update note failed with status {status}"
            )));
        }
        Ok(())
    }

    async fn create_note_on_mr(&self, mr_iid: u64, body: &str) -> Result<(), CoreError> {
        self.client.track_request();
        let url = self
            .client
            .rest_url(&format!("merge_requests/{mr_iid}/notes"));
        let response = self
            .client
            .http
            .post(url)
            .bearer_auth(&self.client.token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        if let Some(signal) = gitlab_rate_limit_signal_from_response("gitlab:rest", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Adapter(format!(
                "gitlab create note failed with status {status}: {text}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl PullRequestCommenter for GitlabPullRequestCommenter {
    fn component_key(&self) -> String {
        "gitlab:merge_requests".into()
    }

    async fn comment_on_pull_request(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
    ) -> Result<(), CoreError> {
        info!(
            repository = %pull_request.repository,
            merge_request = pull_request.number,
            comment_len = body.len(),
            "posting GitLab merge request comment"
        );
        self.create_note_on_mr(pull_request.number, body).await
    }

    async fn sync_pull_request_comment(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
    ) -> Result<(), CoreError> {
        if let Some(note_id) = self
            .find_existing_note_with_marker(pull_request.number, marker)
            .await?
        {
            self.update_note(pull_request.number, note_id, body).await
        } else {
            self.create_note_on_mr(pull_request.number, body).await
        }
    }

    async fn sync_pull_request_review(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
        _comments: &[PullRequestReviewComment],
        _commit_sha: &str,
        _verdict: ReviewVerdict,
    ) -> Result<(), CoreError> {
        // GitLab doesn't have a native pull request review API like GitHub.
        // Post the review as a regular MR note with the marker.
        self.sync_pull_request_comment(pull_request, marker, body)
            .await
    }
}

// ---------------------------------------------------------------------------
// PullRequestManager
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct GitlabPullRequestManager {
    client: GitlabMergeRequestClient,
}

impl GitlabPullRequestManager {
    pub fn new(endpoint: String, token: String, project_path: String) -> Result<Self, CoreError> {
        Ok(Self {
            client: GitlabMergeRequestClient::new(endpoint, token, project_path)?,
        })
    }

    async fn existing_merge_request(
        &self,
        source_branch: &str,
    ) -> Result<Option<PullRequestRef>, CoreError> {
        self.client.track_request();
        let url = self.client.rest_url("merge_requests");
        let response = self
            .client
            .http
            .get(url)
            .bearer_auth(&self.client.token)
            .query(&[
                ("state", "opened"),
                ("source_branch", source_branch),
                ("per_page", "1"),
            ])
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        if let Some(signal) = gitlab_rate_limit_signal_from_response("gitlab:rest", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "gitlab list merge requests failed with status {status}"
            )));
        }

        #[derive(Deserialize)]
        struct MrResponse {
            iid: u64,
            web_url: String,
        }

        let mrs: Vec<MrResponse> = response
            .json()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        Ok(mrs.into_iter().next().map(|mr| PullRequestRef {
            repository: self.client.project_path.clone(),
            number: mr.iid,
            url: Some(mr.web_url),
        }))
    }
}

#[async_trait]
impl PullRequestManager for GitlabPullRequestManager {
    fn component_key(&self) -> String {
        "gitlab:merge_requests".into()
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
            "ensuring GitLab merge request"
        );

        if let Some(existing) = self.existing_merge_request(&request.head_branch).await? {
            debug!(
                merge_request = existing.number,
                "reusing existing GitLab merge request"
            );
            return Ok(existing);
        }

        #[derive(Serialize)]
        struct CreateMr {
            source_branch: String,
            target_branch: String,
            title: String,
            description: String,
        }

        let title = if request.draft {
            format!("Draft: {}", request.title)
        } else {
            request.title.clone()
        };

        self.client.track_request();
        let url = self.client.rest_url("merge_requests");
        let response = self
            .client
            .http
            .post(url)
            .bearer_auth(&self.client.token)
            .json(&CreateMr {
                source_branch: request.head_branch.clone(),
                target_branch: request.base_branch.clone(),
                title,
                description: request.body.clone(),
            })
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;

        if let Some(signal) = gitlab_rate_limit_signal_from_response("gitlab:rest", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Adapter(format!(
                "gitlab create merge request failed with status {status}: {text}"
            )));
        }

        #[derive(Deserialize)]
        struct MrResponse {
            iid: u64,
            web_url: String,
        }

        let mr: MrResponse = response
            .json()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        info!(merge_request = mr.iid, "created GitLab merge request");
        Ok(PullRequestRef {
            repository: request.repository.clone(),
            number: mr.iid,
            url: Some(mr.web_url),
        })
    }

    async fn merge_pull_request(&self, pull_request: &PullRequestRef) -> Result<(), CoreError> {
        info!(
            repository = %pull_request.repository,
            merge_request = pull_request.number,
            "merging GitLab merge request"
        );
        self.client.track_request();
        let url = self
            .client
            .rest_url(&format!("merge_requests/{}/merge", pull_request.number));
        let response = self
            .client
            .http
            .put(url)
            .bearer_auth(&self.client.token)
            .json(&serde_json::json!({ "squash": true }))
            .send()
            .await
            .map_err(|e| CoreError::Adapter(e.to_string()))?;
        if let Some(signal) = gitlab_rate_limit_signal_from_response("gitlab:rest", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Adapter(format!(
                "gitlab merge failed with status {status}: {text}"
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PullRequestEventSource
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct GitlabMergeRequestEventSource {
    client: GitlabMergeRequestClient,
}

impl GitlabMergeRequestEventSource {
    pub fn new(endpoint: String, token: String, project_path: String) -> Result<Self, CoreError> {
        Ok(Self {
            client: GitlabMergeRequestClient::new(endpoint, token, project_path)?,
        })
    }
}

#[async_trait]
impl PullRequestEventSource for GitlabMergeRequestEventSource {
    fn component_key(&self) -> String {
        "pull_requests:gitlab".into()
    }

    async fn fetch_events(&self) -> Result<Vec<PullRequestEvent>, CoreError> {
        let mut events = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let body = FetchOpenMergeRequests::build_query(fetch_open_merge_requests::Variables {
                full_path: self.client.project_path.clone(),
                after: cursor.clone(),
            });
            let data: fetch_open_merge_requests::ResponseData = self.client.graphql(&body).await?;

            let Some(project) = data.project else {
                break;
            };
            let Some(conn) = project.merge_requests else {
                break;
            };
            if let Some(nodes) = &conn.nodes {
                for mr in nodes {
                    let Some(head_sha) = mr.diff_head_sha.as_ref() else {
                        continue;
                    };
                    let labels: Vec<String> = mr
                        .labels
                        .as_ref()
                        .and_then(|c| c.nodes.as_ref())
                        .map(|nodes| nodes.iter().map(|l| l.title.clone()).collect())
                        .unwrap_or_default();

                    events.push(PullRequestEvent::Review(PullRequestReviewEvent {
                        provider: ReviewProviderKind::Gitlab,
                        repository: self.client.project_path.clone(),
                        number: mr.iid.parse::<u64>().unwrap_or(0),
                        title: mr.title.clone(),
                        url: Some(mr.web_url.clone()),
                        base_branch: mr.target_branch.clone(),
                        head_branch: mr.source_branch.clone(),
                        head_sha: head_sha.clone(),
                        checkout_ref: None,
                        author_login: mr.author.as_ref().map(|a| a.username.clone()),
                        approval_state: DispatchApprovalState::Approved,
                        labels,
                        created_at: parse_gitlab_time(&mr.created_at),
                        updated_at: parse_gitlab_time(&mr.updated_at),
                        is_draft: mr.draft,
                    }));
                }
            }

            if conn.page_info.has_next_page {
                cursor = conn.page_info.end_cursor;
            } else {
                break;
            }
        }
        Ok(events)
    }
}
