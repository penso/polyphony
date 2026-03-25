use crate::convert::*;

#[test]
fn normalize_state_opened_to_todo() {
    assert_eq!(normalize_issue_state("opened"), "Todo");
}

#[test]
fn normalize_state_closed_to_done() {
    assert_eq!(normalize_issue_state("closed"), "Done");
}

#[test]
fn normalize_state_is_case_insensitive() {
    assert_eq!(normalize_issue_state("Opened"), "Todo");
    assert_eq!(normalize_issue_state("CLOSED"), "Done");
}

#[test]
fn normalize_state_unknown_defaults_to_todo() {
    assert_eq!(normalize_issue_state("unknown"), "Todo");
}

#[test]
fn wants_open_detects_non_terminal_states() {
    assert!(wants_open_states(&["Todo".into(), "In Progress".into()]));
    assert!(!wants_open_states(&["Done".into(), "Closed".into()]));
}

#[test]
fn wants_closed_detects_terminal_states() {
    assert!(wants_closed_states(&["Done".into()]));
    assert!(!wants_closed_states(&["Todo".into()]));
}

#[test]
fn is_terminalish_matches_known_terminal_states() {
    assert!(is_terminalish_state("done"));
    assert!(is_terminalish_state("closed"));
    assert!(is_terminalish_state("cancelled"));
    assert!(is_terminalish_state("canceled"));
    assert!(is_terminalish_state("duplicate"));
    assert!(!is_terminalish_state("todo"));
    assert!(!is_terminalish_state("in progress"));
}

#[test]
fn parse_gitlab_time_parses_rfc3339() {
    let dt = parse_gitlab_time("2025-03-25T10:30:00Z").unwrap();
    assert_eq!(dt.year(), 2025);
    assert_eq!(dt.month(), 3);
    assert_eq!(dt.day(), 25);
}

#[test]
fn parse_gitlab_time_returns_none_for_invalid() {
    assert!(parse_gitlab_time("not-a-date").is_none());
}

#[test]
fn parse_rate_limit_reset_from_epoch() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("ratelimit-reset", "1711360200".parse().unwrap());
    let dt = parse_rate_limit_reset(&headers).unwrap();
    assert_eq!(dt.timestamp(), 1_711_360_200);
}

#[test]
fn parse_retry_after_converts_to_ms() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "30".parse().unwrap());
    assert_eq!(parse_retry_after_ms(&headers), Some(30_000));
}

#[test]
fn capture_rate_limit_headers_extracts_values() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("ratelimit-remaining", "750".parse().unwrap());
    headers.insert("ratelimit-limit", "800".parse().unwrap());
    headers.insert("ratelimit-reset", "1711360200".parse().unwrap());
    let captured = capture_rate_limit_headers(&headers).unwrap();
    assert_eq!(captured.remaining, 750);
    assert_eq!(captured.limit, 800);
    assert!(captured.reset_at.is_some());
}

#[test]
fn capture_rate_limit_headers_returns_none_when_missing() {
    let headers = reqwest::header::HeaderMap::new();
    assert!(capture_rate_limit_headers(&headers).is_none());
}

#[test]
fn gitlab_rate_limit_signal_ignores_non_429() {
    let headers = reqwest::header::HeaderMap::new();
    assert!(gitlab_rate_limit_signal("test", reqwest::StatusCode::OK, &headers).is_none());
    assert!(gitlab_rate_limit_signal("test", reqwest::StatusCode::FORBIDDEN, &headers).is_none());
}

#[test]
fn gitlab_rate_limit_signal_detects_429() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "60".parse().unwrap());
    let signal = gitlab_rate_limit_signal(
        "tracker:gitlab",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        &headers,
    )
    .unwrap();
    assert_eq!(signal.component, "tracker:gitlab");
    assert_eq!(signal.status_code, Some(429));
    assert_eq!(signal.retry_after_ms, Some(60_000));
}

use chrono::Datelike;
