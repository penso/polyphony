use std::path::{Path, PathBuf};

/// Signals used to detect an agent's presence on the system.
#[derive(Debug, Clone)]
pub struct DetectedAgent {
    /// Profile name used in `[agents.profiles.<name>]`.
    pub profile_name: String,
    /// Agent kind (e.g. "claude", "codex", "openai").
    pub kind: String,
    /// Ready-to-append TOML snippet for the user config.
    pub toml_snippet: String,
    /// Human-readable detection signals for diagnostic output.
    pub detection_signals: Vec<String>,
}

/// Abstraction over system environment for testability.
pub trait DetectionEnv {
    fn which_binary(&self, name: &str) -> Option<PathBuf>;
    fn env_var_present(&self, name: &str) -> bool;
    fn dir_exists(&self, path: &Path) -> bool;
    fn home_dir(&self) -> Option<PathBuf>;
}

/// Real system environment.
pub struct SystemEnv;

impl DetectionEnv for SystemEnv {
    fn which_binary(&self, name: &str) -> Option<PathBuf> {
        which_binary(name)
    }

    fn env_var_present(&self, name: &str) -> bool {
        std::env::var_os(name).is_some()
    }

    fn dir_exists(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }
}

/// Look up a binary by name in `$PATH` without executing it.
pub fn which_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(name);
            if full.is_file() {
                Some(full)
            } else {
                None
            }
        })
    })
}

/// Detect available AI coding agents using the real system environment.
pub fn detect_agents() -> Vec<DetectedAgent> {
    detect_agents_with(&SystemEnv)
}

/// Detect available AI coding agents using the provided environment.
pub fn detect_agents_with(env: &dyn DetectionEnv) -> Vec<DetectedAgent> {
    let home = env.home_dir();
    let probes: Vec<fn(&dyn DetectionEnv, Option<&Path>) -> Option<DetectedAgent>> = vec![
        probe_claude,
        probe_codex,
        probe_pi,
        probe_openclaw,
        probe_copilot,
        probe_kimi,
        probe_openai_api,
        probe_openrouter,
    ];
    let mut detected = Vec::new();
    let mut has_codex = false;
    for probe in probes {
        if let Some(agent) = probe(env, home.as_deref()) {
            if agent.profile_name == "codex" {
                has_codex = true;
            }
            detected.push(agent);
        }
    }
    // OpenAI API-only should not appear if Codex was detected (Codex already uses OPENAI_API_KEY).
    if has_codex {
        detected.retain(|a| a.profile_name != "openai");
    }
    detected
}

/// Build the TOML text for all detected agent profiles, ready to insert after `[agents.profiles]`.
pub fn agent_profiles_toml(agents: &[DetectedAgent]) -> String {
    let mut out = String::new();
    for agent in agents {
        out.push('\n');
        out.push_str(&agent.toml_snippet);
        out.push('\n');
    }
    out
}

fn probe_claude(env: &dyn DetectionEnv, home: Option<&Path>) -> Option<DetectedAgent> {
    let binary_path = env.which_binary("claude")?;
    let mut signals = vec![format!("binary: {}", binary_path.display())];
    if env.env_var_present("ANTHROPIC_API_KEY") {
        signals.push("ANTHROPIC_API_KEY set".into());
    }
    if env.env_var_present("CLAUDE_OAUTH_TOKEN") {
        signals.push("CLAUDE_OAUTH_TOKEN set".into());
    }
    if let Some(home) = home
        && env.dir_exists(&home.join(".claude"))
    {
        signals.push("~/.claude/ found".into());
    }
    Some(DetectedAgent {
        profile_name: "claude".into(),
        kind: "claude".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.claude]
kind = "claude"
transport = "local_cli"
command = "claude -p --verbose --dangerously-skip-permissions"
interaction_mode = "interactive"
fetch_models = true
models_command = "claude models --json""#,
        ),
        detection_signals: signals,
    })
}

fn probe_codex(env: &dyn DetectionEnv, home: Option<&Path>) -> Option<DetectedAgent> {
    let binary_path = env.which_binary("codex")?;
    let mut signals = vec![format!("binary: {}", binary_path.display())];
    if env.env_var_present("CODEX_API_KEY") {
        signals.push("CODEX_API_KEY set".into());
    }
    if env.env_var_present("OPENAI_API_KEY") {
        signals.push("OPENAI_API_KEY set".into());
    }
    if let Some(home) = home
        && env.dir_exists(&home.join(".codex"))
    {
        signals.push("~/.codex/ found".into());
    }
    Some(DetectedAgent {
        profile_name: "codex".into(),
        kind: "codex".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.codex]
kind = "codex"
transport = "app_server"
command = "codex --dangerously-bypass-approvals-and-sandbox app-server"
fetch_models = true
models_command = "codex models --json"
approval_policy = "never"
thread_sandbox = "workspace-write"
turn_sandbox_policy = "workspace-write""#,
        ),
        detection_signals: signals,
    })
}

fn probe_pi(env: &dyn DetectionEnv, _home: Option<&Path>) -> Option<DetectedAgent> {
    let binary_path = env.which_binary("pi")?;
    let signals = vec![format!("binary: {}", binary_path.display())];
    Some(DetectedAgent {
        profile_name: "pi".into(),
        kind: "pi".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.pi]
kind = "pi"
transport = "rpc"
command = "pi""#,
        ),
        detection_signals: signals,
    })
}

fn probe_openclaw(env: &dyn DetectionEnv, home: Option<&Path>) -> Option<DetectedAgent> {
    let binary_path = env.which_binary("openclaw")?;
    let mut signals = vec![format!("binary: {}", binary_path.display())];
    if let Some(home) = home
        && env.dir_exists(&home.join(".openclaw"))
    {
        signals.push("~/.openclaw/ found".into());
    }
    Some(DetectedAgent {
        profile_name: "openclaw".into(),
        kind: "openclaw".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.openclaw]
kind = "openclaw"
transport = "local_cli"
command = "openclaw""#,
        ),
        detection_signals: signals,
    })
}

fn probe_copilot(env: &dyn DetectionEnv, _home: Option<&Path>) -> Option<DetectedAgent> {
    let binary_path = env.which_binary("copilot")?;
    let mut signals = vec![format!("binary: {}", binary_path.display())];
    if env.env_var_present("GITHUB_TOKEN") {
        signals.push("GITHUB_TOKEN set".into());
    }
    Some(DetectedAgent {
        profile_name: "copilot".into(),
        kind: "github-copilot".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.copilot]
kind = "github-copilot"
transport = "local_cli"
command = "copilot"
fetch_models = false"#,
        ),
        detection_signals: signals,
    })
}

fn probe_kimi(env: &dyn DetectionEnv, home: Option<&Path>) -> Option<DetectedAgent> {
    let binary_path = env.which_binary("kimi")?;
    let mut signals = vec![format!("binary: {}", binary_path.display())];
    if env.env_var_present("KIMI_API_KEY") {
        signals.push("KIMI_API_KEY set".into());
    }
    if env.env_var_present("MOONSHOT_API_KEY") {
        signals.push("MOONSHOT_API_KEY set".into());
    }
    if let Some(home) = home
        && env.dir_exists(&home.join(".kimi"))
    {
        signals.push("~/.kimi/ found".into());
    }
    Some(DetectedAgent {
        profile_name: "kimi".into(),
        kind: "kimi".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.kimi]
kind = "kimi"
transport = "openai_chat"
fetch_models = true"#,
        ),
        detection_signals: signals,
    })
}

/// OpenAI API-only (no binary required). Excluded when Codex is detected.
fn probe_openai_api(env: &dyn DetectionEnv, _home: Option<&Path>) -> Option<DetectedAgent> {
    if !env.env_var_present("OPENAI_API_KEY") {
        return None;
    }
    Some(DetectedAgent {
        profile_name: "openai".into(),
        kind: "openai".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.openai]
kind = "openai"
transport = "openai_chat"
model = "gpt-5.1"
api_key = "$OPENAI_API_KEY"
fetch_models = true"#,
        ),
        detection_signals: vec!["OPENAI_API_KEY set".into()],
    })
}

/// OpenRouter API-only (no binary required).
fn probe_openrouter(env: &dyn DetectionEnv, _home: Option<&Path>) -> Option<DetectedAgent> {
    if !env.env_var_present("OPENROUTER_API_KEY") {
        return None;
    }
    Some(DetectedAgent {
        profile_name: "openrouter".into(),
        kind: "openrouter".into(),
        toml_snippet: indoc(
            r#"[agents.profiles.openrouter]
kind = "openrouter"
transport = "openai_chat"
base_url = "https://openrouter.ai/api/v1"
api_key = "$OPENROUTER_API_KEY"
fetch_models = true"#,
        ),
        detection_signals: vec!["OPENROUTER_API_KEY set".into()],
    })
}

fn indoc(s: &str) -> String {
    s.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    struct MockEnv {
        binaries: HashSet<String>,
        env_vars: HashSet<String>,
        dirs: HashSet<PathBuf>,
        home: Option<PathBuf>,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                binaries: HashSet::new(),
                env_vars: HashSet::new(),
                dirs: HashSet::new(),
                home: Some(PathBuf::from("/home/testuser")),
            }
        }

        fn with_binary(mut self, name: &str) -> Self {
            self.binaries.insert(name.into());
            self
        }

        fn with_env(mut self, name: &str) -> Self {
            self.env_vars.insert(name.into());
            self
        }

        fn with_dir(mut self, path: impl Into<PathBuf>) -> Self {
            self.dirs.insert(path.into());
            self
        }
    }

    impl DetectionEnv for MockEnv {
        fn which_binary(&self, name: &str) -> Option<PathBuf> {
            if self.binaries.contains(name) {
                Some(PathBuf::from(format!("/usr/bin/{name}")))
            } else {
                None
            }
        }

        fn env_var_present(&self, name: &str) -> bool {
            self.env_vars.contains(name)
        }

        fn dir_exists(&self, path: &Path) -> bool {
            self.dirs.contains(path)
        }

        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }
    }

    #[test]
    fn detects_claude_binary() {
        let env = MockEnv::new()
            .with_binary("claude")
            .with_env("ANTHROPIC_API_KEY")
            .with_dir("/home/testuser/.claude");
        let agents = detect_agents_with(&env);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].profile_name, "claude");
        assert_eq!(agents[0].kind, "claude");
        assert!(agents[0].detection_signals.len() >= 2);
    }

    #[test]
    fn detects_codex_binary() {
        let env = MockEnv::new()
            .with_binary("codex")
            .with_env("OPENAI_API_KEY");
        let agents = detect_agents_with(&env);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].profile_name, "codex");
    }

    #[test]
    fn openai_excluded_when_codex_present() {
        let env = MockEnv::new()
            .with_binary("codex")
            .with_env("OPENAI_API_KEY");
        let agents = detect_agents_with(&env);
        assert!(agents.iter().all(|a| a.profile_name != "openai"));
    }

    #[test]
    fn openai_included_when_codex_absent() {
        let env = MockEnv::new().with_env("OPENAI_API_KEY");
        let agents = detect_agents_with(&env);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].profile_name, "openai");
    }

    #[test]
    fn openrouter_detected_via_env_var() {
        let env = MockEnv::new().with_env("OPENROUTER_API_KEY");
        let agents = detect_agents_with(&env);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].profile_name, "openrouter");
    }

    #[test]
    fn no_agents_on_empty_env() {
        let env = MockEnv::new();
        let agents = detect_agents_with(&env);
        assert!(agents.is_empty());
    }

    #[test]
    fn priority_order_preserved() {
        let env = MockEnv::new()
            .with_binary("claude")
            .with_binary("codex")
            .with_binary("pi")
            .with_binary("copilot")
            .with_env("OPENROUTER_API_KEY");
        let agents = detect_agents_with(&env);
        let names: Vec<&str> = agents.iter().map(|a| a.profile_name.as_str()).collect();
        assert_eq!(names, vec![
            "claude",
            "codex",
            "pi",
            "copilot",
            "openrouter"
        ]);
    }

    #[test]
    fn multiple_agents_detected() {
        let env = MockEnv::new()
            .with_binary("claude")
            .with_binary("codex")
            .with_env("OPENAI_API_KEY");
        let agents = detect_agents_with(&env);
        let names: Vec<&str> = agents.iter().map(|a| a.profile_name.as_str()).collect();
        // Codex present → openai excluded
        assert_eq!(names, vec!["claude", "codex"]);
    }

    #[test]
    fn toml_snippets_are_valid_toml() {
        let env = MockEnv::new()
            .with_binary("claude")
            .with_binary("codex")
            .with_binary("pi")
            .with_binary("copilot")
            .with_binary("kimi")
            .with_env("OPENROUTER_API_KEY")
            .with_env("OPENAI_API_KEY");
        let agents = detect_agents_with(&env);
        for agent in &agents {
            let wrapped = format!("[agents]\n[agents.profiles]\n{}", agent.toml_snippet);
            let parsed: Result<toml::Value, _> = toml::from_str(&wrapped);
            assert!(
                parsed.is_ok(),
                "Invalid TOML for {}: {:?}",
                agent.profile_name,
                parsed.err()
            );
        }
    }

    #[test]
    fn agent_profiles_toml_concatenates() {
        let agents = vec![
            DetectedAgent {
                profile_name: "a".into(),
                kind: "a".into(),
                toml_snippet: "[agents.profiles.a]\nkind = \"a\"".into(),
                detection_signals: vec![],
            },
            DetectedAgent {
                profile_name: "b".into(),
                kind: "b".into(),
                toml_snippet: "[agents.profiles.b]\nkind = \"b\"".into(),
                detection_signals: vec![],
            },
        ];
        let toml = agent_profiles_toml(&agents);
        assert!(toml.contains("[agents.profiles.a]"));
        assert!(toml.contains("[agents.profiles.b]"));
    }

    #[test]
    fn pi_detected_with_binary_only() {
        let env = MockEnv::new().with_binary("pi");
        let agents = detect_agents_with(&env);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].profile_name, "pi");
        assert_eq!(agents[0].kind, "pi");
    }

    #[test]
    fn kimi_requires_binary() {
        let env = MockEnv::new().with_env("KIMI_API_KEY");
        let agents = detect_agents_with(&env);
        assert!(agents.iter().all(|a| a.profile_name != "kimi"));
    }

    #[test]
    fn copilot_requires_binary() {
        let env = MockEnv::new().with_env("GITHUB_TOKEN");
        let agents = detect_agents_with(&env);
        assert!(agents.iter().all(|a| a.profile_name != "copilot"));
    }
}
