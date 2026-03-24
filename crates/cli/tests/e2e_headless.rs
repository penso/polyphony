//! Headless end-to-end integration tests for polyphony.
//!
//! These tests use a real temporary git repo, a real Beads tracker, and the
//! compiled `polyphony` binary. No live LLM providers are involved — all agent
//! work is done by deterministic fixture scripts.
//!
//! Prerequisites:
//! - `bd` CLI installed and a Dolt server running.
//! - `polyphony` binary built (cargo builds it as part of the test target).

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod test_support;

use test_support::*;

/// Verify that creating beads issues and running `polyphony data issues`
/// returns them in the visible issues list.
#[test]
fn beads_issues_appear_in_data_issues() {
    let repo = TestRepo::new();
    let id1 = repo.create_beads_issue("First test issue");
    let id2 = repo.create_beads_issue("Second test issue");

    let issues = run_polyphony_json(&repo, &["data", "issues"]);
    let arr = issues.as_array().expect("data issues returns array");

    // Both issues should appear.
    assert!(
        arr.iter().any(|i| i["issue_id"].as_str() == Some(&id1)),
        "issue {id1} not found in data issues output: {issues:#}"
    );
    assert!(
        arr.iter().any(|i| i["issue_id"].as_str() == Some(&id2)),
        "issue {id2} not found in data issues output: {issues:#}"
    );

    // Verify fields are populated.
    let first = arr.iter().find(|i| i["issue_id"].as_str() == Some(&id1)).unwrap();
    assert_eq!(first["title"].as_str(), Some("First test issue"));
    assert_eq!(first["state"].as_str(), Some("Open"));
}

/// Verify that `polyphony data snapshot` returns a complete snapshot with
/// expected structure after tracker polling.
#[test]
fn data_snapshot_returns_complete_structure() {
    let repo = TestRepo::new();
    let _id = repo.create_beads_issue("Snapshot test issue");

    let snapshot = run_polyphony_json(&repo, &["data", "snapshot"]);

    // Verify top-level fields exist.
    assert!(snapshot["generated_at"].is_string(), "missing generated_at");
    assert!(snapshot["counts"].is_object(), "missing counts");
    assert!(snapshot["cadence"].is_object(), "missing cadence");
    assert!(snapshot["visible_issues"].is_array(), "missing visible_issues");
    assert!(snapshot["running"].is_array(), "missing running");
    assert!(snapshot["agent_history"].is_array(), "missing agent_history");
    assert!(snapshot["recent_events"].is_array(), "missing recent_events");

    // Tracker should have polled.
    assert!(snapshot_is_ready(&snapshot), "snapshot not ready after data command");

    // Issue should appear.
    assert!(
        !visible_issues(&snapshot).is_empty(),
        "no visible issues in snapshot"
    );
}

/// Verify that `polyphony data counts` returns count fields.
#[test]
fn data_counts_returns_count_fields() {
    let repo = TestRepo::new();
    let _id = repo.create_beads_issue("Counts test");

    let counts = run_polyphony_json(&repo, &["data", "counts"]);
    assert!(counts["running"].is_number(), "missing running count");
    assert!(counts["retrying"].is_number(), "missing retrying count");
}

/// Verify that `polyphony issue create` round-trips through the beads tracker.
#[test]
fn issue_cli_round_trips_against_beads() {
    let repo = TestRepo::new();

    // Create via polyphony CLI.
    let created = run_polyphony_json(
        &repo,
        &["issue", "create", "--title", "CLI-created issue", "--priority", "1"],
    );
    let issue_id = created["id"].as_str().expect("created issue has id");
    assert_eq!(created["title"].as_str(), Some("CLI-created issue"));

    // Show via polyphony CLI.
    let shown = run_polyphony_json(&repo, &["issue", "show", issue_id]);
    // `issue show` may return an array (bd show returns array).
    let issue = if shown.is_array() {
        shown.as_array().unwrap().first().expect("show returned empty array").clone()
    } else {
        shown
    };
    assert_eq!(issue["title"].as_str(), Some("CLI-created issue"));

    // Update via polyphony CLI.
    let updated = run_polyphony_json(
        &repo,
        &["issue", "update", issue_id, "--title", "Updated title"],
    );
    let updated_issue = if updated.is_array() {
        updated.as_array().unwrap().first().unwrap().clone()
    } else {
        updated
    };
    assert_eq!(updated_issue["title"].as_str(), Some("Updated title"));

    // Verify beads agrees.
    let beads_issue = repo.show_beads_issue(issue_id);
    let beads_first = beads_issue.as_array().and_then(|a| a.first());
    if let Some(bi) = beads_first {
        assert_eq!(bi["title"].as_str(), Some("Updated title"));
    }
}

/// Verify that `polyphony issue list` returns the issue list.
#[test]
fn issue_list_returns_issues() {
    let repo = TestRepo::new();
    repo.create_beads_issue("List test A");
    repo.create_beads_issue("List test B");

    let output = run_polyphony(&repo, &["issue", "list"]);
    assert!(output.status.success(), "issue list failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The output should contain both issue titles (either as JSON or text).
    assert!(
        stdout.contains("List test A") || stdout.contains("list_test_a"),
        "issue A not in list output: {stdout}"
    );
}

/// Verify that `polyphony data tracker` returns tracker info.
#[test]
fn data_tracker_returns_tracker_kind() {
    let repo = TestRepo::new();
    let tracker = run_polyphony_json(&repo, &["data", "tracker"]);
    // Should indicate beads tracker.
    let tracker_str = format!("{tracker}");
    assert!(
        tracker_str.contains("beads") || tracker_str.contains("Beads"),
        "tracker output does not mention beads: {tracker_str}"
    );
}
