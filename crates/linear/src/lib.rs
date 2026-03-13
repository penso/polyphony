use {
    async_trait::async_trait,
    chrono::Utc,
    graphql_client::GraphQLQuery,
    polyphony_core::{
        BlockerRef, BudgetSnapshot, Error as CoreError, Issue, IssueAuthor, IssueComment,
        IssueStateUpdate, IssueTracker, RateLimitSignal, TrackerQuery,
    },
    std::time::Duration,
    tracing::debug,
};

const LINEAR_HTTP_TIMEOUT: Duration = Duration::from_millis(30_000);

type DateTime = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/linear_schema.json",
    query_path = "src/issues.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct LinearIssuesPage;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/linear_schema.json",
    query_path = "src/issue_states.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct LinearIssueStates;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/linear_schema.json",
    query_path = "src/issues_by_ids.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct LinearIssuesByIds;

#[derive(Debug, Clone)]
pub struct LinearTracker {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl LinearTracker {
    pub fn new(endpoint: String, api_key: String) -> Result<Self, CoreError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(LINEAR_HTTP_TIMEOUT)
                .build()
                .map_err(|error| CoreError::Adapter(format!("linear_api_request: {error}")))?,
            endpoint,
            api_key,
        })
    }

    async fn graphql(&self, body: serde_json::Value) -> Result<serde_json::Value, CoreError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| CoreError::Adapter(format!("linear_api_request: {error}")))?;
        let status = response.status();
        if status.as_u16() == 429 {
            return Err(CoreError::RateLimited(Box::new(rate_limit_signal(
                "tracker:linear",
                "linear_api_status_429",
                &response,
            ))));
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

            after = next_page_cursor(
                data.issues.page_info.has_next_page,
                data.issues.page_info.end_cursor,
            )?;
            if after.is_none() {
                break;
            }
        }

        Ok(issues)
    }
}

fn next_page_cursor(
    has_next_page: bool,
    end_cursor: Option<String>,
) -> Result<Option<String>, CoreError> {
    if !has_next_page {
        return Ok(None);
    }
    end_cursor
        .ok_or_else(|| CoreError::Adapter("linear_missing_end_cursor".into()))
        .map(Some)
}

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
        debug!(
            project_slug,
            state_count = query.active_states.len(),
            "fetching Linear candidate issues"
        );
        let issues = self
            .fetch_issues_for_states(&project_slug, &query.active_states)
            .await?;
        debug!(
            project_slug,
            issues = issues.len(),
            "fetched Linear candidate issues"
        );
        Ok(issues)
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

    async fn fetch_issues_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>, CoreError> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }
        debug!(
            issue_count = issue_ids.len(),
            "fetching Linear issues by id"
        );
        let payload = self
            .graphql(
                serde_json::to_value(LinearIssuesByIds::build_query(
                    linear_issues_by_ids::Variables {
                        issue_ids: issue_ids.to_vec(),
                    },
                ))
                .map_err(|error| CoreError::Adapter(error.to_string()))?,
            )
            .await?;
        let response: graphql_client::Response<linear_issues_by_ids::ResponseData> =
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
            .map(linear_issue_from_id_node)
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }
        debug!(
            issue_count = issue_ids.len(),
            "fetching Linear issue states by id"
        );
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

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        Ok(None)
    }
}

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

fn linear_issue_from_id_node(node: &linear_issues_by_ids::LinearIssuesByIdsIssuesNodes) -> Issue {
    Issue {
        id: node.id.clone(),
        identifier: node.identifier.clone(),
        title: node.title.clone(),
        description: node.description.clone(),
        priority: parse_linear_priority(node.priority),
        state: node.state.name.clone(),
        branch_name: Some(node.branch_name.clone()),
        url: Some(node.url.clone()),
        author: node.creator.as_ref().map(linear_author_from_id_user),
        labels: node
            .labels
            .nodes
            .iter()
            .map(|label| label.name.to_ascii_lowercase())
            .collect(),
        comments: linear_comments_from_id_connection(&node.comments.nodes),
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

fn linear_author_from_id_user(
    user: &linear_issues_by_ids::LinearIssuesByIdsIssuesNodesCreator,
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

fn linear_comment_author_from_id_user(
    user: &linear_issues_by_ids::LinearIssuesByIdsIssuesNodesCommentsNodesUser,
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

fn linear_comments_from_id_connection(
    comments: &[linear_issues_by_ids::LinearIssuesByIdsIssuesNodesCommentsNodes],
) -> Vec<IssueComment> {
    let mut collected = Vec::new();
    for comment in comments {
        collected.push(IssueComment {
            id: comment.id.clone(),
            body: comment.body.clone(),
            author: comment
                .user
                .as_ref()
                .map(linear_comment_author_from_id_user),
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
                    .map(linear_child_comment_author_from_id_user),
                url: Some(child.url.clone()),
                created_at: parse_rfc3339(&child.created_at),
                updated_at: parse_rfc3339(&child.updated_at),
            });
        }
    }
    collected
}

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

fn linear_child_comment_author_from_id_user(
    user: &linear_issues_by_ids::LinearIssuesByIdsIssuesNodesCommentsNodesChildrenNodesUser,
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

fn parse_linear_priority(priority: f64) -> Option<i32> {
    if priority.fract() != 0.0 {
        return None;
    }
    if priority >= i32::MIN as f64 && priority <= i32::MAX as f64 {
        Some(priority as i32)
    } else {
        None
    }
}

fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use polyphony_core::Error as CoreError;

    use crate::{LINEAR_HTTP_TIMEOUT, next_page_cursor};

    #[test]
    fn next_page_cursor_requires_end_cursor_when_more_pages_exist() {
        let error = next_page_cursor(true, None).unwrap_err();

        assert!(matches!(
            error,
            CoreError::Adapter(message) if message == "linear_missing_end_cursor"
        ));
    }

    #[test]
    fn next_page_cursor_returns_cursor_when_present() {
        let cursor = next_page_cursor(true, Some("cursor-1".into())).unwrap();

        assert_eq!(cursor.as_deref(), Some("cursor-1"));
    }

    #[test]
    fn next_page_cursor_stops_when_no_more_pages_exist() {
        let cursor = next_page_cursor(false, None).unwrap();

        assert!(cursor.is_none());
    }

    #[test]
    fn linear_http_timeout_matches_spec() {
        assert_eq!(LINEAR_HTTP_TIMEOUT, Duration::from_millis(30_000));
    }
}
