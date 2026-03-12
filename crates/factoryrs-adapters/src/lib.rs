use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
#[cfg(any(feature = "linear", feature = "github"))]
use factoryrs_core::RateLimitSignal;
use factoryrs_core::{
    AgentEvent, AgentEventKind, AgentRunResult, AgentRunSpec, AgentRuntime, AttemptStatus,
    BudgetSnapshot, Error as CoreError, Issue, IssueStateUpdate, IssueTracker, TokenUsage,
    TrackerQuery,
};
use thiserror::Error;
use tokio::sync::{RwLock, mpsc};

#[derive(Debug, Error)]
pub enum Error {
    #[error("mock adapter error: {0}")]
    Mock(String),
    #[cfg(feature = "linear")]
    #[error("linear adapter error: {0}")]
    Linear(String),
}

#[cfg(feature = "mock")]
#[derive(Debug, Clone)]
pub struct MockTracker {
    issues: Arc<RwLock<HashMap<String, Issue>>>,
}

#[cfg(feature = "mock")]
impl MockTracker {
    pub fn seeded_demo() -> Self {
        let issues = [
            demo_issue("FAC-101", "Build workflow loader", Some(1), "Todo"),
            demo_issue(
                "FAC-102",
                "Add orchestrator retries",
                Some(1),
                "In Progress",
            ),
            demo_issue("FAC-103", "Stream live task view", Some(2), "Todo"),
        ]
        .into_iter()
        .map(|issue| (issue.id.clone(), issue))
        .collect();
        Self {
            issues: Arc::new(RwLock::new(issues)),
        }
    }

    pub async fn set_state(&self, issue_id: &str, state: &str) {
        if let Some(issue) = self.issues.write().await.get_mut(issue_id) {
            issue.state = state.to_string();
            issue.updated_at = Some(Utc::now());
        }
    }
}

#[cfg(feature = "mock")]
#[async_trait]
impl IssueTracker for MockTracker {
    fn component_key(&self) -> String {
        "tracker:mock".into()
    }

    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        let active = query
            .active_states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let issues = self.issues.read().await;
        Ok(issues
            .values()
            .filter(|issue| active.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let states = states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let issues = self.issues.read().await;
        Ok(issues
            .values()
            .filter(|issue| states.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        let issues = self.issues.read().await;
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .map(|issue| IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        Ok(Some(BudgetSnapshot {
            component: self.component_key(),
            captured_at: Utc::now(),
            credits_remaining: None,
            credits_total: None,
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: None,
        }))
    }
}

#[cfg(feature = "mock")]
#[derive(Debug, Clone)]
pub struct MockAgentRuntime {
    tracker: MockTracker,
}

#[cfg(feature = "mock")]
impl MockAgentRuntime {
    pub fn new(tracker: MockTracker) -> Self {
        Self { tracker }
    }
}

#[cfg(feature = "mock")]
#[async_trait]
impl AgentRuntime for MockAgentRuntime {
    fn component_key(&self) -> String {
        "provider:mock".into()
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        let session_id = format!(
            "{}-attempt-{}",
            spec.issue.identifier,
            spec.attempt.unwrap_or(0)
        );
        let _ = event_tx.send(AgentEvent {
            issue_id: spec.issue.id.clone(),
            issue_identifier: spec.issue.identifier.clone(),
            session_id: Some(session_id.clone()),
            kind: AgentEventKind::SessionStarted,
            at: Utc::now(),
            message: Some("session started".into()),
            usage: None,
            rate_limits: None,
            raw: None,
        });

        for turn in 1..=spec.max_turns.min(3) {
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                session_id: Some(session_id.clone()),
                kind: AgentEventKind::TurnStarted,
                at: Utc::now(),
                message: Some(format!("turn {turn} started")),
                usage: None,
                rate_limits: None,
                raw: None,
            });
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                session_id: Some(session_id.clone()),
                kind: AgentEventKind::Notification,
                at: Utc::now(),
                message: Some(format!("processing {}", spec.issue.title)),
                usage: None,
                rate_limits: None,
                raw: None,
            });
            tokio::time::sleep(Duration::from_millis(400)).await;
            let usage = TokenUsage {
                input_tokens: 600 * u64::from(turn),
                output_tokens: 280 * u64::from(turn),
                total_tokens: 880 * u64::from(turn),
            };
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                session_id: Some(session_id.clone()),
                kind: AgentEventKind::UsageUpdated,
                at: Utc::now(),
                message: Some(format!("turn {turn} usage updated")),
                usage: Some(usage),
                rate_limits: None,
                raw: None,
            });
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                session_id: Some(session_id.clone()),
                kind: AgentEventKind::TurnCompleted,
                at: Utc::now(),
                message: Some(format!("turn {turn} completed")),
                usage: None,
                rate_limits: None,
                raw: None,
            });
        }

        self.tracker.set_state(&spec.issue.id, "Human Review").await;
        Ok(AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: spec.max_turns.min(3),
            error: None,
            final_issue_state: Some("Human Review".into()),
        })
    }

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        Ok(Some(BudgetSnapshot {
            component: self.component_key(),
            captured_at: Utc::now(),
            credits_remaining: Some(100.0),
            credits_total: Some(100.0),
            spent_usd: Some(0.0),
            soft_limit_usd: Some(10.0),
            hard_limit_usd: Some(25.0),
            reset_at: None,
            raw: None,
        }))
    }
}

#[cfg(feature = "linear")]
#[derive(Debug, Clone)]
pub struct LinearTracker {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

#[cfg(feature = "linear")]
impl LinearTracker {
    pub fn new(endpoint: String, api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint,
            api_key,
        }
    }

    async fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, CoreError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .json(&serde_json::json!({
                "query": query,
                "variables": variables,
            }))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let status = response.status();
        if status.as_u16() == 429 {
            return Err(CoreError::RateLimited(rate_limit_signal(
                "tracker:linear",
                "linear_api_status_429",
                &response,
            )));
        }
        let payload = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if !status.is_success() {
            return Err(CoreError::Adapter(format!(
                "linear_api_status: {} {payload}",
                status
            )));
        }
        if payload.get("errors").is_some() {
            return Err(CoreError::Adapter(format!(
                "linear_graphql_errors: {payload}"
            )));
        }
        Ok(payload)
    }
}

#[cfg(feature = "linear")]
#[async_trait]
impl IssueTracker for LinearTracker {
    fn component_key(&self) -> String {
        "tracker:linear".into()
    }

    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        let project_slug = query
            .project_slug
            .clone()
            .ok_or_else(|| CoreError::Adapter("missing_tracker_project_slug".into()))?;
        let payload = self
            .graphql(
                r#"
                query CandidateIssues($projectSlug: String!, $states: [String!]!) {
                  issues(
                    filter: {
                      project: { slugId: { eq: $projectSlug } }
                      state: { name: { in: $states } }
                    }
                  ) {
                    nodes {
                      id
                      identifier
                      title
                      description
                      priority
                      branchName
                      url
                      createdAt
                      updatedAt
                      state { name }
                      labels { nodes { name } }
                    }
                  }
                }
                "#,
                serde_json::json!({
                    "projectSlug": project_slug,
                    "states": query.active_states,
                }),
            )
            .await?;
        Ok(parse_linear_issue_nodes(
            payload
                .pointer("/data/issues/nodes")
                .and_then(|value| value.as_array())
                .ok_or_else(|| CoreError::Adapter("linear_unknown_payload".into()))?,
        ))
    }

    async fn fetch_issues_by_states(
        &self,
        project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let project_slug = project_slug
            .ok_or_else(|| CoreError::Adapter("missing_tracker_project_slug".into()))?;
        let payload = self
            .graphql(
                r#"
                query IssuesByStates($projectSlug: String!, $states: [String!]!) {
                  issues(
                    filter: {
                      project: { slugId: { eq: $projectSlug } }
                      state: { name: { in: $states } }
                    }
                  ) {
                    nodes {
                      id
                      identifier
                      title
                      description
                      priority
                      branchName
                      url
                      createdAt
                      updatedAt
                      state { name }
                      labels { nodes { name } }
                    }
                  }
                }
                "#,
                serde_json::json!({
                    "projectSlug": project_slug,
                    "states": states,
                }),
            )
            .await?;
        Ok(parse_linear_issue_nodes(
            payload
                .pointer("/data/issues/nodes")
                .and_then(|value| value.as_array())
                .ok_or_else(|| CoreError::Adapter("linear_unknown_payload".into()))?,
        ))
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }
        let payload = self
            .graphql(
                r#"
                query IssueStates($issueIds: [ID!]!) {
                  issues(filter: { id: { in: $issueIds } }) {
                    nodes {
                      id
                      identifier
                      updatedAt
                      state { name }
                    }
                  }
                }
                "#,
                serde_json::json!({
                    "issueIds": issue_ids,
                }),
            )
            .await?;
        let nodes = payload
            .pointer("/data/issues/nodes")
            .and_then(|value| value.as_array())
            .ok_or_else(|| CoreError::Adapter("linear_unknown_payload".into()))?;
        Ok(nodes
            .iter()
            .map(|node| IssueStateUpdate {
                id: node["id"].as_str().unwrap_or_default().to_string(),
                identifier: node["identifier"].as_str().unwrap_or_default().to_string(),
                state: node["state"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                updated_at: node["updatedAt"]
                    .as_str()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc)),
            })
            .collect())
    }
}

#[cfg(feature = "github")]
#[derive(Debug, Clone)]
pub struct GithubTracker {
    client: reqwest::Client,
    token: Option<String>,
    repository: String,
}

#[cfg(feature = "github")]
impl GithubTracker {
    pub fn new(repository: String, token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token,
            repository,
        }
    }

    async fn get_issues(&self, state: &str) -> Result<Vec<serde_json::Value>, CoreError> {
        let mut request = self
            .client
            .get(format!(
                "https://api.github.com/repos/{}/issues?state={state}&per_page=100",
                self.repository
            ))
            .header("User-Agent", "factoryrs");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if response.status().as_u16() == 429 || response.status().as_u16() == 403 {
            let remaining = response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1);
            if remaining == 0 || response.status().as_u16() == 429 {
                return Err(CoreError::RateLimited(rate_limit_signal(
                    "tracker:github",
                    "github_rate_limit",
                    &response,
                )));
            }
        }
        response
            .json::<Vec<serde_json::Value>>()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))
    }
}

#[cfg(feature = "github")]
#[async_trait]
impl IssueTracker for GithubTracker {
    fn component_key(&self) -> String {
        "tracker:github".into()
    }

    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        let mut items = Vec::new();
        for state in &query.active_states {
            let api_state =
                if state.eq_ignore_ascii_case("done") || state.eq_ignore_ascii_case("closed") {
                    "closed"
                } else {
                    "open"
                };
            items.extend(self.get_issues(api_state).await?);
        }
        Ok(parse_github_issues(&items))
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        let mut items = Vec::new();
        for state in states {
            let api_state =
                if state.eq_ignore_ascii_case("done") || state.eq_ignore_ascii_case("closed") {
                    "closed"
                } else {
                    "open"
                };
            items.extend(self.get_issues(api_state).await?);
        }
        Ok(parse_github_issues(&items))
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        let items = self.get_issues("all").await?;
        Ok(parse_github_issues(&items)
            .into_iter()
            .filter(|issue| issue_ids.iter().any(|id| id == &issue.id))
            .map(|issue| IssueStateUpdate {
                id: issue.id,
                identifier: issue.identifier,
                state: issue.state,
                updated_at: issue.updated_at,
            })
            .collect())
    }
}

#[cfg(feature = "linear")]
fn parse_linear_issue_nodes(nodes: &[serde_json::Value]) -> Vec<Issue> {
    nodes
        .iter()
        .map(|node| Issue {
            id: node["id"].as_str().unwrap_or_default().to_string(),
            identifier: node["identifier"].as_str().unwrap_or_default().to_string(),
            title: node["title"].as_str().unwrap_or_default().to_string(),
            description: node["description"].as_str().map(ToOwned::to_owned),
            priority: node["priority"].as_i64().map(|value| value as i32),
            state: node["state"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            branch_name: node["branchName"].as_str().map(ToOwned::to_owned),
            url: node["url"].as_str().map(ToOwned::to_owned),
            labels: node["labels"]["nodes"]
                .as_array()
                .map(|labels| {
                    labels
                        .iter()
                        .filter_map(|label| label["name"].as_str())
                        .map(|label| label.to_ascii_lowercase())
                        .collect()
                })
                .unwrap_or_default(),
            blocked_by: Vec::new(),
            created_at: node["createdAt"]
                .as_str()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc)),
            updated_at: node["updatedAt"]
                .as_str()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc)),
        })
        .collect()
}

#[cfg(feature = "github")]
fn parse_github_issues(nodes: &[serde_json::Value]) -> Vec<Issue> {
    nodes
        .iter()
        .filter(|node| node.get("pull_request").is_none())
        .map(|node| {
            let number = node["number"].as_i64().unwrap_or_default();
            let state = if node["state"].as_str().unwrap_or("open") == "open" {
                "Todo"
            } else {
                "Done"
            };
            Issue {
                id: node["node_id"].as_str().unwrap_or_default().to_string(),
                identifier: format!("#{}", number),
                title: node["title"].as_str().unwrap_or_default().to_string(),
                description: node["body"].as_str().map(ToOwned::to_owned),
                priority: None,
                state: state.to_string(),
                branch_name: None,
                url: node["html_url"].as_str().map(ToOwned::to_owned),
                labels: node["labels"]
                    .as_array()
                    .map(|labels| {
                        labels
                            .iter()
                            .filter_map(|label| label["name"].as_str())
                            .map(|label| label.to_ascii_lowercase())
                            .collect()
                    })
                    .unwrap_or_default(),
                blocked_by: Vec::new(),
                created_at: node["created_at"]
                    .as_str()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc)),
                updated_at: node["updated_at"]
                    .as_str()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc)),
            }
        })
        .collect()
}

#[cfg(any(feature = "linear", feature = "github"))]
fn rate_limit_signal(
    component: &str,
    reason: &str,
    response: &reqwest::Response,
) -> RateLimitSignal {
    let retry_after_ms = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value * 1_000);
    let reset_at = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|value| chrono::DateTime::<Utc>::from_timestamp(value, 0));
    RateLimitSignal {
        component: component.to_string(),
        reason: reason.to_string(),
        limited_at: Utc::now(),
        retry_after_ms,
        reset_at,
        status_code: Some(response.status().as_u16()),
        raw: None,
    }
}

#[cfg(feature = "mock")]
fn demo_issue(identifier: &str, title: &str, priority: Option<i32>, state: &str) -> Issue {
    Issue {
        id: identifier.to_string(),
        identifier: identifier.to_string(),
        title: title.to_string(),
        description: Some(format!("Implement {title}")),
        priority,
        state: state.to_string(),
        branch_name: Some(format!("feat/{}", identifier.to_ascii_lowercase())),
        url: None,
        labels: vec!["automation".into(), "rust".into()],
        blocked_by: Vec::new(),
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    }
}
