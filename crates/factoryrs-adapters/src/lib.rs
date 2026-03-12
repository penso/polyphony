use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
#[cfg(feature = "linear")]
use factoryrs_core::RateLimitSignal;
use factoryrs_core::{
    AgentEvent, AgentEventKind, AgentRunResult, AgentRunSpec, AgentRuntime, AttemptStatus,
    BlockerRef, BudgetSnapshot, Error as CoreError, Issue, IssueAuthor, IssueComment,
    IssueStateUpdate, IssueTracker, TokenUsage, TrackerQuery,
};
#[cfg(feature = "linear")]
use graphql_client::GraphQLQuery;
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

#[cfg(feature = "linear")]
type DateTime = String;

#[cfg(feature = "linear")]
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/linear_schema.json",
    query_path = "src/issues.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct LinearIssuesPage;

#[cfg(feature = "linear")]
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/linear_schema.json",
    query_path = "src/issue_states.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct LinearIssueStates;

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

    async fn graphql(&self, body: serde_json::Value) -> Result<serde_json::Value, CoreError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .json(&body)
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

    async fn fetch_issues_for_states(
        &self,
        project_slug: &str,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        let mut after = None::<String>;
        let mut issues = Vec::new();

        loop {
            let payload = self
                .graphql(
                    serde_json::to_value(LinearIssuesPage::build_query(
                        linear_issues_page::Variables {
                            project_slug: project_slug.to_string(),
                            states: states.to_vec(),
                            after: after.clone(),
                        },
                    ))
                    .map_err(|error| CoreError::Adapter(error.to_string()))?,
                )
                .await?;
            let response: graphql_client::Response<linear_issues_page::ResponseData> =
                serde_json::from_value(payload)
                    .map_err(|error| CoreError::Adapter(error.to_string()))?;
            if response.errors.is_some() {
                return Err(CoreError::Adapter("linear_graphql_errors".into()));
            }
            let data = response
                .data
                .ok_or_else(|| CoreError::Adapter("linear_unknown_payload".into()))?;
            issues.extend(data.issues.nodes.iter().map(linear_issue_from_node));

            let has_next_page = data.issues.page_info.has_next_page;
            after = data.issues.page_info.end_cursor;
            if !has_next_page || after.is_none() {
                break;
            }
        }

        Ok(issues)
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
        self.fetch_issues_for_states(&project_slug, &query.active_states)
            .await
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
        self.fetch_issues_for_states(project_slug, states).await
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
                serde_json::to_value(LinearIssueStates::build_query(
                    linear_issue_states::Variables {
                        issue_ids: issue_ids.to_vec(),
                    },
                ))
                .map_err(|error| CoreError::Adapter(error.to_string()))?,
            )
            .await?;
        let response: graphql_client::Response<linear_issue_states::ResponseData> =
            serde_json::from_value(payload)
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if response.errors.is_some() {
            return Err(CoreError::Adapter("linear_graphql_errors".into()));
        }
        let data = response
            .data
            .ok_or_else(|| CoreError::Adapter("linear_unknown_payload".into()))?;
        Ok(data
            .issues
            .nodes
            .iter()
            .map(|node| IssueStateUpdate {
                id: node.id.clone(),
                identifier: node.identifier.clone(),
                state: node.state.name.clone(),
                updated_at: parse_rfc3339(&node.updated_at),
            })
            .collect())
    }
}

#[cfg(feature = "linear")]
fn linear_issue_from_node(node: &linear_issues_page::LinearIssuesPageIssuesNodes) -> Issue {
    Issue {
        id: node.id.clone(),
        identifier: node.identifier.clone(),
        title: node.title.clone(),
        description: node.description.clone(),
        priority: parse_linear_priority(node.priority),
        state: node.state.name.clone(),
        branch_name: Some(node.branch_name.clone()),
        url: Some(node.url.clone()),
        author: node.creator.as_ref().map(linear_author_from_user),
        labels: node
            .labels
            .nodes
            .iter()
            .map(|label| label.name.to_ascii_lowercase())
            .collect(),
        comments: linear_comments_from_connection(&node.comments.nodes),
        blocked_by: node
            .inverse_relations
            .nodes
            .iter()
            .filter(|relation| relation.type_ == "blocks")
            .map(|relation| BlockerRef {
                id: Some(relation.related_issue.id.clone()),
                identifier: Some(relation.related_issue.identifier.clone()),
                state: Some(relation.related_issue.state.name.clone()),
            })
            .collect(),
        created_at: parse_rfc3339(&node.created_at),
        updated_at: parse_rfc3339(&node.updated_at),
    }
}

#[cfg(feature = "linear")]
fn linear_author_from_user(
    user: &linear_issues_page::LinearIssuesPageIssuesNodesCreator,
) -> IssueAuthor {
    IssueAuthor {
        id: Some(user.id.clone()),
        username: Some(user.name.clone()),
        display_name: Some(user.display_name.clone()),
        role: Some(linear_role(user.owner, user.admin, user.guest)),
        trust_level: Some(linear_trust_level(user.owner, user.admin, user.guest)),
        url: Some(user.url.clone()),
    }
}

#[cfg(feature = "linear")]
fn linear_comment_author_from_user(
    user: &linear_issues_page::LinearIssuesPageIssuesNodesCommentsNodesUser,
) -> IssueAuthor {
    IssueAuthor {
        id: Some(user.id.clone()),
        username: Some(user.name.clone()),
        display_name: Some(user.display_name.clone()),
        role: Some(linear_role(user.owner, user.admin, user.guest)),
        trust_level: Some(linear_trust_level(user.owner, user.admin, user.guest)),
        url: Some(user.url.clone()),
    }
}

#[cfg(feature = "linear")]
fn linear_comments_from_connection(
    comments: &[linear_issues_page::LinearIssuesPageIssuesNodesCommentsNodes],
) -> Vec<IssueComment> {
    let mut collected = Vec::new();
    for comment in comments {
        collected.push(IssueComment {
            id: comment.id.clone(),
            body: comment.body.clone(),
            author: comment.user.as_ref().map(linear_comment_author_from_user),
            url: Some(comment.url.clone()),
            created_at: parse_rfc3339(&comment.created_at),
            updated_at: parse_rfc3339(&comment.updated_at),
        });
        for child in &comment.children.nodes {
            collected.push(IssueComment {
                id: child.id.clone(),
                body: child.body.clone(),
                author: child
                    .user
                    .as_ref()
                    .map(linear_child_comment_author_from_user),
                url: Some(child.url.clone()),
                created_at: parse_rfc3339(&child.created_at),
                updated_at: parse_rfc3339(&child.updated_at),
            });
        }
    }
    collected
}

#[cfg(feature = "linear")]
fn linear_child_comment_author_from_user(
    user: &linear_issues_page::LinearIssuesPageIssuesNodesCommentsNodesChildrenNodesUser,
) -> IssueAuthor {
    IssueAuthor {
        id: Some(user.id.clone()),
        username: Some(user.name.clone()),
        display_name: Some(user.display_name.clone()),
        role: Some(linear_role(user.owner, user.admin, user.guest)),
        trust_level: Some(linear_trust_level(user.owner, user.admin, user.guest)),
        url: Some(user.url.clone()),
    }
}

#[cfg(feature = "linear")]
fn linear_role(owner: bool, admin: bool, guest: bool) -> String {
    if owner {
        "owner".into()
    } else if admin {
        "admin".into()
    } else if guest {
        "guest".into()
    } else {
        "member".into()
    }
}

#[cfg(feature = "linear")]
fn linear_trust_level(owner: bool, admin: bool, guest: bool) -> String {
    if guest {
        "external_guest".into()
    } else if owner {
        "internal_owner".into()
    } else if admin {
        "internal_admin".into()
    } else {
        "internal_member".into()
    }
}

#[cfg(feature = "linear")]
fn parse_linear_priority(priority: f64) -> Option<i32> {
    if priority.fract() == 0.0 {
        Some(priority as i32)
    } else {
        None
    }
}

#[cfg(feature = "linear")]
fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

#[cfg(feature = "linear")]
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
        author: Some(IssueAuthor {
            id: Some("mock-user".into()),
            username: Some("factory-bot".into()),
            display_name: Some("Factory Bot".into()),
            role: Some("member".into()),
            trust_level: Some("internal_member".into()),
            url: None,
        }),
        labels: vec!["automation".into(), "rust".into()],
        comments: vec![IssueComment {
            id: format!("{identifier}-comment-1"),
            body: "Please include tests and note any follow-up risks.".into(),
            author: Some(IssueAuthor {
                id: Some("mock-reviewer".into()),
                username: Some("maintainer".into()),
                display_name: Some("Maintainer".into()),
                role: Some("admin".into()),
                trust_level: Some("internal_admin".into()),
                url: None,
            }),
            url: None,
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }],
        blocked_by: Vec::new(),
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    }
}
