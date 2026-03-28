//! Retry behavior and granular data-command integration tests.

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod test_support;

use std::time::Duration;

use test_support::*;

/// After a failed agent run the issue should appear in the retrying queue
/// with a future due_at and an error message.
#[test]
fn failed_run_schedules_retry() {
    let repo = TestRepo::new();

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

    let issue_id = repo.create_beads_issue("Retry test issue");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Switch to manual mode so retries are scheduled (stop mode skips retries).
    let _ = daemon_command(&repo, &["mode", "manual"]);

    // Dispatch to the fail agent.
    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "fail-agent"]);

    // Wait for the retry entry to appear.
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        let retrying = s["retrying"].as_array().map_or(0, |a| a.len());
        retrying > 0
    });
    assert!(
        snap.is_some(),
        "failed run did not produce a retry entry\n{}",
        dump_daemon_logs(&repo),
    );

    let snap = snap.unwrap();
    let retrying = snap["retrying"].as_array().expect("retrying is array");
    assert!(!retrying.is_empty(), "retrying should have an entry");

    let entry = &retrying[0];
    assert_eq!(
        entry["issue_id"].as_str(),
        Some(issue_id.as_str()),
        "retry entry should reference the failed issue"
    );
    assert!(
        entry["due_at"].as_str().is_some(),
        "retry entry should have a due_at timestamp"
    );
    // Attempt should be >= 1 (first retry).
    let attempt = entry["attempt"].as_u64().unwrap_or(0);
    assert!(attempt >= 1, "retry attempt should be >= 1, got {attempt}");

    daemon.stop_and_kill(&repo);
}

/// `polyphony data running` should return the running agents array.
#[test]
fn data_running_returns_running_agents() {
    let repo = TestRepo::new();

    // Use a stalling agent so it stays running while we query.
    let stall_script = repo.root().join(".polyphony-fixtures/agent-stall.sh");
    let toml = format!(
        r#"[tracker]
kind = "beads"
active_states = ["Open", "In Progress", "Blocked"]
terminal_states = ["Closed", "Deferred"]

[workspace]
checkout_kind = "directory"
sync_on_reuse = false

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

    let issue_id = repo.create_beads_issue("Running query test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch the issue.
    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);

    // Wait for the agent to start running.
    let _ = wait_for_daemon_snapshot(&repo, Duration::from_secs(15), |s| {
        !running_agents(s).is_empty()
    });

    // Now query via `data running` against the daemon snapshot.
    let snap = daemon_snapshot(&repo).expect("daemon snapshot");
    let running = running_agents(&snap);
    assert!(
        !running.is_empty(),
        "data running should show the active agent"
    );

    let agent = &running[0];
    assert_eq!(agent["issue_id"].as_str(), Some(issue_id.as_str()));
    assert!(agent["agent_name"].as_str().is_some());
    assert!(agent["started_at"].as_str().is_some());
    assert!(agent["workspace_path"].as_str().is_some());

    daemon.stop_and_kill(&repo);
}

/// `polyphony data history` should return completed runs.
#[test]
fn data_history_returns_completed_runs() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("History query test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch and wait for completion.
    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);
    let snap = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        !agent_run_history(s).is_empty()
    });
    assert!(
        snap.is_some(),
        "run never completed\n{}",
        dump_daemon_logs(&repo)
    );

    let snap = snap.unwrap();
    let history = agent_run_history(&snap);
    assert!(!history.is_empty(), "history should have entries");

    let entry = &history[0];
    assert!(entry["started_at"].as_str().is_some());
    assert!(entry["finished_at"].as_str().is_some());
    assert!(entry["status"].as_str().is_some());
    assert!(entry["agent_name"].as_str().is_some());
    assert!(entry["issue_id"].as_str().is_some());

    daemon.stop_and_kill(&repo);
}

/// The daemon snapshot should contain runtime events after a dispatch.
#[test]
fn daemon_snapshot_contains_runtime_events() {
    let repo = TestRepo::new();
    let issue_id = repo.create_beads_issue("Events data test");

    let daemon = PolyphonyProcess::start_daemon(&repo);
    assert!(
        PolyphonyProcess::wait_ready(&repo, Duration::from_secs(15)),
        "daemon did not become ready\n{}",
        dump_daemon_logs(&repo),
    );

    // Dispatch to generate events.
    let _ = daemon_command(&repo, &["dispatch", &issue_id, "--agent", "test-agent"]);
    let _ = wait_for_daemon_snapshot(&repo, Duration::from_secs(30), |s| {
        !agent_run_history(s).is_empty()
    });

    let snap = daemon_snapshot(&repo).expect("daemon snapshot");
    let events = recent_events(&snap);

    assert!(
        !events.is_empty(),
        "daemon should have runtime events after dispatch"
    );

    let event = &events[0];
    assert!(
        event["at"].as_str().is_some(),
        "event should have timestamp"
    );
    assert!(event["scope"].as_str().is_some(), "event should have scope");
    assert!(
        event["message"].as_str().is_some(),
        "event should have message"
    );

    daemon.stop_and_kill(&repo);
}

/// `polyphony config --json` should return the merged config as valid JSON.
#[test]
fn config_json_returns_valid_config() {
    let repo = TestRepo::new();

    let config = run_polyphony_json(&repo, &["config", "--json"]);
    // The merged config should contain tracker and agents sections.
    assert!(
        config["tracker"].is_object(),
        "config should have tracker section"
    );
    assert_eq!(
        config["tracker"]["kind"].as_str(),
        Some("beads"),
        "tracker kind should be beads"
    );
    assert!(
        config["agents"].is_object(),
        "config should have agents section"
    );
}

/// `polyphony doctor` should pass with a valid configuration.
#[test]
fn doctor_passes_with_valid_config() {
    let repo = TestRepo::new();

    let output = run_polyphony(&repo, &["doctor"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "doctor should pass with valid config: {stdout}"
    );
    assert!(
        stdout.contains("passed") || stdout.contains("ok"),
        "doctor output should indicate success: {stdout}"
    );
}
