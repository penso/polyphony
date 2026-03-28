use crate::{prelude::*, *};

pub(crate) fn to_issue(issue: GithubIssue, comments: Vec<GithubComment>) -> Issue {
    let state = normalize_issue_state(&issue);
    let approval_state = github_issue_approval_state(
        issue.author_association.as_ref(),
        Some(issue.user.login.as_str()),
    );
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
        approval_state,
        parent_id: None,
        created_at: Some(issue.created_at.with_timezone(&Utc)),
        updated_at: Some(issue.updated_at.with_timezone(&Utc)),
    }
}

pub(crate) fn github_comment(comment: GithubComment) -> IssueComment {
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

pub(crate) fn github_author(
    author: &Author,
    association: Option<&AuthorAssociation>,
) -> IssueAuthor {
    IssueAuthor {
        id: Some(author.id.to_string()),
        username: Some(author.login.clone()),
        display_name: author.name.clone().or_else(|| Some(author.login.clone())),
        role: association.map(github_role),
        trust_level: association.map(github_trust_level),
        url: Some(author.html_url.to_string()),
    }
}

pub(crate) fn github_role(association: &AuthorAssociation) -> String {
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

pub(crate) fn github_trust_level(association: &AuthorAssociation) -> String {
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

pub(crate) fn github_issue_approval_state(
    association: Option<&AuthorAssociation>,
    author_login: Option<&str>,
) -> DispatchApprovalState {
    if github_login_is_auto_approved(author_login) {
        return DispatchApprovalState::Approved;
    }

    match association {
        Some(
            AuthorAssociation::Owner | AuthorAssociation::Member | AuthorAssociation::Collaborator,
        ) => DispatchApprovalState::Approved,
        Some(
            AuthorAssociation::Contributor
            | AuthorAssociation::FirstTimer
            | AuthorAssociation::FirstTimeContributor
            | AuthorAssociation::Mannequin
            | AuthorAssociation::None
            | AuthorAssociation::Other(_),
        )
        | None => DispatchApprovalState::Waiting,
        Some(_) => DispatchApprovalState::Waiting,
    }
}

fn github_login_is_auto_approved(author_login: Option<&str>) -> bool {
    author_login.is_some_and(|login| login.eq_ignore_ascii_case("dependabot[bot]"))
}

pub(crate) fn normalize_issue_state(issue: &GithubIssue) -> String {
    if issue.state == octocrab::models::IssueState::Open {
        "Todo".into()
    } else {
        "Done".into()
    }
}

pub(crate) fn wants_open_states(states: &[String]) -> bool {
    states.iter().any(|state| !is_terminalish_state(state))
}

pub(crate) fn wants_closed_states(states: &[String]) -> bool {
    states.iter().any(|state| is_terminalish_state(state))
}

pub(crate) fn is_terminalish_state(state: &str) -> bool {
    matches!(
        state.to_ascii_lowercase().as_str(),
        "done" | "closed" | "cancelled" | "canceled" | "duplicate"
    )
}

pub(crate) fn split_repo(repository: &str) -> Result<(String, String), CoreError> {
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

pub(crate) fn project_id_from_context(
    data: &resolve_project_issue_context::ResponseData,
) -> Option<String> {
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

pub(crate) fn project_field_nodes(
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

pub(crate) fn find_status_field_option(
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

pub(crate) fn map_github_error(error: octocrab::Error) -> CoreError {
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

pub(crate) fn github_rate_limit_signal_from_response(
    component: &str,
    response: &Response,
) -> Option<RateLimitSignal> {
    github_rate_limit_signal(component, response.status(), response.headers(), None)
}

pub(crate) fn github_rate_limit_signal(
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

pub(crate) fn github_rate_limit_reason(status: StatusCode, message: Option<&str>) -> String {
    match message.map(str::trim).filter(|message| !message.is_empty()) {
        Some(message) => format!("github api {status}: {message}"),
        None => format!("github api {status}"),
    }
}

pub(crate) fn parse_retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1_000))
}

pub(crate) fn parse_rate_limit_reset(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let reset_epoch = headers
        .get("x-ratelimit-reset")?
        .to_str()
        .ok()?
        .parse::<i64>()
        .ok()?;
    DateTime::from_timestamp(reset_epoch, 0)
}

pub(crate) fn is_primary_rate_limit(headers: &HeaderMap) -> bool {
    headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == "0")
        .unwrap_or(false)
}
