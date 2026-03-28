use crate::{prelude::*, *};

#[derive(Debug, Clone)]
pub struct GithubPullRequestReviewEventSource {
    http: reqwest::Client,
    token: String,
    owner: String,
    repo: String,
}

impl GithubPullRequestReviewEventSource {
    pub fn new(repository: String, token: String) -> Result<Self, CoreError> {
        let (owner, repo) = split_repo(&repository)?;
        Ok(Self {
            http: reqwest::Client::new(),
            token,
            owner,
            repo,
        })
    }

    async fn graphql<ResponseData, QueryBody>(
        &self,
        body: QueryBody,
    ) -> Result<graphql_client::Response<ResponseData>, CoreError>
    where
        ResponseData: DeserializeOwned,
        QueryBody: serde::Serialize,
    {
        let response = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .header("User-Agent", "polyphony")
            .json(&body)
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
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
}

#[async_trait]
impl PullRequestEventSource for GithubPullRequestReviewEventSource {
    fn component_key(&self) -> String {
        "pull_requests:github".into()
    }

    async fn fetch_events(&self) -> Result<Vec<PullRequestEvent>, CoreError> {
        let repository_name = format!("{}/{}", self.owner, self.repo);
        let mut events = Vec::new();
        let mut after = None;
        loop {
            let payload = self
                .graphql::<fetch_pull_request_events::ResponseData, _>(
                    FetchPullRequestEvents::build_query(fetch_pull_request_events::Variables {
                        owner: self.owner.clone(),
                        name: self.repo.clone(),
                        after: after.clone(),
                    }),
                )
                .await?;
            let Some(repository) = payload.data.and_then(|data| data.repository) else {
                break;
            };
            let pull_requests = repository.pull_requests;
            events.extend(pull_request_events_from_graphql(
                &repository_name,
                pull_requests.nodes.unwrap_or_default(),
            ));
            if !pull_requests.page_info.has_next_page {
                break;
            }
            after = pull_requests.page_info.end_cursor;
        }
        Ok(events)
    }
}

#[cfg(test)]
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct GithubReviewPullRequestResponse {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) html_url: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) draft: Option<bool>,
    pub(crate) user: Option<GithubReviewUser>,
    pub(crate) author_association: Option<AuthorAssociation>,
    #[serde(default)]
    pub(crate) labels: Vec<GithubReviewLabel>,
    pub(crate) base: GithubReviewBranchRef,
    pub(crate) head: GithubReviewHeadRef,
}

#[cfg(test)]
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct GithubReviewUser {
    pub(crate) login: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct GithubReviewLabel {
    pub(crate) name: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct GithubReviewBranchRef {
    #[serde(rename = "ref")]
    pub(crate) name: String,
}

#[cfg(test)]
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct GithubReviewHeadRef {
    #[serde(rename = "ref")]
    pub(crate) name: String,
    pub(crate) sha: String,
}

#[cfg(test)]
pub(crate) fn pull_request_review_events_from_responses(
    repository_name: &str,
    pull_requests: Vec<GithubReviewPullRequestResponse>,
) -> Vec<PullRequestReviewEvent> {
    pull_requests
        .into_iter()
        .filter(|pull_request| {
            !pull_request.head.name.is_empty() && !pull_request.head.sha.is_empty()
        })
        .map(|pull_request| {
            let author_login = pull_request
                .user
                .map(|user| user.login.to_ascii_lowercase());
            PullRequestReviewEvent {
                provider: ReviewProviderKind::Github,
                repository: repository_name.to_string(),
                number: pull_request.number,
                title: pull_request.title,
                url: Some(pull_request.html_url),
                base_branch: pull_request.base.name,
                head_branch: pull_request.head.name,
                head_sha: pull_request.head.sha,
                checkout_ref: Some(format!("refs/pull/{}/head", pull_request.number)),
                author_login: author_login.clone(),
                approval_state: github_issue_approval_state(
                    pull_request.author_association.as_ref(),
                    author_login.as_deref(),
                ),
                labels: pull_request
                    .labels
                    .into_iter()
                    .map(|label| label.name.trim().to_ascii_lowercase())
                    .filter(|label| !label.is_empty())
                    .collect(),
                created_at: Some(pull_request.created_at),
                updated_at: Some(pull_request.updated_at),
                is_draft: pull_request.draft.unwrap_or(false),
            }
        })
        .collect()
}

fn pull_request_events_from_graphql(
    repository_name: &str,
    pull_requests: Vec<
        Option<fetch_pull_request_events::FetchPullRequestEventsRepositoryPullRequestsNodes>,
    >,
) -> Vec<PullRequestEvent> {
    let mut events = Vec::new();
    for pull_request in pull_requests.into_iter().flatten() {
        let author_login = pull_request
            .author
            .as_ref()
            .map(|author| author.login.to_ascii_lowercase());
        let approval_state = github_graphql_approval_state(
            &pull_request.author_association,
            author_login.as_deref(),
        );
        let review_event = PullRequestReviewEvent {
            provider: ReviewProviderKind::Github,
            repository: repository_name.to_string(),
            number: pull_request.number as u64,
            title: pull_request.title.clone(),
            url: Some(pull_request.url.clone()),
            base_branch: pull_request.base_ref_name.clone(),
            head_branch: pull_request.head_ref_name.clone(),
            head_sha: pull_request.head_ref_oid.clone(),
            checkout_ref: Some(format!("refs/pull/{}/head", pull_request.number)),
            author_login: author_login.clone(),
            approval_state,
            labels: pull_request
                .labels
                .and_then(|labels| labels.nodes)
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .map(|label| label.name.trim().to_ascii_lowercase())
                .filter(|label: &String| !label.is_empty())
                .collect(),
            created_at: Some(pull_request.created_at),
            updated_at: Some(pull_request.updated_at),
            is_draft: pull_request.is_draft,
        };
        if should_emit_conflict_event(&pull_request.mergeable, &pull_request.merge_state_status) {
            events.push(PullRequestEvent::Conflict(PullRequestConflictEvent {
                provider: ReviewProviderKind::Github,
                repository: repository_name.to_string(),
                number: pull_request.number as u64,
                pull_request_title: pull_request.title.clone(),
                url: Some(pull_request.url.clone()),
                base_branch: pull_request.base_ref_name.clone(),
                head_branch: pull_request.head_ref_name.clone(),
                head_sha: pull_request.head_ref_oid.clone(),
                checkout_ref: Some(format!("refs/pull/{}/head", pull_request.number)),
                author_login: author_login.clone(),
                approval_state,
                labels: review_event.labels.clone(),
                created_at: Some(pull_request.created_at),
                updated_at: Some(pull_request.updated_at),
                is_draft: pull_request.is_draft,
                mergeable_state: format!("{:?}", pull_request.mergeable).to_ascii_lowercase(),
                merge_state_status: format!("{:?}", pull_request.merge_state_status)
                    .to_ascii_lowercase(),
            }));
        }
        let review_target = review_event.review_target();
        let review_labels = review_event.labels.clone();
        events.push(PullRequestEvent::Review(review_event));

        for thread in pull_request
            .review_threads
            .nodes
            .unwrap_or_default()
            .into_iter()
            .flatten()
        {
            if thread.is_resolved || thread.is_outdated {
                continue;
            }
            let Some(comment) =
                latest_unresolved_thread_comment(thread.comments.nodes.unwrap_or_default())
            else {
                continue;
            };
            let body = comment.body.trim().to_string();
            if body.is_empty() {
                continue;
            }
            let comment_author_login = comment
                .author
                .as_ref()
                .map(|author| author.login.to_ascii_lowercase());
            events.push(PullRequestEvent::Comment(PullRequestCommentEvent {
                provider: ReviewProviderKind::Github,
                repository: review_target.repository.clone(),
                number: review_target.number,
                pull_request_title: pull_request.title.clone(),
                url: Some(comment.url),
                base_branch: review_target.base_branch.clone(),
                head_branch: review_target.head_branch.clone(),
                head_sha: review_target.head_sha.clone(),
                checkout_ref: review_target.checkout_ref.clone(),
                thread_id: thread.id,
                comment_id: comment.id,
                path: thread.path,
                line: thread
                    .line
                    .map(|line| line as u32)
                    .or(thread.original_line.map(|line| line as u32)),
                body,
                author_login: comment_author_login.clone(),
                approval_state: github_graphql_approval_state(
                    &comment.author_association,
                    comment_author_login.as_deref(),
                ),
                labels: review_labels.clone(),
                created_at: Some(comment.created_at),
                updated_at: Some(comment.updated_at),
                is_draft: pull_request.is_draft,
            }));
        }
    }
    events
}

fn github_graphql_approval_state(
    association: &fetch_pull_request_events::CommentAuthorAssociation,
    author_login: Option<&str>,
) -> DispatchApprovalState {
    use fetch_pull_request_events::CommentAuthorAssociation::{COLLABORATOR, MEMBER, OWNER};

    if author_login.is_some_and(|login| login.eq_ignore_ascii_case("dependabot[bot]"))
        || matches!(association, OWNER | MEMBER | COLLABORATOR)
    {
        DispatchApprovalState::Approved
    } else {
        DispatchApprovalState::Waiting
    }
}

fn latest_unresolved_thread_comment(
    comments: Vec<Option<fetch_pull_request_events::FetchPullRequestEventsRepositoryPullRequestsNodesReviewThreadsNodesCommentsNodes>>,
) -> Option<fetch_pull_request_events::FetchPullRequestEventsRepositoryPullRequestsNodesReviewThreadsNodesCommentsNodes>{
    comments
        .into_iter()
        .flatten()
        .max_by_key(|comment| comment.updated_at)
}

pub(crate) fn should_emit_conflict_event(
    mergeable: &fetch_pull_request_events::MergeableState,
    merge_state_status: &fetch_pull_request_events::MergeStateStatus,
) -> bool {
    matches!(
        mergeable,
        fetch_pull_request_events::MergeableState::CONFLICTING
    ) || matches!(
        merge_state_status,
        fetch_pull_request_events::MergeStateStatus::DIRTY
    )
}
