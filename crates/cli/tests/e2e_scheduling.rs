//! Scheduling and dispatch-ordering integration tests.
//!
//! These tests verify the orchestrator's decision-making: which issues to
//! dispatch, in what order, and which to skip.

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod test_support;

use std::time::Duration;

use test_support::*;

/// When multiple issues exist with different priorities and the daemon is in
/// automatic mode with `max_concurrent_agents = 1`, the highest-priority
/// issue (lowest numeric value) should be dispatched first.
#[test]
fn automatic_dispatch_respects_priority_order() {
    let repo = TestRepo::new();

    // Create issues: priority 3 first, then priority 0 (critical).
    // If ordering works, priority 0 should dispatch first despite being
    // created second.
    let low_id = repo.create_beads_issue("Low priority work");
    repo.update_beads_issue_priority(&low_id, 3);

    let high_id = repo.create_beads_issue("Critical priority work");
    repo.update_beads_issue_priority(&high_id, 0);

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Switch to automatic mode.
    let _ = daemon_command(&repo, &["mode", "automatic"]);

    // Wait for at least one run to appear in history.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        !agent_run_history(s).is_empty()
    });
    assert!(
        snap.is_some(),
        "no dispatch happened\n{}",
        dump_daemon_logs(&repo),
    );

    // The first history entry should be for the critical-priority issue.
    let snap = snap.unwrap();
    let history = agent_run_history(&snap);
    assert!(
        !history.is_empty(),
        "history should have at least one entry"
    );

    let first_dispatched = &history[0];
    let first_id = first_dispatched["issue_id"].as_str().unwrap_or("");
    assert_eq!(
        first_id, high_id,
        "critical (P0) issue should dispatch before low (P3) issue; first dispatched: {first_id}"
    );

    daemon.stop_and_kill(&repo);
}

/// An issue in a terminal state (Closed) should never be dispatched,
/// even in automatic mode.
#[test]
fn terminal_issue_is_not_dispatched() {
    let repo = TestRepo::new();

    // Create an issue, then close it.
    let closed_id = repo.create_beads_issue("Already closed");
    repo.update_beads_issue_status(&closed_id, "closed");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Switch to automatic mode.
    let _ = daemon_command(&repo, &["mode", "automatic"]);

    // Wait a few poll cycles — the closed issue should NOT dispatch.
    std::thread::sleep(Duration::from_secs(5));

    let snap = daemon_snapshot(&repo).expect("snapshot");
    let history = agent_run_history(&snap);
    let running = running_agents(&snap);

    // Nothing should have been dispatched.
    assert!(
        history.is_empty(),
        "closed issue should not produce history entries: {history:#?}"
    );
    assert!(
        running.is_empty(),
        "closed issue should not be running: {running:#?}"
    );

    daemon.stop_and_kill(&repo);
}

/// Verify that the daemon does not dispatch more agents than
/// `max_concurrent_agents` allows.
#[test]
fn max_concurrent_agents_is_respected() {
    let repo = TestRepo::new();

    // Use a slow agent so multiple could overlap.
    let slow_script = repo.root().join(".polyphony-fixtures/agent-stall.sh");
    let toml = format!(
        r#"[tracker]
kind = "beads"
active_states = ["Open", "In Progress", "Blocked"]
terminal_states = ["Closed", "Deferred"]

[workspace]
checkout_kind = "directory"
sync_on_reuse = false

[agent]
max_concurrent_agents = 1
max_retry_backoff_ms = 60000

[agents.profiles.test-agent]
kind = "local"
transport = "local_cli"
command = "bash {slow_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 30000
stall_timeout_ms = 30000
"#,
        slow_script = slow_script.display(),
    );
    repo.write_repo_config(Some(&toml));

    // Create two issues.
    repo.create_beads_issue("Concurrent test A");
    repo.create_beads_issue("Concurrent test B");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Switch to automatic mode — both issues are eligible.
    let _ = daemon_command(&repo, &["mode", "automatic"]);

    // Wait for at least one agent to be running.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(15), |s| {
        !running_agents(s).is_empty()
    });
    assert!(
        snap.is_some(),
        "no agent started running\n{}",
        dump_daemon_logs(&repo),
    );

    // Check that at most 1 agent is running at a time.
    let snap = snap.unwrap();
    let running = running_agents(&snap);
    assert!(
        running.len() <= 1,
        "max_concurrent_agents=1 but {} agents running: {running:#?}",
        running.len()
    );

    daemon.stop_and_kill(&repo);
}

/// Verify that an issue already being dispatched (running) is not
/// double-dispatched on subsequent poll cycles.
#[test]
fn running_issue_is_not_double_dispatched() {
    let repo = TestRepo::new();

    // Use the stall agent so the first dispatch stays running.
    let stall_script = repo.root().join(".polyphony-fixtures/agent-stall.sh");
    let toml = format!(
        r#"[tracker]
kind = "beads"
active_states = ["Open", "In Progress", "Blocked"]
terminal_states = ["Closed", "Deferred"]

[workspace]
checkout_kind = "directory"
sync_on_reuse = false

[agent]
max_concurrent_agents = 2
max_retry_backoff_ms = 60000

[agents.profiles.test-agent]
kind = "local"
transport = "local_cli"
command = "bash {stall_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 30000
stall_timeout_ms = 30000
"#,
        stall_script = stall_script.display(),
    );
    repo.write_repo_config(Some(&toml));

    let issue_id = repo.create_beads_issue("No-double-dispatch test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch manually.
    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);

    // Wait for the agent to start running.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(15), |s| {
        !running_agents(s).is_empty()
    });
    assert!(snap.is_some(), "agent never started running");

    // Switch to automatic mode — the orchestrator should NOT dispatch the
    // same issue again.
    let _ = daemon_command(&repo, &["mode", "automatic"]);

    // Wait a few poll cycles.
    std::thread::sleep(Duration::from_secs(3));

    let snap2 = daemon_snapshot(&repo).expect("snapshot");
    let running = running_agents(&snap2);

    // There should be at most 1 running agent for this issue.
    let runs_for_issue: Vec<_> = running
        .iter()
        .filter(|r| r["issue_id"].as_str() == Some(&issue_id))
        .collect();
    assert!(
        runs_for_issue.len() <= 1,
        "issue should not be double-dispatched; running entries: {runs_for_issue:#?}"
    );

    daemon.stop_and_kill(&repo);
}
