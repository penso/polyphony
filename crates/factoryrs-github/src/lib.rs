use async_trait::async_trait;
use chrono::Utc;
use factoryrs_core::{
    BudgetSnapshot, Error as CoreError, Issue, IssueStateUpdate, IssueTracker,
    PullRequestCommenter, PullRequestRef, TrackerQuery,
};
use graphql_client::GraphQLQuery;
use octocrab::{Octocrab, models::issues::Issue as GithubIssue};
use thiserror::Error;

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

#[derive(Debug, Clone)]
pub struct GithubIssueTracker {
    crab: Octocrab,
    owner: String,
    repo: String,
}

impl GithubIssueTracker {
    pub fn new(repository: String, token: Option<String>) -> Result<Self, CoreError> {
        let (owner, repo) = split_repo(&repository)?;
        let mut builder = Octocrab::builder();
        if let Some(token) = token {
            builder = builder.personal_token(token);
        }
        let crab = builder
            .build()
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        Ok(Self { crab, owner, repo })
    }

    async fn all_issues(
        &self,
        state: octocrab::params::State,
    ) -> Result<Vec<GithubIssue>, CoreError> {
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
            page = next;
            issues.extend(page.take_items());
        }
        Ok(issues)
    }

    async fn issue_by_number(&self, number: u64) -> Result<GithubIssue, CoreError> {
        self.crab
            .issues(&self.owner, &self.repo)
            .get(number)
            .await
            .map_err(map_github_error)
    }
}

#[async_trait]
impl IssueTracker for GithubIssueTracker {
    fn component_key(&self) -> String {
        "tracker:github".into()
    }

    async fn fetch_candidate_issues(&self, _query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        Ok(self
            .all_issues(octocrab::params::State::Open)
            .await?
            .into_iter()
            .filter(|issue| issue.pull_request.is_none())
            .map(|issue| to_issue(&self.owner, &self.repo, issue))
            .collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        let wants_open = states.iter().any(|state| {
            !matches!(
                state.to_ascii_lowercase().as_str(),
                "done" | "closed" | "cancelled"
            )
        });
        let wants_closed = states.iter().any(|state| {
            matches!(
                state.to_ascii_lowercase().as_str(),
                "done" | "closed" | "cancelled"
            )
        });
        let mut issues = Vec::new();
        if wants_open {
            issues.extend(self.all_issues(octocrab::params::State::Open).await?);
        }
        if wants_closed {
            issues.extend(self.all_issues(octocrab::params::State::Closed).await?);
        }
        Ok(issues
            .into_iter()
            .filter(|issue| issue.pull_request.is_none())
            .map(|issue| to_issue(&self.owner, &self.repo, issue))
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        let mut updates = Vec::new();
        for issue_id in issue_ids {
            let number = issue_id
                .parse::<u64>()
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            let issue = self.issue_by_number(number).await?;
            updates.push(IssueStateUpdate {
                id: issue.number.to_string(),
                identifier: format!("#{}", issue.number),
                state: if issue.state == octocrab::models::IssueState::Open {
                    "Todo".into()
                } else {
                    "Done".into()
                },
                updated_at: Some(issue.updated_at.with_timezone(&Utc)),
            });
        }
        Ok(updates)
    }

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        Ok(None)
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
        let (owner, name) = split_repo(&pull_request.repository)?;
        let number = i64::try_from(pull_request.number)
            .map_err(|error| CoreError::Adapter(error.to_string()))?;

        let response = self
            .client
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .header("User-Agent", "factoryrs")
            .json(&ResolvePullRequestId::build_query(
                resolve_pull_request_id::Variables {
                    owner: owner.clone(),
                    name: name.clone(),
                    number: number as i64,
                },
            ))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
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
            .header("User-Agent", "factoryrs")
            .json(&AddCommentToPullRequest::build_query(
                add_comment_to_pull_request::Variables {
                    subject_id,
                    body: body.to_string(),
                },
            ))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
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

fn to_issue(_owner: &str, _repo: &str, issue: GithubIssue) -> Issue {
    Issue {
        id: issue.number.to_string(),
        identifier: format!("#{}", issue.number),
        title: issue.title,
        description: issue.body,
        priority: None,
        state: if issue.state == octocrab::models::IssueState::Open {
            "Todo".into()
        } else {
            "Done".into()
        },
        branch_name: Some(format!("issue-{}", issue.number)),
        url: Some(issue.html_url.to_string()),
        labels: issue
            .labels
            .into_iter()
            .map(|label| label.name.to_ascii_lowercase())
            .collect(),
        blocked_by: Vec::new(),
        created_at: Some(issue.created_at.with_timezone(&Utc)),
        updated_at: Some(issue.updated_at.with_timezone(&Utc)),
    }
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

fn map_github_error(error: octocrab::Error) -> CoreError {
    match &error {
        octocrab::Error::GitHub { source, .. } => {
            if source.status_code.as_u16() == 403 || source.status_code.as_u16() == 429 {
                return CoreError::RateLimited(factoryrs_core::RateLimitSignal {
                    component: "tracker:github".into(),
                    reason: format!("github api {}", source.status_code),
                    limited_at: Utc::now(),
                    retry_after_ms: None,
                    reset_at: None,
                    status_code: Some(source.status_code.as_u16()),
                    raw: None,
                });
            }
        }
        _ => {}
    }
    CoreError::Adapter(error.to_string())
}
