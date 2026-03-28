//! Reset command end-to-end coverage.

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod test_support;

use std::time::Duration;

use test_support::*;

#[test]
fn reset_removes_json_state_and_clears_history() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("Reset state test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);
    let snapshot = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |snapshot| {
        !agent_run_history(snapshot).is_empty()
    });
    assert!(
        snapshot.is_some(),
        "run never completed\n{}",
        dump_daemon_logs(&repo)
    );

    daemon.stop_and_kill(&repo);

    let state_path = repo.root().join(".polyphony/state.json");
    assert!(
        state_path.exists(),
        "expected state file at {}",
        state_path.display()
    );

    let reset = run_polyphony_json(&repo, &["reset"]);
    assert_eq!(reset["backend"].as_str(), Some("json"));
    assert_eq!(
        reset["removed_paths"]
            .as_array()
            .map(|values| values.len())
            .unwrap_or_default(),
        1
    );
    assert!(
        !state_path.exists(),
        "reset should remove {}",
        state_path.display()
    );

    let history = run_polyphony_json(&repo, &["data", "history"]);
    assert_eq!(history.as_array().map(Vec::len), Some(0));

    let runs = run_polyphony_json(&repo, &["data", "runs"]);
    assert_eq!(runs.as_array().map(Vec::len), Some(0));
}

#[test]
fn reset_refuses_to_run_while_daemon_is_active() {
    let repo = TestRepo::new();

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    let output = run_polyphony(&repo, &["reset"]);
    assert!(
        !output.status.success(),
        "reset should fail while daemon is running"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("daemon is running"),
        "expected daemon warning, got:\n{stderr}"
    );

    daemon.stop_and_kill(&repo);
}
