use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use {
    polyphony_core::{CheckoutKind, TrackerKind},
    serde::{Deserialize, Serialize},
    serde_yaml::Value as YamlValue,
    thiserror::Error,
};

const DEFAULT_USER_CONFIG_TEMPLATE: &str = include_str!("../../../templates/config.toml");
const DEFAULT_WORKFLOW_TEMPLATE: &str = include_str!("../../../templates/WORKFLOW.md");
const DEFAULT_REPO_CONFIG_TEMPLATE: &str = include_str!("../../../templates/repo-config.toml");
const DEFAULT_REPO_AGENT_ROUTER_TEMPLATE: &str =
    include_str!("../../../templates/agents/router.md");
const DEFAULT_REPO_AGENT_IMPLEMENTER_TEMPLATE: &str =
    include_str!("../../../templates/agents/implementer.md");
const DEFAULT_REPO_AGENT_RESEARCHER_TEMPLATE: &str =
    include_str!("../../../templates/agents/researcher.md");
const DEFAULT_REPO_AGENT_TESTER_TEMPLATE: &str =
    include_str!("../../../templates/agents/tester.md");
const DEFAULT_REPO_AGENT_REVIEWER_TEMPLATE: &str =
    include_str!("../../../templates/agents/reviewer.md");
const DEFAULT_LINEAR_ENDPOINT: &str = "https://api.linear.app/graphql";

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing_workflow_file: {0}")]
    MissingWorkflowFile(PathBuf),
    #[error("workflow_parse_error: {0}")]
    WorkflowParse(String),
    #[error("workflow_front_matter_not_a_map")]
    FrontMatterNotMap,
    #[error("template_parse_error: {0}")]
    TemplateParse(String),
    #[error("template_render_error: {0}")]
    TemplateRender(String),
    #[error("config_error: {0}")]
    Config(String),
    #[error("invalid_config: {0}")]
    InvalidConfig(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowDefinition {
    pub config: YamlValue,
    pub prompt_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackerConfig {
    pub kind: TrackerKind,
    pub profile: Option<String>,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub project_slug: Option<String>,
    pub project_owner: Option<String>,
    pub project_number: Option<u32>,
    pub project_status_field: Option<String>,
    pub repository: Option<String>,
    pub team_id: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
struct TrackerProfileConfig {
    pub kind: Option<TrackerKind>,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub project_slug: Option<String>,
    pub project_owner: Option<String>,
    pub project_number: Option<u32>,
    pub project_status_field: Option<String>,
    pub repository: Option<String>,
    pub team_id: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
struct TrackersConfig {
    pub profiles: HashMap<String, TrackerProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollingConfig {
    pub interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
    pub checkout_kind: CheckoutKind,
    pub sync_on_reuse: bool,
    pub transient_paths: Vec<String>,
    pub source_repo_path: Option<PathBuf>,
    pub clone_url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub after_outcome: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    pub max_concurrent_agents: usize,
    pub max_concurrent_agents_by_state: HashMap<String, usize>,
    pub max_retry_backoff_ms: u64,
    pub max_turns: u32,
    pub continuation_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct OrchestrationConfig {
    pub router_agent: Option<String>,
    pub mode: String,
    pub dispatch_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct CodexConfig {
    pub kind: Option<String>,
    pub command: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: Option<i64>,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AgentProfileConfig {
    pub kind: String,
    pub transport: Option<String>,
    pub command: Option<String>,
    pub fallbacks: Vec<String>,
    pub model: Option<String>,
    pub models: Vec<String>,
    pub models_command: Option<String>,
    #[serde(default = "crate::service::default_true")]
    pub fetch_models: bool,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: Option<i64>,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
    pub use_tmux: bool,
    pub tmux_session_prefix: Option<String>,
    pub interaction_mode: Option<String>,
    pub prompt_mode: Option<String>,
    pub idle_timeout_ms: u64,
    pub completion_sentinel: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AgentProfileOverride {
    pub kind: Option<String>,
    pub transport: Option<String>,
    pub command: Option<String>,
    pub fallbacks: Option<Vec<String>>,
    pub model: Option<String>,
    pub models: Option<Vec<String>>,
    pub models_command: Option<String>,
    pub fetch_models: Option<bool>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: Option<u64>,
    pub read_timeout_ms: Option<u64>,
    pub stall_timeout_ms: Option<i64>,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
    pub use_tmux: Option<bool>,
    pub tmux_session_prefix: Option<String>,
    pub interaction_mode: Option<String>,
    pub prompt_mode: Option<String>,
    pub idle_timeout_ms: Option<u64>,
    pub completion_sentinel: Option<String>,
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AgentPromptConfig {
    pub profile: AgentProfileOverride,
    pub prompt_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ToolPolicyConfig {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ToolsConfig {
    pub enabled: bool,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub by_agent: HashMap<String, ToolPolicyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AgentsConfig {
    pub default: Option<String>,
    pub reviewer: Option<String>,
    pub by_state: HashMap<String, String>,
    pub by_label: HashMap<String, String>,
    pub profiles: HashMap<String, AgentProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DaemonConfig {
    pub listen_address: String,
    pub listen_port: u16,
    pub auth_token: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".into(),
            listen_port: 0,
            auth_token: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AutomationGitAuthorConfig {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AutomationGitConfig {
    pub remote_name: String,
    pub author: AutomationGitAuthorConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AutomationConfig {
    pub enabled: bool,
    pub draft_pull_requests: bool,
    pub review_agent: Option<String>,
    pub commit_message: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub review_prompt: Option<String>,
    pub git: AutomationGitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct PullRequestReviewConfig {
    pub enabled: bool,
    pub provider: String,
    pub agent: Option<String>,
    pub debounce_seconds: u64,
    pub include_drafts: bool,
    pub only_labels: Vec<String>,
    pub ignore_labels: Vec<String>,
    pub ignore_authors: Vec<String>,
    pub ignore_bot_authors: bool,
    pub comment_mode: String,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ReviewTriggersConfig {
    pub pr_reviews: PullRequestReviewConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct TelegramFeedbackConfig {
    pub bot_token: Option<String>,
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct WebhookFeedbackConfig {
    pub url: Option<String>,
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct FeedbackConfig {
    pub offered: Vec<String>,
    pub action_base_url: Option<String>,
    pub telegram: HashMap<String, TelegramFeedbackConfig>,
    pub webhook: HashMap<String, WebhookFeedbackConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct PipelineStageConfig {
    pub category: String,
    pub agent: Option<String>,
    pub prompt: Option<String>,
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct PipelineConfig {
    pub enabled: bool,
    pub planner_agent: Option<String>,
    pub planner_prompt: Option<String>,
    pub stages: Vec<PipelineStageConfig>,
    pub replan_on_failure: bool,
    pub validation_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub tools: ToolsConfig,
    pub agent: AgentConfig,
    pub orchestration: OrchestrationConfig,
    pub agents: AgentsConfig,
    pub pipeline: PipelineConfig,
    pub automation: AutomationConfig,
    pub review_triggers: ReviewTriggersConfig,
    pub feedback: FeedbackConfig,
    pub server: ServerConfig,
    pub daemon: DaemonConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct RawServiceConfig {
    pub tracker: TrackerConfig,
    #[serde(default)]
    pub trackers: TrackersConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub orchestration: OrchestrationConfig,
    pub agents: AgentsConfig,
    pub codex: Option<CodexConfig>,
    pub provider: Option<CodexConfig>,
    pub pipeline: PipelineConfig,
    pub automation: AutomationConfig,
    pub review_triggers: ReviewTriggersConfig,
    pub feedback: FeedbackConfig,
    pub server: ServerConfig,
    pub daemon: DaemonConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedWorkflow {
    pub definition: WorkflowDefinition,
    pub config: ServiceConfig,
    pub path: PathBuf,
    pub agent_prompts: HashMap<String, AgentPromptConfig>,
}

mod files;
mod prelude;
pub(crate) mod render;
pub(crate) mod service;

#[cfg(test)]
mod tests;

pub(crate) use crate::service::*;
pub use crate::{
    files::{
        agent_prompt_dirs, default_repo_agent_prompt_templates, default_repo_config_toml,
        default_user_config_toml, default_workflow_md, ensure_repo_agent_prompt_files,
        ensure_repo_config_file, ensure_user_config_file, ensure_workflow_file, load_workflow,
        load_workflow_with_user_config, repo_agent_prompt_dir, repo_config_path,
        seed_repo_config_with_beads, seed_repo_config_with_github, user_agent_prompt_dir,
        user_config_path,
    },
    render::{
        agent_definition, apply_agent_prompt_template, render_agent_prompt, render_issue_template,
        render_issue_template_with_strings, render_prompt, render_turn_prompt,
        render_turn_template,
    },
    service::parse_workflow,
};
