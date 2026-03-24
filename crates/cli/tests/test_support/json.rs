use serde_json::Value;

/// Extract the "visible_issues" array from a snapshot.
pub fn visible_issues(snapshot: &Value) -> Vec<&Value> {
    snapshot["visible_issues"]
        .as_array()
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

/// Find an issue in the visible_issues array by substring match on issue_identifier.
pub fn find_issue_by_id<'a>(snapshot: &'a Value, id_substring: &str) -> Option<&'a Value> {
    visible_issues(snapshot).into_iter().find(|issue| {
        issue["issue_identifier"]
            .as_str()
            .is_some_and(|id| id.contains(id_substring))
            || issue["issue_id"]
                .as_str()
                .is_some_and(|id| id.contains(id_substring))
    })
}

/// Extract the "running" array from a snapshot.
pub fn running_agents(snapshot: &Value) -> Vec<&Value> {
    snapshot["running"]
        .as_array()
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

/// Extract the "agent_history" array from a snapshot.
pub fn agent_history(snapshot: &Value) -> Vec<&Value> {
    snapshot["agent_history"]
        .as_array()
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

/// Extract the "recent_events" array from a snapshot.
pub fn recent_events(snapshot: &Value) -> Vec<&Value> {
    snapshot["recent_events"]
        .as_array()
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

/// Check if the snapshot indicates the tracker has polled at least once.
pub fn tracker_has_polled(snapshot: &Value) -> bool {
    snapshot["cadence"]["last_tracker_poll_at"]
        .as_str()
        .is_some()
}

/// Check if loading is complete (no active loading flags).
pub fn loading_complete(snapshot: &Value) -> bool {
    let loading = &snapshot["loading"];
    !loading["fetching_issues"].as_bool().unwrap_or(true)
        && !loading["fetching_budgets"].as_bool().unwrap_or(true)
        && !loading["fetching_models"].as_bool().unwrap_or(true)
        && !loading["reconciling"].as_bool().unwrap_or(true)
}

/// Check if the snapshot is "ready" (tracker polled and loading complete).
pub fn snapshot_is_ready(snapshot: &Value) -> bool {
    tracker_has_polled(snapshot) && loading_complete(snapshot)
}

/// Get the dispatch_mode from a snapshot.
pub fn dispatch_mode(snapshot: &Value) -> Option<&str> {
    snapshot["dispatch_mode"].as_str()
}

/// Count visible issues matching a given state.
pub fn count_issues_in_state(snapshot: &Value, state: &str) -> usize {
    visible_issues(snapshot)
        .into_iter()
        .filter(|issue| issue["state"].as_str() == Some(state))
        .count()
}
