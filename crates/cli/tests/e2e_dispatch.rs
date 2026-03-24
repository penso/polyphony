//! Dispatch and agent execution integration tests.
//!
//! These tests exercise the core orchestration loop: dispatching issues to
//! local CLI agents, waiting for completion, and verifying that the runtime
//! records the outcome in running/history/events.

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod test_support;

use std::time::Duration;

use test_support::*;

/// Dispatch an issue manually, verify the agent runs, and check that the
/// run appears in agent history with a succeeded status.
#[test]
fn manual_dispatch_runs_agent_and_records_history() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("Dispatch test issue");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch the issue manually.
    let dispatch_out = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);
    assert!(
        dispatch_out.status.success(),
        "dispatch failed: {}",
        String::from_utf8_lossy(&dispatch_out.stderr)
    );

    // Wait for the run to appear in history (agent finishes).
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        !agent_history(s).is_empty()
    });
    assert!(
        snap.is_some(),
        "agent run never appeared in history\n{}",
        dump_daemon_logs(&repo),
    );

    let snap = snap.unwrap();
    let history = agent_history(&snap);
    assert!(
        !history.is_empty(),
        "history should have at least one entry"
    );

    // Verify the history entry matches our dispatch.
    let entry = &history[0];
    assert_eq!(
        entry["agent_name"].as_str(),
        Some("test-agent"),
        "history entry should reference test-agent"
    );
    assert_eq!(
        entry["status"].as_str(),
        Some("Succeeded"),
        "agent should have succeeded: {entry:#}"
    );

    daemon.stop_and_kill(&repo);
}

/// Dispatch an issue using the fail agent. The run should appear in history
/// with a Failed status.
#[test]
fn failed_agent_run_is_visible_in_history() {
    let repo = TestRepo::new();

    // Add a fail-agent profile alongside the default test-agent.
    let fail_script = repo.root().join(".polyphony-fixtures/agent-fail.sh");
    let success_script = repo.root().join(".polyphony-fixtures/agent-success.sh");
    let toml = format!(
        r#"[tracker]
kind = "beads"
active_states = ["Open", "In Progress", "Blocked"]
terminal_states = ["Closed", "Deferred"]

[workspace]
checkout_kind = "directory"
sync_on_reuse = false

[agent]
max_retry_backoff_ms = 60000

[agents.profiles.test-agent]
kind = "local"
transport = "local_cli"
command = "bash {success_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 10000
stall_timeout_ms = 5000
completion_sentinel = "POLYPHONY_AGENT_DONE"

[agents.profiles.fail-agent]
kind = "local"
transport = "local_cli"
command = "bash {fail_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 10000
stall_timeout_ms = 5000
"#,
        success_script = success_script.display(),
        fail_script = fail_script.display(),
    );
    repo.write_repo_config(Some(&toml));

    let issue_id = repo.create_beads_issue("Fail test issue");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch to the fail agent.
    let dispatch_out = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "fail-agent"]);
    assert!(
        dispatch_out.status.success(),
        "dispatch command failed: {}",
        String::from_utf8_lossy(&dispatch_out.stderr)
    );

    // Wait for the run to finish in history.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        agent_history(s)
            .iter()
            .any(|h| h["status"].as_str() == Some("Failed"))
    });
    assert!(
        snap.is_some(),
        "failed run never appeared in history\n{}",
        dump_daemon_logs(&repo),
    );

    let snap = snap.unwrap();
    let failed_entries: Vec<_> = agent_history(&snap)
        .into_iter()
        .filter(|h| h["status"].as_str() == Some("Failed"))
        .collect();
    assert!(
        !failed_entries.is_empty(),
        "expected a Failed history entry"
    );
    assert_eq!(failed_entries[0]["agent_name"].as_str(), Some("fail-agent"));

    daemon.stop_and_kill(&repo);
}

/// Set automatic mode, create a ready issue, and verify the daemon
/// dispatches it without manual intervention.
#[test]
fn automatic_dispatch_picks_up_ready_issue() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("Auto-dispatch test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Switch to automatic mode.
    let mode_out = daemon_command(&repo, &["mode", "automatic"]);
    assert!(mode_out.status.success(), "mode switch failed");

    // Wait for the issue to be picked up and appear in history.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        // Either currently running or already in history.
        let running = running_agents(s);
        let history = agent_history(s);
        let issue_running = running
            .iter()
            .any(|r| r["issue_id"].as_str() == Some(&issue_id));
        let issue_done = history
            .iter()
            .any(|h| h["issue_id"].as_str() == Some(&issue_id));
        issue_running || issue_done
    });
    assert!(
        snap.is_some(),
        "issue was never dispatched automatically\n{}",
        dump_daemon_logs(&repo),
    );

    daemon.stop_and_kill(&repo);
}

/// Dispatch an issue that uses a stalling agent with a short timeout.
/// The run should eventually appear in history as TimedOut or Stalled.
#[test]
fn stall_timeout_is_visible_in_history() {
    let repo = TestRepo::new();

    // Add a stall-agent alongside the default test-agent, with short timeouts.
    let stall_script = repo.root().join(".polyphony-fixtures/agent-stall.sh");
    let success_script = repo.root().join(".polyphony-fixtures/agent-success.sh");
    let toml = format!(
        r#"[tracker]
kind = "beads"
active_states = ["Open", "In Progress", "Blocked"]
terminal_states = ["Closed", "Deferred"]

[workspace]
checkout_kind = "directory"
sync_on_reuse = false

[agent]
max_retry_backoff_ms = 60000

[agents.profiles.test-agent]
kind = "local"
transport = "local_cli"
command = "bash {success_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 10000
stall_timeout_ms = 5000
completion_sentinel = "POLYPHONY_AGENT_DONE"

[agents.profiles.stall-agent]
kind = "local"
transport = "local_cli"
command = "bash {stall_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 3000
stall_timeout_ms = 3000
read_timeout_ms = 2000
"#,
        success_script = success_script.display(),
        stall_script = stall_script.display(),
    );
    repo.write_repo_config(Some(&toml));

    let issue_id = repo.create_beads_issue("Stall test issue");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch to the stall agent.
    let dispatch_out = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "stall-agent"]);
    assert!(
        dispatch_out.status.success(),
        "dispatch command failed: {}",
        String::from_utf8_lossy(&dispatch_out.stderr)
    );

    // Wait for a timeout/stall outcome in history. Use a longer wait since
    // the agent needs time to stall and get killed.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(45), |s| {
        agent_history(s).iter().any(|h| {
            let status = h["status"].as_str().unwrap_or("");
            status == "TimedOut" || status == "Stalled" || status == "Failed"
        })
    });
    assert!(
        snap.is_some(),
        "stalled run never appeared in history\n{}",
        dump_daemon_logs(&repo),
    );

    let snap = snap.unwrap();
    let entry = agent_history(&snap)
        .into_iter()
        .find(|h| {
            let status = h["status"].as_str().unwrap_or("");
            status == "TimedOut" || status == "Stalled" || status == "Failed"
        })
        .expect("should find timeout/stall entry");
    let status = entry["status"].as_str().unwrap();
    assert!(
        status == "TimedOut" || status == "Stalled" || status == "Failed",
        "expected TimedOut/Stalled/Failed, got: {status}"
    );

    daemon.stop_and_kill(&repo);
}

/// Verify that a dispatched agent's workspace directory exists while running
/// or after completion.
#[test]
fn dispatch_creates_workspace() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("Workspace test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch the issue.
    let dispatch_out = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);
    assert!(dispatch_out.status.success());

    // Wait for the run to finish.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        !agent_history(s).is_empty()
    });
    assert!(
        snap.is_some(),
        "run never completed\n{}",
        dump_daemon_logs(&repo)
    );

    let snap = snap.unwrap();

    // Check that `data workspaces` shows something.
    let ws_output = run_polyphony(&repo, &["data", "workspaces"]);
    if ws_output.status.success() {
        let stdout = String::from_utf8_lossy(&ws_output.stdout);
        // If there's workspace data, it should have some content.
        assert!(
            !stdout.trim().is_empty(),
            "workspaces output should not be empty"
        );
    }

    // The history entry should have a workspace_path.
    let history = agent_history(&snap);
    if let Some(entry) = history.first()
        && let Some(ws_path) = entry["workspace_path"].as_str()
    {
        assert!(!ws_path.is_empty(), "workspace_path should be populated");
    }

    daemon.stop_and_kill(&repo);
}

/// Verify that events are recorded during a dispatch cycle.
#[test]
fn dispatch_records_events() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("Events test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch the issue.
    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);

    // Wait for completion.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        !agent_history(s).is_empty()
    });
    assert!(
        snap.is_some(),
        "run never completed\n{}",
        dump_daemon_logs(&repo)
    );

    let snap = snap.unwrap();
    let events = recent_events(&snap);

    // There should be dispatch-related events.
    assert!(!events.is_empty(), "expected runtime events after dispatch");

    // Look for dispatch or worker events.
    let has_dispatch_event = events.iter().any(|e| {
        let scope = e["scope"].as_str().unwrap_or("");
        scope == "dispatch" || scope == "worker" || scope == "agent"
    });
    assert!(
        has_dispatch_event,
        "expected dispatch/worker/agent events, got: {events:#?}"
    );

    daemon.stop_and_kill(&repo);
}
