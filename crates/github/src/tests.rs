use chrono::{TimeZone, Utc};
use octocrab::models::AuthorAssociation;
use polyphony_core::DispatchApprovalState;
use reqwest::{
    StatusCode,
    header::{HeaderMap, HeaderValue, RETRY_AFTER},
};

use crate::{
    convert::{
        find_status_field_option, github_issue_approval_state, github_rate_limit_signal,
        parse_rate_limit_reset, parse_retry_after_ms, project_id_from_context,
    },
    fetch_pull_request_events,
    pull_requests::{GithubIssueCommentResponse, find_issue_comment_id_with_marker},
    resolve_project_issue_context, resolve_project_status_field,
    review_events::{
        GithubReviewBranchRef, GithubReviewHeadRef, GithubReviewLabel,
        GithubReviewPullRequestResponse, GithubReviewUser,
        pull_request_review_events_from_responses, should_emit_conflict_event,
    },
};

#[test]
fn project_id_prefers_org_then_user() {
    let data = resolve_project_issue_context::ResponseData {
            repository: None,
            organization: Some(resolve_project_issue_context::ResolveProjectIssueContextOrganization {
                project_v2: Some(resolve_project_issue_context::ResolveProjectIssueContextOrganizationProjectV2 {
                    id: "ORG_PROJECT".into(),
                }),
            }),
            user: Some(resolve_project_issue_context::ResolveProjectIssueContextUser {
                project_v2: Some(resolve_project_issue_context::ResolveProjectIssueContextUserProjectV2 {
                    id: "USER_PROJECT".into(),
                }),
            }),
        };
    assert_eq!(
        project_id_from_context(&data).as_deref(),
        Some("ORG_PROJECT")
    );
}

#[test]
fn finds_status_option_case_insensitively() {
    let nodes = vec![vec![Some(
            resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodes::ProjectV2SingleSelectField(
                resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectField {
                    id: "field-1".into(),
                    name: "Status".into(),
                    options: vec![
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-1".into(),
                            name: "Todo".into(),
                        },
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-2".into(),
                            name: "In Progress".into(),
                        },
                        resolve_project_status_field::ResolveProjectStatusFieldNodeOnProjectV2FieldsNodesOnProjectV2SingleSelectFieldOptions {
                            id: "opt-3".into(),
                            name: "Human Review".into(),
                        },
                    ],
                },
            ),
        )]];

    assert_eq!(
        find_status_field_option(&nodes, "status", "human review"),
        Some(("field-1".into(), "opt-3".into()))
    );
}

#[test]
fn retry_after_header_is_converted_to_milliseconds() {
    let mut headers = HeaderMap::new();
    headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));

    assert_eq!(parse_retry_after_ms(&headers), Some(12_000));
}

#[test]
fn reset_header_is_converted_to_utc_timestamp() {
    let mut headers = HeaderMap::new();
    headers.insert("x-ratelimit-reset", HeaderValue::from_static("1710000000"));

    assert_eq!(
        parse_rate_limit_reset(&headers),
        Utc.timestamp_opt(1_710_000_000, 0).single()
    );
}

#[test]
fn secondary_rate_limit_without_headers_falls_back_to_one_minute() {
    let signal = github_rate_limit_signal(
        "tracker:github",
        StatusCode::TOO_MANY_REQUESTS,
        &HeaderMap::new(),
        None,
    )
    .unwrap();

    assert_eq!(signal.retry_after_ms, Some(60_000));
    assert!(signal.reset_at.is_none());
}

#[test]
fn primary_rate_limit_uses_reset_header_instead_of_guessing_retry_after() {
    let mut headers = HeaderMap::new();
    headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
    headers.insert("x-ratelimit-reset", HeaderValue::from_static("1710000000"));

    let signal =
        github_rate_limit_signal("tracker:github", StatusCode::FORBIDDEN, &headers, None).unwrap();

    assert_eq!(signal.retry_after_ms, None);
    assert_eq!(
        signal.reset_at,
        Utc.timestamp_opt(1_710_000_000, 0).single()
    );
}

#[test]
fn pull_request_review_events_keep_fork_heads_and_set_checkout_refs() {
    let events = pull_request_review_events_from_responses("penso/polyphony", vec![
        GithubReviewPullRequestResponse {
            number: 42,
            title: "Ready".into(),
            html_url: "https://github.com/penso/polyphony/pull/42".into(),
            created_at: Utc.timestamp_opt(1_709_999_000, 0).single().unwrap(),
            updated_at: Utc.timestamp_opt(1_710_000_000, 0).single().unwrap(),
            draft: Some(false),
            user: Some(GithubReviewUser {
                login: "alice".into(),
            }),
            author_association: Some(AuthorAssociation::Collaborator),
            labels: vec![GithubReviewLabel {
                name: "Needs Review".into(),
            }],
            base: GithubReviewBranchRef {
                name: "main".into(),
            },
            head: GithubReviewHeadRef {
                name: "feature/review".into(),
                sha: "abc123".into(),
            },
        },
        GithubReviewPullRequestResponse {
            number: 43,
            title: "Fork".into(),
            html_url: "https://github.com/penso/polyphony/pull/43".into(),
            created_at: Utc.timestamp_opt(1_709_999_001, 0).single().unwrap(),
            updated_at: Utc.timestamp_opt(1_710_000_001, 0).single().unwrap(),
            draft: Some(false),
            user: Some(GithubReviewUser {
                login: "dependabot[bot]".into(),
            }),
            author_association: Some(AuthorAssociation::Contributor),
            labels: Vec::new(),
            base: GithubReviewBranchRef {
                name: "main".into(),
            },
            head: GithubReviewHeadRef {
                name: "fork/review".into(),
                sha: "def456".into(),
            },
        },
    ]);

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].number, 42);
    assert_eq!(events[0].head_sha, "abc123");
    assert_eq!(events[0].author_login.as_deref(), Some("alice"));
    assert_eq!(events[0].approval_state, DispatchApprovalState::Approved);
    assert_eq!(events[0].labels, vec!["needs review"]);
    assert_eq!(events[0].checkout_ref.as_deref(), Some("refs/pull/42/head"));
    assert_eq!(events[1].number, 43);
    assert_eq!(events[1].author_login.as_deref(), Some("dependabot[bot]"));
    assert_eq!(events[1].approval_state, DispatchApprovalState::Approved);
    assert_eq!(events[1].checkout_ref.as_deref(), Some("refs/pull/43/head"));
}

#[test]
fn conflict_event_detection_uses_mergeable_and_merge_state_status() {
    assert!(should_emit_conflict_event(
        &fetch_pull_request_events::MergeableState::CONFLICTING,
        &fetch_pull_request_events::MergeStateStatus::CLEAN,
    ));
    assert!(should_emit_conflict_event(
        &fetch_pull_request_events::MergeableState::MERGEABLE,
        &fetch_pull_request_events::MergeStateStatus::DIRTY,
    ));
    assert!(!should_emit_conflict_event(
        &fetch_pull_request_events::MergeableState::MERGEABLE,
        &fetch_pull_request_events::MergeStateStatus::CLEAN,
    ));
}

#[test]
fn find_issue_comment_id_with_marker_matches_existing_review_comment() {
    let comments = vec![
        GithubIssueCommentResponse {
            id: 1,
            body: Some("hello".into()),
        },
        GithubIssueCommentResponse {
            id: 2,
            body: Some(
                "review\n\n<!-- polyphony:pr-review github penso/polyphony#42 sha=abc123 -->"
                    .into(),
            ),
        },
    ];

    assert_eq!(
        find_issue_comment_id_with_marker(
            &comments,
            "<!-- polyphony:pr-review github penso/polyphony#42 sha=abc123 -->",
        ),
        Some(2)
    );
}

#[test]
fn github_issue_approval_waits_for_outsiders_and_approves_collaborators() {
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::Owner), Some("repo-owner")),
        DispatchApprovalState::Approved
    );
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::Collaborator), Some("teammate"),),
        DispatchApprovalState::Approved
    );
    assert_eq!(
        github_issue_approval_state(
            Some(&AuthorAssociation::Contributor),
            Some("dependabot[bot]"),
        ),
        DispatchApprovalState::Approved
    );
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::Contributor), Some("outsider")),
        DispatchApprovalState::Waiting
    );
    assert_eq!(
        github_issue_approval_state(Some(&AuthorAssociation::FirstTimer), Some("newcomer")),
        DispatchApprovalState::Waiting
    );
}
