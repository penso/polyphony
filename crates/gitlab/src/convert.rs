use chrono::{DateTime, Utc};
use polyphony_core::{
    Issue, IssueApprovalState, IssueAuthor, IssueComment, IssueStateUpdate, RateLimitSignal,
};
use reqwest::{Response, StatusCode, header::HeaderMap};

use crate::fetch_issue_by_iid;

pub(crate) fn to_issue(
    issue: &fetch_issue_by_iid::FetchIssueByIidProjectIssue,
    _project_path: &str,
) -> Issue {
    let state = normalize_issue_state(&format!("{:?}", issue.state));
    let iid = &issue.iid;
    let notes: Vec<IssueComment> = issue
        .notes
        .as_ref()
        .and_then(|conn| conn.nodes.as_ref())
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| !n.system)
                .map(gitlab_note_to_comment)
                .collect()
        })
        .unwrap_or_default();

    Issue {
        id: iid.clone(),
        identifier: format!("#{iid}"),
        title: issue.title.clone(),
        description: issue.description.clone(),
        priority: None,
        state,
        branch_name: Some(format!("{iid}-{}", slugify(&issue.title))),
        url: Some(issue.web_url.clone()),
        author: issue.author.as_ref().map(gitlab_user_to_author),
        labels: extract_labels(&issue.labels),
        comments: notes,
        blocked_by: Vec::new(),
        approval_state: issue
            .author
            .as_ref()
            .map(gitlab_approval_state)
            .unwrap_or(IssueApprovalState::Waiting),
        parent_id: None,
        created_at: parse_gitlab_time(&issue.created_at),
        updated_at: parse_gitlab_time(&issue.updated_at),
    }
}

pub(crate) fn list_node_to_issue(
    node: &crate::fetch_project_issues::FetchProjectIssuesProjectIssuesNodes,
    _project_path: &str,
) -> Issue {
    let state = normalize_issue_state(&format!("{:?}", node.state));
    let iid = &node.iid;
    Issue {
        id: iid.clone(),
        identifier: format!("#{iid}"),
        title: node.title.clone(),
        description: node.description.clone(),
        priority: None,
        state,
        branch_name: Some(format!("{iid}-{}", slugify(&node.title))),
        url: Some(node.web_url.clone()),
        author: node.author.as_ref().map(gitlab_list_user_to_author),
        labels: node
            .labels
            .as_ref()
            .and_then(|conn| conn.nodes.as_ref())
            .map(|nodes| nodes.iter().map(|l| l.title.to_ascii_lowercase()).collect())
            .unwrap_or_default(),
        comments: Vec::new(),
        blocked_by: Vec::new(),
        approval_state: if node.author.is_some() {
            IssueApprovalState::Approved
        } else {
            IssueApprovalState::Waiting
        },
        parent_id: None,
        created_at: parse_gitlab_time(&node.created_at),
        updated_at: parse_gitlab_time(&node.updated_at),
    }
}

pub(crate) fn issue_to_state_update(issue: &Issue) -> IssueStateUpdate {
    IssueStateUpdate {
        id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        state: issue.state.clone(),
        updated_at: issue.updated_at,
    }
}

fn gitlab_user_to_author(
    user: &fetch_issue_by_iid::FetchIssueByIidProjectIssueAuthor,
) -> IssueAuthor {
    IssueAuthor {
        id: Some(user.username.clone()),
        username: Some(user.username.clone()),
        display_name: Some(user.name.clone()),
        role: if user.bot {
            Some("bot".into())
        } else {
            Some("member".into())
        },
        trust_level: if user.bot {
            Some("trusted_bot".into())
        } else {
            Some("trusted_member".into())
        },
        url: Some(user.web_url.clone()),
    }
}

fn gitlab_list_user_to_author(
    user: &crate::fetch_project_issues::FetchProjectIssuesProjectIssuesNodesAuthor,
) -> IssueAuthor {
    IssueAuthor {
        id: Some(user.username.clone()),
        username: Some(user.username.clone()),
        display_name: Some(user.name.clone()),
        role: if user.bot {
            Some("bot".into())
        } else {
            Some("member".into())
        },
        trust_level: if user.bot {
            Some("trusted_bot".into())
        } else {
            Some("trusted_member".into())
        },
        url: Some(user.web_url.clone()),
    }
}

fn gitlab_note_to_comment(
    note: &fetch_issue_by_iid::FetchIssueByIidProjectIssueNotesNodes,
) -> IssueComment {
    IssueComment {
        id: note.id.clone(),
        body: note.body.clone(),
        author: Some(IssueAuthor {
            id: Some(note.author.username.clone()),
            username: Some(note.author.username.clone()),
            display_name: Some(note.author.name.clone()),
            role: if note.author.bot {
                Some("bot".into())
            } else {
                Some("member".into())
            },
            trust_level: if note.author.bot {
                Some("trusted_bot".into())
            } else {
                Some("trusted_member".into())
            },
            url: Some(note.author.web_url.clone()),
        }),
        url: None,
        created_at: parse_gitlab_time(&note.created_at),
        updated_at: parse_gitlab_time(&note.updated_at),
    }
}

fn gitlab_approval_state(
    _user: &fetch_issue_by_iid::FetchIssueByIidProjectIssueAuthor,
) -> IssueApprovalState {
    // GitLab doesn't expose author_association like GitHub.
    // Default to Approved; a future enhancement could use project membership API.
    IssueApprovalState::Approved
}

fn extract_labels(
    labels: &Option<fetch_issue_by_iid::FetchIssueByIidProjectIssueLabels>,
) -> Vec<String> {
    labels
        .as_ref()
        .and_then(|conn| conn.nodes.as_ref())
        .map(|nodes| nodes.iter().map(|l| l.title.to_ascii_lowercase()).collect())
        .unwrap_or_default()
}

pub(crate) fn normalize_issue_state(state: &str) -> String {
    match state.to_ascii_lowercase().as_str() {
        "opened" => "Todo".into(),
        "closed" => "Done".into(),
        _ => "Todo".into(),
    }
}

pub(crate) fn wants_open_states(states: &[String]) -> bool {
    states.iter().any(|s| !is_terminalish_state(s))
}

pub(crate) fn wants_closed_states(states: &[String]) -> bool {
    states.iter().any(|s| is_terminalish_state(s))
}

pub(crate) fn is_terminalish_state(state: &str) -> bool {
    matches!(
        state.to_ascii_lowercase().as_str(),
        "done" | "closed" | "cancelled" | "canceled" | "duplicate"
    )
}

pub(crate) fn parse_gitlab_time(time: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn slugify(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ---------------------------------------------------------------------------
// Rate limit helpers
// ---------------------------------------------------------------------------

pub(crate) fn gitlab_rate_limit_signal_from_response(
    component: &str,
    response: &Response,
) -> Option<RateLimitSignal> {
    gitlab_rate_limit_signal(component, response.status(), response.headers())
}

pub(crate) fn gitlab_rate_limit_signal(
    component: &str,
    status: StatusCode,
    headers: &HeaderMap,
) -> Option<RateLimitSignal> {
    if status.as_u16() != 429 {
        return None;
    }
    let retry_after_ms = parse_retry_after_ms(headers).or(Some(60_000));
    Some(RateLimitSignal {
        component: component.into(),
        reason: format!("gitlab api {status}"),
        limited_at: Utc::now(),
        retry_after_ms,
        reset_at: parse_rate_limit_reset(headers),
        status_code: Some(status.as_u16()),
        raw: None,
    })
}

pub(crate) fn parse_retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(|s| s.saturating_mul(1_000))
}

pub(crate) fn parse_rate_limit_reset(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let epoch = headers
        .get("ratelimit-reset")?
        .to_str()
        .ok()?
        .parse::<i64>()
        .ok()?;
    DateTime::from_timestamp(epoch, 0)
}

#[derive(Debug)]
pub(crate) struct CapturedRateLimit {
    pub remaining: u64,
    pub limit: u64,
    pub reset_at: Option<DateTime<Utc>>,
}

pub(crate) fn capture_rate_limit_headers(headers: &HeaderMap) -> Option<CapturedRateLimit> {
    let remaining = headers
        .get("ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())?;
    let limit = headers
        .get("ratelimit-limit")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())?;
    let reset_at = parse_rate_limit_reset(headers);
    Some(CapturedRateLimit {
        remaining,
        limit,
        reset_at,
    })
}
