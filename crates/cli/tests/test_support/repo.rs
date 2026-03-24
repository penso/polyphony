use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::fixtures::write_agent_fixture_scripts;

/// An isolated temporary git repository with Beads tracker initialized.
///
/// Drops the temp directory on drop. All polyphony and beads commands run
/// against this repo root so tests are fully isolated from developer state.
pub struct TestRepo {
    pub dir: TempDir,
    pub home_dir: TempDir,
}

impl TestRepo {
    /// Create a new temp repo with git, beads, and a minimal polyphony config.
    ///
    /// Panics if any setup step fails — tests should not proceed on broken harness.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create temp repo dir");
        let home_dir = TempDir::new().expect("create temp home dir");
        let root = dir.path();

        // Git init with test identity.
        run_ok(Command::new("git").args(["init"]).current_dir(root));
        run_ok(
            Command::new("git")
                .args(["config", "user.name", "Polyphony Test"])
                .current_dir(root),
        );
        run_ok(
            Command::new("git")
                .args(["config", "user.email", "polyphony-test@example.com"])
                .current_dir(root),
        );

        // Initial commit so branches and worktrees work.
        std::fs::write(root.join(".gitkeep"), "").expect("write .gitkeep");
        run_ok(Command::new("git").args(["add", "."]).current_dir(root));
        run_ok(
            Command::new("git")
                .args(["commit", "-m", "initial commit"])
                .current_dir(root),
        );

        // Initialize beads.
        let prefix = format!("test-{}", std::process::id());
        run_ok(
            Command::new("bd")
                .args(["init", "--quiet", "--prefix", &prefix])
                .current_dir(root),
        );

        // Write agent fixture scripts.
        write_agent_fixture_scripts(root);

        let repo = Self { dir, home_dir };

        // Write default workflow and config.
        repo.write_workflow(None);
        repo.write_repo_config(None);

        repo
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn home(&self) -> &Path {
        self.home_dir.path()
    }

    /// Write WORKFLOW.md with the given YAML front matter config, or a sensible default.
    pub fn write_workflow(&self, custom_yaml: Option<&str>) {
        let yaml = custom_yaml.unwrap_or(
            r#"tracker:
  kind: beads
  active_states: ["Open", "In Progress", "Blocked"]
  terminal_states: ["Closed", "Deferred"]
polling:
  interval_ms: 500
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
  sync_on_reuse: false
agent:
  max_concurrent_agents: 1
  max_turns: 2
  max_retry_backoff_ms: 1000
orchestration:
  mode: advisory
agents:
  default: test-agent
pipeline:
  enabled: false"#,
        );
        let workflow = format!("---\n{yaml}\n---\nTest workflow.\n");
        std::fs::write(self.root().join("WORKFLOW.md"), workflow).expect("write WORKFLOW.md");
    }

    /// Write polyphony.toml with the given TOML content, or a sensible default.
    pub fn write_repo_config(&self, custom_toml: Option<&str>) {
        let agent_script = self.root().join(".polyphony-fixtures/agent-success.sh");
        let default_toml = format!(
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
command = "bash {agent_script}"
interaction_mode = "one_shot"
turn_timeout_ms = 10000
stall_timeout_ms = 5000
completion_sentinel = "POLYPHONY_AGENT_DONE"
"#,
            agent_script = agent_script.display(),
        );
        let toml = custom_toml.unwrap_or(&default_toml);
        std::fs::write(self.root().join("polyphony.toml"), toml).expect("write polyphony.toml");
    }

    /// Create a beads issue and return its full ID.
    pub fn create_beads_issue(&self, title: &str) -> String {
        let output = Command::new("bd")
            .args(["create", "--json", &format!("--title={title}"), "--type=task", "--priority=2"])
            .current_dir(self.root())
            .output()
            .expect("run bd create");
        assert!(
            output.status.success(),
            "bd create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse bd create JSON");
        json["id"].as_str().expect("bd create returned id").to_string()
    }

    /// Update a beads issue status.
    pub fn update_beads_issue_status(&self, id: &str, status: &str) {
        run_ok(
            Command::new("bd")
                .args(["update", id, &format!("--status={status}")])
                .current_dir(self.root()),
        );
    }

    /// Show a beads issue and return the JSON.
    pub fn show_beads_issue(&self, id: &str) -> serde_json::Value {
        let output = Command::new("bd")
            .args(["show", id, "--long", "--json"])
            .current_dir(self.root())
            .output()
            .expect("run bd show");
        assert!(
            output.status.success(),
            "bd show failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("parse bd show JSON")
    }

    /// Return the path to the polyphony binary built by cargo.
    pub fn polyphony_bin() -> PathBuf {
        // cargo test sets this env var to the directory containing test binaries.
        let mut path = PathBuf::from(env!("CARGO_BIN_EXE_polyphony"));
        // If not set, fall back to cargo build path.
        if !path.exists() {
            path = PathBuf::from("target/debug/polyphony");
        }
        path
    }

    /// Environment variables for isolated polyphony execution.
    pub fn env_vars(&self) -> Vec<(&str, String)> {
        vec![
            ("HOME", self.home().display().to_string()),
            ("XDG_CONFIG_HOME", self.home().join(".config").display().to_string()),
        ]
    }
}

fn run_ok(cmd: &mut Command) {
    let output = cmd.output().expect("spawn command");
    assert!(
        output.status.success(),
        "command {:?} failed with {}: {}",
        cmd.get_program(),
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}
