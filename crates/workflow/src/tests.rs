use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use polyphony_core::{
    AgentInteractionMode, AgentPromptMode, AgentTransport, CheckoutKind, DispatchMode, Issue,
    TrackerKind,
};
use serde_yaml::Value as YamlValue;

use crate::{
    AgentProfileOverride, AgentPromptConfig, LoadedWorkflow, ServiceConfig, WorkflowDefinition,
    files::*, load_workflow_with_user_config, parse_workflow, render::*, render_issue_template,
    render_turn_template, repo_config_path,
};

fn sample_issue() -> Issue {
    Issue {
        id: "1".into(),
        identifier: "ISSUE-1".into(),
        title: "Title".into(),
        state: "Todo".into(),
        ..Issue::default()
    }
}

fn unique_temp_path(name: &str, extension: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "polyphony-workflow-{name}-{}.{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        extension
    ))
}

#[test]
fn workspace_defaults_include_reuse_and_transient_paths() {
    let workflow = WorkflowDefinition {
        config: YamlValue::Mapping(Default::default()),
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert!(config.workspace.sync_on_reuse);
    assert_eq!(config.workspace.transient_paths, vec!["tmp"]);
    assert_eq!(config.tracker.kind, TrackerKind::None);
    assert!(config.agents.default.is_none());
    assert!(config.agents.profiles.is_empty());
}

#[test]
fn workspace_config_parses_reuse_and_transient_paths() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
workspace:
  sync_on_reuse: false
  transient_paths:
    - target
    - .cache
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert!(!config.workspace.sync_on_reuse);
    assert_eq!(config.workspace.transient_paths, vec!["target", ".cache"]);
}

#[test]
fn render_template_treats_first_attempt_as_nil() {
    let rendered = render_issue_template(
        "{{ attempt | default: \"first\" }}",
        &sample_issue(),
        None,
        Default::default(),
    )
    .unwrap();

    assert_eq!(rendered, "first");
}

#[test]
fn render_template_passes_retry_attempt_number() {
    let rendered = render_issue_template(
        "{{ attempt | default: \"first\" }}",
        &sample_issue(),
        Some(2),
        Default::default(),
    )
    .unwrap();

    assert_eq!(rendered, "2");
}

#[test]
fn render_template_exposes_turn_context() {
    let rendered = render_turn_template(
        "{{ turn_number }}/{{ max_turns }}/{{ is_continuation }}",
        &sample_issue(),
        None,
        2,
        5,
    )
    .unwrap();

    assert_eq!(rendered, "2/5/true");
}

#[test]
fn render_template_rejects_unknown_variables() {
    let error = render_issue_template(
        "{{ missing_value }}",
        &sample_issue(),
        None,
        Default::default(),
    )
    .unwrap_err();

    assert!(matches!(error, crate::Error::TemplateRender(_)));
    assert!(error.to_string().contains("Unknown variable"));
}

#[test]
fn render_template_rejects_unknown_filters() {
    let error = render_issue_template(
        "{{ issue.title | missing_filter }}",
        &sample_issue(),
        None,
        Default::default(),
    )
    .unwrap_err();

    assert!(matches!(error, crate::Error::TemplateParse(_)));
    assert!(error.to_string().contains("Unknown filter"));
}

#[test]
fn missing_stall_timeout_defaults_to_five_minutes() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    let selected = config.select_agent_for_issue(&sample_issue()).unwrap();
    assert_eq!(selected.stall_timeout_ms, 300_000);
}

#[test]
fn zero_stall_timeout_disables_detection() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      stall_timeout_ms: 0
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    let selected = config.select_agent_for_issue(&sample_issue()).unwrap();
    assert_eq!(selected.stall_timeout_ms, 0);
}

#[test]
fn continuation_prompt_is_loaded_from_agent_config() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agent:
  continuation_prompt: |
    Continue {{ issue.identifier }} turn {{ turn_number }} of {{ max_turns }}
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert_eq!(
        config.agent.continuation_prompt.as_deref(),
        Some("Continue {{ issue.identifier }} turn {{ turn_number }} of {{ max_turns }}\n")
    );
}

#[test]
fn codex_shorthand_is_hydrated_into_agents() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
codex:
  approval_policy: auto
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let agent = config
        .agents
        .profiles
        .get("codex")
        .expect("codex shorthand should be migrated");

    assert_eq!(config.agents.default.as_deref(), Some("codex"));
    assert_eq!(agent.command.as_deref(), Some("codex app-server"));
    assert_eq!(agent.approval_policy.as_deref(), Some("auto"));
    assert!(agent.fetch_models);
}

#[test]
fn legacy_provider_is_hydrated_into_agents() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
provider:
  kind: codex
  command: codex app-server
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let agent = config
        .agents
        .profiles
        .get("codex")
        .expect("legacy provider should be migrated");

    assert_eq!(config.agents.default.as_deref(), Some("codex"));
    assert_eq!(agent.command.as_deref(), Some("codex app-server"));
    assert!(agent.fetch_models);
}

#[test]
fn codex_shorthand_cannot_be_combined_with_agents() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
codex:
  command: codex app-server
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      command: codex app-server
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let error = ServiceConfig::from_workflow(&workflow).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot be combined with `agents`")
    );
}

#[test]
fn codex_and_provider_cannot_both_be_configured() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
codex:
  command: codex app-server
provider:
  command: codex app-server
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let error = ServiceConfig::from_workflow(&workflow).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("`codex` and legacy `provider` cannot both be configured")
    );
}

#[test]
fn select_agent_prefers_label_then_default() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: codex
  by_label:
    risky: claude
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
    claude:
      kind: claude
      transport: local_cli
      command: claude --print "$POLYPHONY_PROMPT"
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let mut issue = Issue {
        id: "1".into(),
        identifier: "ISSUE-1".into(),
        title: "Title".into(),
        state: "Todo".into(),
        labels: vec!["risky".into()],
        ..Issue::default()
    };

    let selected = config.select_agent_for_issue(&issue).unwrap();
    assert_eq!(selected.name, "claude");
    assert!(matches!(selected.transport, AgentTransport::LocalCli));

    issue.labels.clear();
    let fallback = config.select_agent_for_issue(&issue).unwrap();
    assert_eq!(fallback.name, "codex");
    assert!(matches!(fallback.transport, AgentTransport::AppServer));
}

#[test]
fn candidate_agents_include_configured_fallback_chain() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      fallbacks:
        - kimi
        - claude
    kimi:
      kind: kimi
      api_key: test-kimi
      model: kimi-2.5
    claude:
      kind: claude
      transport: local_cli
      command: claude
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let issue = Issue {
        id: "1".into(),
        identifier: "ISSUE-1".into(),
        title: "Title".into(),
        state: "Todo".into(),
        ..Issue::default()
    };

    let candidates = config.candidate_agents_for_issue(&issue).unwrap();
    let names = candidates
        .iter()
        .map(|agent| agent.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["codex", "kimi", "claude"]);
    assert_eq!(candidates[0].fallback_agents, vec!["kimi", "claude"]);
}

#[test]
fn invalid_fallback_reference_is_rejected() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      fallbacks:
        - missing
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let error = ServiceConfig::from_workflow(&workflow).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fallbacks references unknown agent `missing`")
    );
}

#[test]
fn interactive_local_agents_default_prompt_mode_by_transport() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: claude
  profiles:
    claude:
      kind: claude
      transport: local_cli
      command: claude
      interaction_mode: interactive
    claude_tmux:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let claude = config.agents.profiles.get("claude").unwrap();
    let claude_tmux = config.agents.profiles.get("claude_tmux").unwrap();

    let claude_agent = agent_definition("claude", claude);
    let claude_tmux_agent = agent_definition("claude_tmux", claude_tmux);

    assert!(matches!(
        claude_agent.interaction_mode,
        AgentInteractionMode::Interactive
    ));
    assert!(matches!(claude_agent.prompt_mode, AgentPromptMode::Stdin));
    assert!(matches!(
        claude_tmux_agent.interaction_mode,
        AgentInteractionMode::Interactive
    ));
    assert!(matches!(
        claude_tmux_agent.prompt_mode,
        AgentPromptMode::TmuxPaste
    ));
}

#[test]
fn kimi_profiles_infer_openai_transport_and_base_url() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: kimi
  profiles:
    kimi:
      kind: kimi
      api_key: test-kimi
      fetch_models: true
      model: kimi-2.5
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let kimi = config
        .select_agent_for_issue(&Issue {
            id: "1".into(),
            identifier: "ISSUE-1".into(),
            title: "Title".into(),
            state: "Todo".into(),
            ..Issue::default()
        })
        .unwrap();

    assert!(matches!(kimi.transport, AgentTransport::OpenAiChat));
    assert_eq!(kimi.base_url.as_deref(), Some("https://api.moonshot.ai/v1"));
    assert_eq!(kimi.model.as_deref(), Some("kimi-2.5"));
}

#[test]
fn acp_profiles_infer_acp_transport() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: claude_acp
  profiles:
    claude_acp:
      kind: claude
      transport: acp
      command: openclaw acp
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let selected = config
        .select_agent_for_issue(&Issue {
            id: "1".into(),
            identifier: "ISSUE-1".into(),
            title: "Title".into(),
            state: "Todo".into(),
            ..Issue::default()
        })
        .unwrap();

    assert!(matches!(selected.transport, AgentTransport::Acp));
    assert_eq!(selected.command.as_deref(), Some("openclaw acp"));
}

#[test]
fn acpx_profiles_infer_acpx_transport_and_default_command() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: claude_acpx
  profiles:
    claude_acpx:
      kind: claude
      transport: acpx
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let selected = config
        .select_agent_for_issue(&Issue {
            id: "1".into(),
            identifier: "ISSUE-1".into(),
            title: "Title".into(),
            state: "Todo".into(),
            ..Issue::default()
        })
        .unwrap();

    assert!(matches!(selected.transport, AgentTransport::Acpx));
    assert_eq!(selected.command.as_deref(), Some("acpx"));
}

#[test]
fn pi_profiles_infer_rpc_transport_and_default_command() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: pi
  profiles:
    pi:
      kind: pi
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let selected = config
        .select_agent_for_issue(&Issue {
            id: "1".into(),
            identifier: "ISSUE-1".into(),
            title: "Title".into(),
            state: "Todo".into(),
            ..Issue::default()
        })
        .unwrap();

    assert!(matches!(selected.transport, AgentTransport::Rpc));
    assert_eq!(selected.command.as_deref(), Some("pi"));
}

#[test]
fn review_triggers_parse_and_reuse_review_agent_defaults() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: penso/polyphony
  api_key: token
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex --dangerously-bypass-approvals-and-sandbox app-server
automation:
  review_agent: codex
review_triggers:
  pr_reviews:
    enabled: true
    debounce_seconds: 45
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert!(config.review_triggers.pr_reviews.enabled);
    assert_eq!(config.review_triggers.pr_reviews.provider, "github");
    assert_eq!(config.review_triggers.pr_reviews.comment_mode, "summary");
    assert_eq!(config.review_triggers.pr_reviews.debounce_seconds, 45);
    assert!(config.review_triggers.pr_reviews.only_labels.is_empty());
    assert!(config.review_triggers.pr_reviews.ignore_labels.is_empty());
    assert!(config.review_triggers.pr_reviews.ignore_authors.is_empty());
    assert!(!config.review_triggers.pr_reviews.ignore_bot_authors);
    assert_eq!(config.pr_review_agent().unwrap().unwrap().name, "codex");
}

#[test]
fn pr_review_agent_prefers_agents_reviewer_role() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: penso/polyphony
  api_key: token
agents:
  default: codex
  reviewer: reviewer
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex --dangerously-bypass-approvals-and-sandbox app-server
    reviewer:
      kind: claude
      transport: local_cli
      command: claude -p --verbose --dangerously-skip-permissions
automation:
  review_agent: codex
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert_eq!(config.pr_review_agent().unwrap().unwrap().name, "reviewer");
}

#[test]
fn review_triggers_allow_inline_comment_mode() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: penso/polyphony
  api_key: token
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex --dangerously-bypass-approvals-and-sandbox app-server
review_triggers:
  pr_reviews:
    enabled: true
    comment_mode: inline
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert_eq!(config.review_triggers.pr_reviews.comment_mode, "inline");
}

#[test]
fn review_triggers_normalize_suppression_rules() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: penso/polyphony
  api_key: token
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex --dangerously-bypass-approvals-and-sandbox app-server
review_triggers:
  pr_reviews:
    enabled: true
    only_labels: [" Ready ", "Needs-Review"]
    ignore_labels: [" WIP "]
    ignore_authors: [" Dependabot[bot] ", " renovate-bot "]
    ignore_bot_authors: true
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert_eq!(config.review_triggers.pr_reviews.only_labels, vec![
        "ready",
        "needs-review"
    ]);
    assert_eq!(config.review_triggers.pr_reviews.ignore_labels, vec!["wip"]);
    assert_eq!(config.review_triggers.pr_reviews.ignore_authors, vec![
        "dependabot[bot]",
        "renovate-bot"
    ]);
    assert!(config.review_triggers.pr_reviews.ignore_bot_authors);
}

#[test]
fn review_triggers_reject_overlapping_only_and_ignore_labels() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: penso/polyphony
  api_key: token
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex --dangerously-bypass-approvals-and-sandbox app-server
review_triggers:
  pr_reviews:
    enabled: true
    only_labels: [ready]
    ignore_labels: [ready]
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let error = ServiceConfig::from_workflow(&workflow).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("only_labels and ignore_labels must not overlap")
    );
}

#[test]
fn openai_chat_fallbacks_without_api_keys_do_not_block_workflow_load() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      fallbacks:
        - kimi_fast
        - openai
    kimi_fast:
      kind: kimi
      model: kimi-2.5
      fetch_models: true
    openai:
      kind: openai
      transport: openai_chat
      model: gpt-4.1
      fetch_models: true
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    let candidates = config.candidate_agents_for_issue(&sample_issue()).unwrap();
    let names = candidates
        .iter()
        .map(|agent| agent.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["codex", "kimi_fast", "openai"]);
    assert_eq!(candidates[1].api_key, None);
    assert_eq!(candidates[2].api_key, None);
}

#[test]
fn tracker_only_workflow_without_agents_is_valid() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: owner/repo
  api_key: test-token
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert!(!config.has_dispatch_agents());
    assert!(
        config
            .candidate_agents_for_issue(&sample_issue())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn github_tracker_without_api_key_is_valid() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: owner/repo
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert_eq!(config.tracker.kind, TrackerKind::Github);
    assert_eq!(config.tracker.repository.as_deref(), Some("owner/repo"));
    assert_eq!(config.tracker.api_key, None);
}

#[test]
fn tracker_profile_can_supply_global_tracker_credentials() {
    let user_config_path = unique_temp_path("tracker-profile", "toml");
    fs::write(
        &user_config_path,
        r#"
[trackers.profiles.github_personal]
kind = "github"
api_key = "test-token"
"#,
    )
    .unwrap();
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  profile: github_personal
  repository: owner/repo
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };
    let config =
        ServiceConfig::from_workflow_with_configs(&workflow, Some(&user_config_path), None)
            .unwrap();
    let _ = fs::remove_file(&user_config_path);

    assert_eq!(config.tracker.profile.as_deref(), Some("github_personal"));
    assert_eq!(config.tracker.kind, TrackerKind::Github);
    assert_eq!(config.tracker.repository.as_deref(), Some("owner/repo"));
    assert_eq!(config.tracker.api_key.as_deref(), Some("test-token"));
}

#[test]
fn unknown_tracker_profile_is_rejected() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  profile: missing
  repository: owner/repo
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let error = ServiceConfig::from_workflow(&workflow).unwrap_err();

    assert!(matches!(error, crate::Error::InvalidConfig(_)));
    assert!(
        error
            .to_string()
            .contains("tracker.profile `missing` is not defined")
    );
}

#[test]
fn user_config_can_supply_tracker_and_agents() {
    let user_config_path = unique_temp_path("user-config", "toml");
    fs::write(
        &user_config_path,
        r#"
[tracker]
kind = "github"
repository = "owner/repo"
api_key = "test-token"

[agents]
default = "codex"

[agents.profiles.codex]
kind = "codex"
transport = "app_server"
command = "codex app-server"
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config: YamlValue::Mapping(Default::default()),
        prompt_template: String::new(),
    };

    let config =
        ServiceConfig::from_workflow_with_configs(&workflow, Some(&user_config_path), None)
            .unwrap();
    let _ = fs::remove_file(&user_config_path);

    assert_eq!(config.tracker.kind, TrackerKind::Github);
    assert_eq!(config.tracker.repository.as_deref(), Some("owner/repo"));
    assert_eq!(config.agents.default.as_deref(), Some("codex"));
}

#[test]
fn workflow_front_matter_overrides_user_config() {
    let user_config_path = unique_temp_path("user-config-override", "toml");
    fs::write(
        &user_config_path,
        r#"
[polling]
interval_ms = 30000
"#,
    )
    .unwrap();
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
polling:
  interval_ms: 2000
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config =
        ServiceConfig::from_workflow_with_configs(&workflow, Some(&user_config_path), None)
            .unwrap();
    let _ = fs::remove_file(&user_config_path);

    assert_eq!(config.polling.interval_ms, 2000);
}

#[test]
fn ensure_user_config_file_writes_template_once() {
    let user_config_path = unique_temp_path("bootstrap-config", "toml");

    let created = ensure_user_config_file(&user_config_path).unwrap();
    let contents = fs::read_to_string(&user_config_path).unwrap();
    let created_again = ensure_user_config_file(&user_config_path).unwrap();
    let _ = fs::remove_file(&user_config_path);

    assert!(created);
    assert!(!created_again);
    assert!(contents.contains("[tracker]"));
    assert!(contents.contains("[trackers.profiles]"));
    assert!(contents.contains("[agents.profiles]"));
    assert!(contents.contains("Polyphony user config."));
}

#[test]
fn ensure_workflow_file_writes_template_once() {
    let workflow_path = unique_temp_path("bootstrap-workflow", "md");

    let created = ensure_workflow_file(&workflow_path).unwrap();
    let contents = fs::read_to_string(&workflow_path).unwrap();
    let created_again = ensure_workflow_file(&workflow_path).unwrap();
    let _ = fs::remove_file(&workflow_path);

    assert!(created);
    assert!(!created_again);
    assert!(contents.contains("# Polyphony Workflow"));
    assert!(contents.contains("tracker:"));
    assert!(contents.contains("Shared credentials and reusable agent profiles"));
}

#[test]
fn ensure_workflow_file_supports_repo_root_relative_path() {
    let workflow_name = format!(
        "polyphony-workflow-relative-{}.md",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let workflow_path = std::path::PathBuf::from(&workflow_name);

    let created = ensure_workflow_file(&workflow_path).unwrap();
    let contents = fs::read_to_string(&workflow_path).unwrap();
    let created_again = ensure_workflow_file(&workflow_path).unwrap();
    let _ = fs::remove_file(&workflow_path);

    assert!(created);
    assert!(!created_again);
    assert!(contents.contains("# Polyphony Workflow"));
    assert!(
        contents.contains("Local tracker identity, router selection, and repo wiring can live")
    );
}

#[test]
fn ensure_repo_config_file_writes_template_once() {
    let root = unique_temp_path("repo-config-root", "d");
    fs::create_dir_all(&root).unwrap();
    let repo_config_path = root.join("polyphony.toml");

    let created = ensure_repo_config_file(&repo_config_path, &root).unwrap();
    let contents = fs::read_to_string(&repo_config_path).unwrap();
    let created_again = ensure_repo_config_file(&repo_config_path, &root).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(created);
    assert!(!created_again);
    assert!(contents.contains("Polyphony repo-local config."));
    assert!(contents.contains("checkout_kind = \"linked_worktree\""));
    assert!(contents.contains(&format!("source_repo_path = \"{}\"", root.display())));
}

#[test]
fn ensure_repo_agent_prompt_files_writes_defaults_once() {
    let root = unique_temp_path("repo-agent-prompts-root", "d");
    fs::create_dir_all(&root).unwrap();
    let workflow_path = root.join("WORKFLOW.md");
    fs::write(&workflow_path, default_workflow_md()).unwrap();

    let created = ensure_repo_agent_prompt_files(&workflow_path).unwrap();
    let created_again = ensure_repo_agent_prompt_files(&workflow_path).unwrap();
    let router = root.join(".polyphony").join("agents").join("router.md");
    let reviewer = root.join(".polyphony").join("agents").join("reviewer.md");
    let router_contents = fs::read_to_string(&router).unwrap();
    let reviewer_contents = fs::read_to_string(&reviewer).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(created.len(), default_repo_agent_prompt_templates().len());
    assert!(created_again.is_empty());
    assert!(router_contents.contains("You are the routing agent"));
    assert!(reviewer_contents.contains("You are the review specialist"));
}

#[test]
fn all_agent_prompt_templates_parse_successfully() {
    for (name, contents) in default_repo_agent_prompt_templates() {
        let definition = parse_workflow(contents)
            .unwrap_or_else(|error| panic!("template {name}.md failed to parse workflow: {error}"));
        let profile = serde_yaml::from_value::<AgentProfileOverride>(definition.config)
            .unwrap_or_else(|error| {
                panic!("template {name}.md front matter failed to parse: {error}")
            });
        assert!(
            profile.kind.is_some(),
            "template {name}.md must have a 'kind' field"
        );
        assert!(
            !definition.prompt_template.trim().is_empty(),
            "template {name}.md must have a non-empty prompt body"
        );
    }
}

#[test]
fn generated_defaults_load_with_seeded_agent_prompts() {
    let root = unique_temp_path("generated-defaults-load", "d");
    fs::create_dir_all(&root).unwrap();
    let workflow_path = root.join("WORKFLOW.md");
    fs::write(&workflow_path, default_workflow_md()).unwrap();
    ensure_repo_agent_prompt_files(&workflow_path).unwrap();

    let workflow = load_workflow_with_user_config(&workflow_path, None).unwrap();

    let _ = fs::remove_dir_all(&root);

    assert!(workflow.config.pipeline_active());
    assert_eq!(workflow.config.router_agent_name(), Some("router"));
    assert_eq!(
        workflow.config.agents.default.as_deref(),
        Some("implementer")
    );
    assert!(workflow.config.agents.profiles.contains_key("reviewer"));
}

#[test]
fn repo_local_config_overrides_workflow_front_matter() {
    let root = unique_temp_path("repo-overlay-root", "d");
    fs::create_dir_all(&root).unwrap();
    let workflow_path = root.join("WORKFLOW.md");
    fs::write(
        &workflow_path,
        r#"---
tracker:
  kind: none
workspace:
  checkout_kind: directory
---
# Prompt
"#,
    )
    .unwrap();

    let repo_config_path = repo_config_path(&workflow_path).unwrap();
    fs::create_dir_all(repo_config_path.parent().unwrap()).unwrap();
    fs::write(
        &repo_config_path,
        r#"
[tracker]
kind = "github"
repository = "penso/polyphony"
api_key = "test-token"

[workspace]
checkout_kind = "linked_worktree"
source_repo_path = "/tmp/polyphony"
"#,
    )
    .unwrap();

    let workflow = load_workflow_with_user_config(&workflow_path, None).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(workflow.config.tracker.kind, TrackerKind::Github);
    assert_eq!(
        workflow.config.tracker.repository.as_deref(),
        Some("penso/polyphony")
    );
    assert_eq!(
        workflow.config.workspace.checkout_kind,
        CheckoutKind::LinkedWorktree
    );
    assert_eq!(
        workflow.config.workspace.source_repo_path.as_deref(),
        Some(std::path::Path::new("/tmp/polyphony"))
    );
}

#[test]
fn automation_and_feedback_config_parse() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
tracker:
  kind: github
  repository: owner/repo
  api_key: test-token
workspace:
  checkout_kind: linked_worktree
  source_repo_path: /tmp/source
automation:
  enabled: true
  review_agent: reviewer
  commit_message: "fix({{ issue.identifier }}): handoff"
feedback:
  offered: [telegram, webhook]
  telegram:
    ops:
      bot_token: telegram-token
      chat_id: "12345"
  webhook:
    audit:
      url: https://example.com/hook
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
    reviewer:
      kind: codex
      transport: app_server
      command: codex app-server
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert!(config.automation.enabled);
    assert_eq!(config.automation.review_agent.as_deref(), Some("reviewer"));
    assert_eq!(config.feedback.offered, vec!["telegram", "webhook"]);
    assert_eq!(config.feedback.telegram["ops"].chat_id, "12345".to_string());
    assert_eq!(
        config.feedback.webhook["audit"].url.as_deref(),
        Some("https://example.com/hook")
    );
}

#[test]
fn agent_prompt_files_define_profiles_and_repo_overrides_global() {
    let root = unique_temp_path("agent-prompts-root", "d");
    let user_root = unique_temp_path("agent-prompts-user", "d");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&user_root).unwrap();

    let workflow_path = root.join("WORKFLOW.md");
    fs::write(
        &workflow_path,
        r#"---
agents:
  default: research
  profiles: {}
---
Shared workflow for {{ issue.identifier }}
"#,
    )
    .unwrap();

    let user_config_path = user_root.join("config.toml");
    let user_agent_dir = user_root.join("agents");
    fs::create_dir_all(&user_agent_dir).unwrap();
    fs::write(
        user_agent_dir.join("research.md"),
        r#"---
kind: kimi
model: kimi-2.5
fetch_models: false
---
Global research prompt for {{ issue.identifier }}
"#,
    )
    .unwrap();

    let repo_agent_dir = root.join(".polyphony").join("agents");
    fs::create_dir_all(&repo_agent_dir).unwrap();
    fs::write(
        repo_agent_dir.join("research.md"),
        r#"---
model: kimi-k2
---
Repo research prompt for {{ issue.title }}
"#,
    )
    .unwrap();

    let workflow = load_workflow_with_user_config(&workflow_path, Some(&user_config_path)).unwrap();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&user_root);

    let research = workflow.config.agents.profiles.get("research").unwrap();
    assert_eq!(research.kind, "kimi");
    assert_eq!(research.model.as_deref(), Some("kimi-k2"));
    assert!(!research.fetch_models);
    assert_eq!(
        workflow
            .agent_prompts
            .get("research")
            .unwrap()
            .prompt_template
            .trim(),
        "Repo research prompt for {{ issue.title }}"
    );
}

#[test]
fn render_agent_prompt_appends_role_specific_instructions() {
    let workflow = WorkflowDefinition {
        config: serde_yaml::from_str::<YamlValue>(
            r#"
agents:
  default: research
  profiles:
    research:
      kind: kimi
      model: kimi-2.5
      fetch_models: false
"#,
        )
        .unwrap(),
        prompt_template: "Base prompt for {{ issue.identifier }}".into(),
    };
    let config = ServiceConfig::from_workflow(&workflow).unwrap();
    let loaded = LoadedWorkflow {
        definition: workflow,
        config,
        path: unique_temp_path("render-agent-prompt", "md"),
        agent_prompts: [("research".to_string(), AgentPromptConfig {
            profile: AgentProfileOverride::default(),
            prompt_template: "Investigate carefully for {{ issue.title }}".into(),
            source: Default::default(),
        })]
        .into_iter()
        .collect(),
    };

    let rendered = render_agent_prompt(&loaded, "research", &sample_issue(), None, 1, 3).unwrap();

    assert!(rendered.contains("Base prompt for ISSUE-1"));
    assert!(rendered.contains("## Agent Instructions (research)"));
    assert!(rendered.contains("Investigate carefully for Title"));
}

#[test]
fn orchestration_router_agent_enables_pipeline_activation() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
orchestration:
  router_agent: router
agents:
  default: implementer
  profiles:
    implementer:
      kind: codex
      transport: app_server
      command: codex app-server
    router:
      kind: codex
      transport: app_server
      command: codex app-server
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert!(config.pipeline_active());
    assert_eq!(config.router_agent_name(), Some("router"));
}

#[test]
fn orchestration_dispatch_mode_parses_startup_mode() {
    let config = serde_yaml::from_str::<YamlValue>(
        r#"
orchestration:
  dispatch_mode: automatic
agents:
  default: implementer
  profiles:
    implementer:
      kind: claude
      transport: local_cli
      command: claude -p
"#,
    )
    .unwrap();
    let workflow = WorkflowDefinition {
        config,
        prompt_template: String::new(),
    };

    let config = ServiceConfig::from_workflow(&workflow).unwrap();

    assert_eq!(config.startup_dispatch_mode(), DispatchMode::Automatic);
}
