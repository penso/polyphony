//! Daemon control integration tests.
//!
//! These tests exercise daemon start/stop/refresh/mode/dispatch/snapshot
//! commands against a real temporary repo with beads.

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod test_support;

use std::time::Duration;

use test_support::*;

/// Verify that the daemon can start, report a snapshot, and stop cleanly.
#[test]
fn daemon_lifecycle_start_snapshot_stop() {
    let repo = TestRepo::new();
    repo.create_beads_issue("Lifecycle test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready in time\n{}",
        dump_daemon_logs(&repo),
    );

    // Snapshot should be available.
    let snap = daemon_snapshot(&repo).expect("daemon should return snapshot");
    assert!(snap["visible_issues"].is_array());

    // Stop and verify clean exit.
    daemon.stop_and_kill(&repo);

    // After stop, daemon snapshot should fail.
    assert!(
        daemon_snapshot(&repo).is_none(),
        "daemon should not respond after stop"
    );
}

/// Verify that `daemon snapshot` returns valid JSON with issues.
#[test]
fn daemon_snapshot_returns_valid_json() {
    let repo = TestRepo::new();
    repo.create_beads_issue("Snapshot via daemon");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    let snapshot = wait_for_daemon_snapshot(&repo, Duration::from_secs(10), |s| {
        snapshot_is_ready(s)
    });
    assert!(snapshot.is_some(), "daemon snapshot never became ready");

    let snapshot = snapshot.unwrap();
    assert!(snapshot["visible_issues"].is_array());
    assert!(!visible_issues(&snapshot).is_empty(), "no issues in daemon snapshot");

    daemon.stop_and_kill(&repo);
}

/// Verify that `daemon refresh` triggers a new tracker poll.
#[test]
fn daemon_refresh_triggers_new_poll() {
    let repo = TestRepo::new();
    repo.create_beads_issue("Refresh test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Wait for initial poll to complete.
    let snap1 = wait_for_daemon_snapshot(&repo, Duration::from_secs(10), |s| {
        snapshot_is_ready(s)
    })
    .expect("initial snapshot never ready");

    let _poll1 = snap1["cadence"]["last_tracker_poll_at"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Create a new issue after initial poll.
    repo.create_beads_issue("Post-refresh issue");

    // Send refresh.
    let refresh_out = daemon_command(&repo, &["refresh"]);
    assert!(
        refresh_out.status.success(),
        "refresh failed: {}",
        String::from_utf8_lossy(&refresh_out.stderr)
    );

    // Wait for the new issue to appear in a subsequent poll.
    // The refresh triggers an immediate poll, but we may need a second refresh
    // if the first poll raced with the issue creation.
    let new_snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(10), |s| {
        visible_issues(s).len() >= 2
    });

    // If the issue didn't appear yet, send another refresh and try again.
    let new_snap = new_snap.or_else(|| {
        let _ = daemon_command(&repo, &["refresh"]);
        wait_for_daemon_snapshot(&repo, Duration::from_secs(10), |s| {
            visible_issues(s).len() >= 2
        })
    });

    assert!(
        new_snap.is_some(),
        "expected at least 2 issues after refresh"
    );

    daemon.stop_and_kill(&repo);
}

/// Verify that `daemon mode` can switch between modes.
#[test]
fn daemon_mode_switch() {
    let repo = TestRepo::new();

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Switch to automatic.
    let mode_out = daemon_command(&repo, &["mode", "automatic"]);
    assert!(
        mode_out.status.success(),
        "mode switch failed: {}",
        String::from_utf8_lossy(&mode_out.stderr)
    );

    // Verify mode changed.
    let snap2 = wait_for_daemon_snapshot(&repo, Duration::from_secs(5), |s| {
        dispatch_mode(s) == Some("automatic")
    });
    assert!(snap2.is_some(), "mode did not switch to automatic");

    // Switch to manual.
    let _ = daemon_command(&repo, &["mode", "manual"]);
    let snap3 = wait_for_daemon_snapshot(&repo, Duration::from_secs(5), |s| {
        dispatch_mode(s) == Some("manual")
    });
    assert!(snap3.is_some(), "mode did not switch to manual");

    daemon.stop_and_kill(&repo);
}

/// Verify daemon shutdown is clean — process exits.
#[test]
fn daemon_shutdown_is_clean() {
    let repo = TestRepo::new();

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // stop_and_kill waits for exit, then force-kills if needed.
    daemon.stop_and_kill(&repo);

    // After stop, daemon should not respond.
    assert!(
        daemon_snapshot(&repo).is_none(),
        "daemon should not respond after stop"
    );
}
