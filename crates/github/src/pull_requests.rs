use crate::{prelude::*, *};

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

    async fn existing_comment_id_with_marker(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
    ) -> Result<Option<u64>, CoreError> {
        let (owner, repo) = split_repo(&pull_request.repository)?;
        let response = self
            .client
            .get(format!(
                "https://api.github.com/repos/{owner}/{repo}/issues/{}/comments",
                pull_request.number
            ))
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .query(&[("per_page", "100")])
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github list pull request comments failed with status {status}"
            )));
        }
        let comments = response
            .json::<Vec<GithubIssueCommentResponse>>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        Ok(find_issue_comment_id_with_marker(&comments, marker))
    }

    async fn update_issue_comment(
        &self,
        pull_request: &PullRequestRef,
        comment_id: u64,
        body: &str,
    ) -> Result<(), CoreError> {
        let (owner, repo) = split_repo(&pull_request.repository)?;
        let response = self
            .client
            .patch(format!(
                "https://api.github.com/repos/{owner}/{repo}/issues/comments/{comment_id}"
            ))
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github update comment failed with status {status}"
            )));
        }
        Ok(())
    }

    async fn existing_review_with_marker(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
    ) -> Result<Option<u64>, CoreError> {
        let (owner, repo) = split_repo(&pull_request.repository)?;
        let response = self
            .client
            .get(format!(
                "https://api.github.com/repos/{owner}/{repo}/pulls/{}/reviews",
                pull_request.number
            ))
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .query(&[("per_page", "100")])
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "github list pull request reviews failed with status {status}"
            )));
        }
        let reviews = response
            .json::<Vec<GithubPullRequestReviewResponse>>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        Ok(reviews
            .into_iter()
            .find(|review| {
                review
                    .body
                    .as_deref()
                    .is_some_and(|body| body.contains(marker))
            })
            .map(|review| review.id))
    }

    async fn submit_pull_request_review(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
        comments: &[PullRequestReviewComment],
        commit_sha: &str,
        verdict: polyphony_core::ReviewVerdict,
    ) -> Result<(), CoreError> {
        let (owner, repo) = split_repo(&pull_request.repository)?;
        let response = self
            .client
            .post(format!(
                "https://api.github.com/repos/{owner}/{repo}/pulls/{}/reviews",
                pull_request.number
            ))
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&CreatePullRequestReviewBody {
                body: body.to_string(),
                event: verdict.github_event().to_string(),
                commit_id: commit_sha.to_string(),
                comments: comments
                    .iter()
                    .map(|comment| CreatePullRequestReviewComment {
                        path: comment.path.clone(),
                        line: comment.line,
                        side: "RIGHT".to_string(),
                        body: format_review_comment_body(comment),
                    })
                    .collect(),
            })
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = github_rate_limit_signal_from_response("github:graphql", &response) {
            return Err(CoreError::RateLimited(Box::new(signal)));
        }
        let status = response.status();
        if !status.is_success() {
            let response_body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<unreadable body>"));
            return Err(CoreError::Adapter(format!(
                "github create pull request review failed with status {status}: {response_body}"
            )));
        }
        Ok(())
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

    async fn sync_pull_request_comment(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
    ) -> Result<(), CoreError> {
        if let Some(comment_id) = self
            .existing_comment_id_with_marker(pull_request, marker)
            .await?
        {
            self.update_issue_comment(pull_request, comment_id, body)
                .await
        } else {
            self.comment_on_pull_request(pull_request, body).await
        }
    }

    async fn sync_pull_request_review(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
        comments: &[PullRequestReviewComment],
        commit_sha: &str,
        verdict: polyphony_core::ReviewVerdict,
    ) -> Result<(), CoreError> {
        // When there are no inline comments and the verdict is a plain comment,
        // fall back to a regular issue comment (editable, idempotent).
        if comments.is_empty() && verdict == polyphony_core::ReviewVerdict::Comment {
            return self
                .sync_pull_request_comment(pull_request, marker, body)
                .await;
        }
        if self
            .existing_review_with_marker(pull_request, marker)
            .await?
            .is_some()
        {
            return Ok(());
        }
        self.submit_pull_request_review(pull_request, body, comments, commit_sha, verdict)
            .await
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

#[derive(Debug, Deserialize)]
pub(crate) struct GithubIssueCommentResponse {
    pub(crate) id: u64,
    pub(crate) body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubPullRequestReviewResponse {
    pub(crate) id: u64,
    pub(crate) body: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreatePullRequestReviewBody {
    body: String,
    event: String,
    commit_id: String,
    comments: Vec<CreatePullRequestReviewComment>,
}

#[derive(Debug, Serialize)]
struct CreatePullRequestReviewComment {
    path: String,
    line: u32,
    side: String,
    body: String,
}

/// Format a review comment body with an optional priority badge and title.
///
/// When a priority (0-4) is present, a shields.io badge is prepended:
/// `![P1](https://img.shields.io/badge/P1-orange?style=flat-square)`
///
/// When a title is present, it is rendered bold after the badge.
fn format_review_comment_body(comment: &PullRequestReviewComment) -> String {
    let mut parts = Vec::new();
    if let Some(priority) = comment.priority {
        let (label, color) = match priority {
            0 => ("P0", "red"),
            1 => ("P1", "orange"),
            2 => ("P2", "yellow"),
            3 => ("P3", "blue"),
            _ => ("P4", "lightgrey"),
        };
        parts.push(format!(
            "![{label}](https://img.shields.io/badge/{label}-{color}?style=flat-square)"
        ));
    }
    if let Some(title) = &comment.title {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            parts.push(format!("**{trimmed}**"));
        }
    }
    if parts.is_empty() {
        comment.body.clone()
    } else {
        // Badge and title on one line, body below separated by a blank line.
        let header = parts.join("  ");
        format!("{header}\n\n{}", comment.body)
    }
}

pub(crate) fn find_issue_comment_id_with_marker(
    comments: &[GithubIssueCommentResponse],
    marker: &str,
) -> Option<u64> {
    comments.iter().find_map(|comment| {
        comment
            .body
            .as_deref()
            .is_some_and(|body| body.contains(marker))
            .then_some(comment.id)
    })
}

#[derive(Debug, Serialize)]
struct CreatePullRequestBody {
    title: String,
    head: String,
    base: String,
    body: String,
    draft: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use polyphony_core::PullRequestReviewComment;

    use super::*;

    #[test]
    fn format_comment_body_plain() {
        let comment = PullRequestReviewComment {
            path: "src/lib.rs".into(),
            line: 10,
            body: "This looks wrong.".into(),
            title: None,
            priority: None,
        };
        assert_eq!(format_review_comment_body(&comment), "This looks wrong.");
    }

    #[test]
    fn format_comment_body_with_priority_and_title() {
        let comment = PullRequestReviewComment {
            path: "src/auth.rs".into(),
            line: 42,
            body: "Use `map_err` instead.".into(),
            title: Some("Unchecked unwrap".into()),
            priority: Some(1),
        };
        let body = format_review_comment_body(&comment);
        assert!(body.contains("img.shields.io/badge/P1-orange"));
        assert!(body.contains("**Unchecked unwrap**"));
        assert!(body.contains("Use `map_err` instead."));
    }

    #[test]
    fn format_comment_body_p0_is_red() {
        let comment = PullRequestReviewComment {
            path: "x.rs".into(),
            line: 1,
            body: "Critical.".into(),
            title: None,
            priority: Some(0),
        };
        let body = format_review_comment_body(&comment);
        assert!(body.contains("P0-red"));
    }

    #[test]
    fn format_comment_body_priority_only() {
        let comment = PullRequestReviewComment {
            path: "x.rs".into(),
            line: 1,
            body: "Minor issue.".into(),
            title: None,
            priority: Some(3),
        };
        let body = format_review_comment_body(&comment);
        assert!(body.contains("P3-blue"));
        assert!(body.contains("\n\nMinor issue."));
    }
}
