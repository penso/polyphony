use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::repo::TestRepo;

/// A running polyphony daemon process.
pub struct PolyphonyProcess {
    child: Child,
}

impl PolyphonyProcess {
    /// Start a daemon in the foreground (daemon run) and return immediately.
    ///
    /// The daemon process runs as a child that can be stopped later.
    pub fn start_daemon(repo: &TestRepo) -> Self {
        let child = Command::new(TestRepo::polyphony_bin())
            .args([
                "-C",
                &repo.root().display().to_string(),
                "--no-tui",
                "daemon",
                "run",
            ])
            .envs(repo.env_vars().into_iter().map(|(k, v)| (k, v)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn polyphony daemon");

        Self { child }
    }

    /// Wait for the daemon to become ready by polling `daemon snapshot`.
    ///
    /// Uses `daemon snapshot` instead of `daemon status` because snapshot
    /// returns structured JSON we can verify for readiness.
    pub fn wait_ready(repo: &TestRepo, timeout: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if let Some(snap) = daemon_snapshot(repo) {
                if snapshot_is_ready_for_daemon(&snap) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        false
    }

    /// Stop the daemon via the daemon stop command, then kill if needed.
    pub fn stop_and_kill(mut self, repo: &TestRepo) {
        let _ = run_polyphony(repo, &["daemon", "stop"]);
        // Give the daemon a moment to shut down gracefully.
        let start = Instant::now();
        loop {
            if start.elapsed() > Duration::from_secs(3) {
                break;
            }
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Force kill if still running.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Kill the child process directly.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PolyphonyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Check if a daemon snapshot indicates the runtime is ready.
fn snapshot_is_ready_for_daemon(snapshot: &serde_json::Value) -> bool {
    // The daemon status JSON wraps the snapshot under a "snapshot" key.
    let snap = if snapshot.get("snapshot").is_some() {
        &snapshot["snapshot"]
    } else {
        snapshot
    };
    let loading = &snap["loading"];
    snap["cadence"]["last_tracker_poll_at"].as_str().is_some()
        && !loading["fetching_issues"].as_bool().unwrap_or(true)
        && !loading["fetching_budgets"].as_bool().unwrap_or(true)
        && !loading["fetching_models"].as_bool().unwrap_or(true)
        && !loading["reconciling"].as_bool().unwrap_or(true)
}

/// Run a polyphony subcommand synchronously and return the full output.
pub fn run_polyphony(repo: &TestRepo, args: &[&str]) -> Output {
    let dir_str = repo.root().display().to_string();
    let mut full_args: Vec<&str> = vec!["-C", &dir_str];
    full_args.extend_from_slice(args);

    Command::new(TestRepo::polyphony_bin())
        .args(&full_args)
        .envs(repo.env_vars().into_iter().map(|(k, v)| (k, v)))
        .output()
        .expect("run polyphony command")
}

/// Run a polyphony subcommand that returns JSON and parse it.
pub fn run_polyphony_json(repo: &TestRepo, args: &[&str]) -> serde_json::Value {
    let output = run_polyphony(repo, args);
    assert!(
        output.status.success(),
        "polyphony {:?} failed (exit {}): {}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse polyphony {:?} JSON: {e}\nstdout was:\n{stdout}",
            args
        )
    })
}

/// Poll a polyphony data subcommand until a condition is met or timeout.
pub fn wait_for_snapshot<F>(
    repo: &TestRepo,
    data_args: &[&str],
    timeout: Duration,
    condition: F,
) -> Option<serde_json::Value>
where
    F: Fn(&serde_json::Value) -> bool,
{
    let started = Instant::now();
    while started.elapsed() < timeout {
        let output = run_polyphony(repo, data_args);
        if output.status.success() {
            if let Ok(json) =
                serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&output.stdout))
            {
                if condition(&json) {
                    return Some(json);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    None
}

/// Send a daemon control command (refresh, mode, dispatch, etc.).
pub fn daemon_command(repo: &TestRepo, args: &[&str]) -> Output {
    let mut full_args = vec!["daemon"];
    full_args.extend_from_slice(args);
    run_polyphony(repo, &full_args)
}

/// Get the daemon snapshot via `polyphony daemon snapshot`.
pub fn daemon_snapshot(repo: &TestRepo) -> Option<serde_json::Value> {
    let output = run_polyphony(repo, &["daemon", "snapshot"]);
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Poll daemon snapshot until a condition is met.
pub fn wait_for_daemon_snapshot<F>(
    repo: &TestRepo,
    timeout: Duration,
    condition: F,
) -> Option<serde_json::Value>
where
    F: Fn(&serde_json::Value) -> bool,
{
    let started = Instant::now();
    while started.elapsed() < timeout {
        if let Some(snapshot) = daemon_snapshot(repo) {
            if condition(&snapshot) {
                return Some(snapshot);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    None
}

/// Dump daemon logs for debugging. Call on test failure.
pub fn dump_daemon_logs(repo: &TestRepo) -> String {
    let log_dir = repo.root().join(".polyphony/logs");
    if !log_dir.exists() {
        return "no logs directory".to_string();
    }
    let mut logs = String::new();
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                logs.push_str(&format!("--- {} ---\n", entry.path().display()));
                // Only last 100 lines to avoid huge output.
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(100);
                for line in &lines[start..] {
                    logs.push_str(line);
                    logs.push('\n');
                }
            }
        }
    }
    logs
}
