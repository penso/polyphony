use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::{Path, PathBuf},
};

use {
    config::{Config, Environment, File, FileFormat},
    liquid::{
        ParserBuilder,
        model::{Array, Object, Value},
        object,
    },
    polyphony_core::{
        AgentDefinition, AgentInteractionMode, AgentPromptMode, AgentTransport, CheckoutKind,
        Issue, TrackerQuery,
    },
    serde::{Deserialize, Serialize},
    serde_yaml::{Mapping, Value as YamlValue},
    thiserror::Error,
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub config: YamlValue,
    pub prompt_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerConfig {
    pub kind: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub project_slug: Option<String>,
    pub project_owner: Option<String>,
    pub project_number: Option<u32>,
    pub project_status_field: Option<String>,
    pub repository: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollingConfig {
    pub interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
    pub checkout_kind: String,
    pub sync_on_reuse: bool,
    pub transient_paths: Vec<String>,
    pub source_repo_path: Option<PathBuf>,
    pub clone_url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub max_concurrent_agents: usize,
    pub max_concurrent_agents_by_state: HashMap<String, usize>,
    pub max_retry_backoff_ms: u64,
    pub max_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CodexConfig {
    pub kind: Option<String>,
    pub command: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: i64,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentProfileConfig {
    pub kind: String,
    pub transport: Option<String>,
    pub command: Option<String>,
    pub fallbacks: Vec<String>,
    pub model: Option<String>,
    pub models: Vec<String>,
    pub models_command: Option<String>,
    #[serde(default = "default_true")]
    pub fetch_models: bool,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: i64,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentsConfig {
    pub default: Option<String>,
    pub by_state: HashMap<String, String>,
    pub by_label: HashMap<String, String>,
    pub profiles: HashMap<String, AgentProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutomationGitAuthorConfig {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutomationGitConfig {
    pub remote_name: String,
    pub author: AutomationGitAuthorConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TelegramFeedbackConfig {
    pub bot_token: Option<String>,
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebhookFeedbackConfig {
    pub url: Option<String>,
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FeedbackConfig {
    pub offered: Vec<String>,
    pub action_base_url: Option<String>,
    pub telegram: HashMap<String, TelegramFeedbackConfig>,
    pub webhook: HashMap<String, WebhookFeedbackConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub agents: AgentsConfig,
    pub automation: AutomationConfig,
    pub feedback: FeedbackConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub agents: AgentsConfig,
    pub codex: Option<CodexConfig>,
    pub provider: Option<CodexConfig>,
    pub automation: AutomationConfig,
    pub feedback: FeedbackConfig,
    pub server: ServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedWorkflow {
    pub definition: WorkflowDefinition,
    pub config: ServiceConfig,
    pub path: PathBuf,
}

pub fn load_workflow(path: impl AsRef<Path>) -> Result<LoadedWorkflow, Error> {
    let path = path.as_ref().to_path_buf();
    let raw = fs::read_to_string(&path).map_err(|_| Error::MissingWorkflowFile(path.clone()))?;
    let definition = parse_workflow(&raw)?;
    let config = ServiceConfig::from_workflow(&definition)?;
    Ok(LoadedWorkflow {
        definition,
        config,
        path,
    })
}

pub fn parse_workflow(raw: &str) -> Result<WorkflowDefinition, Error> {
    if !raw.starts_with("---") {
        return Ok(WorkflowDefinition {
            config: YamlValue::Mapping(Mapping::new()),
            prompt_template: raw.trim().to_string(),
        });
    }

    let mut parts = raw.splitn(3, "---");
    let _ = parts.next();
    let front_matter = parts
        .next()
        .ok_or_else(|| Error::WorkflowParse("missing closing front matter".into()))?;
    let body = parts
        .next()
        .ok_or_else(|| Error::WorkflowParse("missing body after front matter".into()))?;
    let config = serde_yaml::from_str::<YamlValue>(front_matter)
        .map_err(|err| Error::WorkflowParse(err.to_string()))?;
    if !matches!(config, YamlValue::Mapping(_)) {
        return Err(Error::FrontMatterNotMap);
    }

    Ok(WorkflowDefinition {
        config,
        prompt_template: body.trim().to_string(),
    })
}

impl ServiceConfig {
    pub fn from_workflow(workflow: &WorkflowDefinition) -> Result<Self, Error> {
        let front_matter = serde_yaml::to_string(&workflow.config)
            .map_err(|err| Error::WorkflowParse(err.to_string()))?;
        let built = Config::builder()
            .set_default("tracker.kind", "mock")
            .map_err(config_error)?
            .set_default("tracker.endpoint", "https://api.linear.app/graphql")
            .map_err(config_error)?
            .set_default("tracker.active_states", vec!["Todo", "In Progress"])
            .map_err(config_error)?
            .set_default("tracker.terminal_states", vec![
                "Closed",
                "Cancelled",
                "Canceled",
                "Duplicate",
                "Done",
                "Human Review",
            ])
            .map_err(config_error)?
            .set_default("polling.interval_ms", 30_000)
            .map_err(config_error)?
            .set_default(
                "workspace.root",
                env::temp_dir()
                    .join("symphony_workspaces")
                    .to_string_lossy()
                    .to_string(),
            )
            .map_err(config_error)?
            .set_default("workspace.checkout_kind", "directory")
            .map_err(config_error)?
            .set_default("workspace.sync_on_reuse", true)
            .map_err(config_error)?
            .set_default("workspace.transient_paths", vec![
                "tmp".to_string(),
                ".elixir_ls".to_string(),
            ])
            .map_err(config_error)?
            .set_default("hooks.timeout_ms", 60_000)
            .map_err(config_error)?
            .set_default("agent.max_concurrent_agents", 10)
            .map_err(config_error)?
            .set_default(
                "agent.max_concurrent_agents_by_state",
                HashMap::<String, i64>::new(),
            )
            .map_err(config_error)?
            .set_default("agent.max_retry_backoff_ms", 300_000)
            .map_err(config_error)?
            .set_default("agent.max_turns", 20)
            .map_err(config_error)?
            .set_default("agents", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("automation.enabled", false)
            .map_err(config_error)?
            .set_default("automation.draft_pull_requests", true)
            .map_err(config_error)?
            .set_default("automation.git.remote_name", "origin")
            .map_err(config_error)?
            .set_default("feedback", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("server", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .add_source(File::from_str(&front_matter, FileFormat::Yaml))
            .add_source(
                Environment::with_prefix("POLYPHONY")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .map_err(config_error)?;
        let raw = built
            .try_deserialize::<RawServiceConfig>()
            .map_err(config_error)?;
        let mut config = ServiceConfig {
            tracker: raw.tracker,
            polling: raw.polling,
            workspace: raw.workspace,
            hooks: raw.hooks,
            agent: raw.agent,
            agents: raw.agents,
            automation: raw.automation,
            feedback: raw.feedback,
            server: raw.server,
        };
        config.hydrate_agents(raw.codex, raw.provider)?;
        config.resolve();
        config.normalize();
        config.validate()?;
        Ok(config)
    }

    fn hydrate_agents(
        &mut self,
        codex: Option<CodexConfig>,
        legacy_provider: Option<CodexConfig>,
    ) -> Result<(), Error> {
        let shorthand = match (codex, legacy_provider) {
            (Some(_), Some(_)) => {
                return Err(Error::InvalidConfig(
                    "`codex` and legacy `provider` cannot both be configured".into(),
                ));
            },
            (Some(codex), None) => Some(codex),
            (None, Some(provider)) => Some(provider),
            (None, None) => None,
        };
        if shorthand.is_some() && self.agents.is_configured() {
            return Err(Error::InvalidConfig(
                "`codex` and legacy `provider` cannot be combined with `agents`; use `agents` for multi-agent routing".into(),
            ));
        }

        if let Some(shorthand) = shorthand {
            let (default_name, profile) = shorthand_agent_profile(shorthand);
            self.agents.default = Some(default_name.clone());
            self.agents.profiles.insert(default_name, profile);
        } else if self.agents.profiles.is_empty() {
            self.agents.default = Some("mock".into());
            self.agents
                .profiles
                .insert("mock".into(), default_mock_agent_profile());
        }

        if self.agents.default.is_none() && self.agents.profiles.len() == 1 {
            self.agents.default = self.agents.profiles.keys().next().cloned();
        }

        Ok(())
    }

    fn resolve(&mut self) {
        let tracker_api_key = match self.tracker.kind.as_str() {
            "linear" => self
                .tracker
                .api_key
                .clone()
                .or_else(|| env::var("LINEAR_API_KEY").ok()),
            "github" => self
                .tracker
                .api_key
                .clone()
                .or_else(|| env::var("GITHUB_TOKEN").ok())
                .or_else(|| env::var("GH_TOKEN").ok()),
            _ => self.tracker.api_key.clone(),
        };
        self.tracker.api_key = resolve_env_token(tracker_api_key);
        self.workspace.root = expand_path_like(&self.workspace.root);
        self.workspace.source_repo_path = self
            .workspace
            .source_repo_path
            .take()
            .map(|path| expand_path_like(&path));
        self.automation.review_agent = resolve_env_token(self.automation.review_agent.take());
        self.automation.commit_message = resolve_env_token(self.automation.commit_message.take());
        self.automation.pr_title = resolve_env_token(self.automation.pr_title.take());
        self.automation.pr_body = resolve_env_token(self.automation.pr_body.take());
        self.automation.review_prompt = resolve_env_token(self.automation.review_prompt.take());
        self.automation.git.author.name = resolve_env_token(self.automation.git.author.name.take());
        self.automation.git.author.email =
            resolve_env_token(self.automation.git.author.email.take());
        self.feedback.action_base_url = resolve_env_token(self.feedback.action_base_url.take());
        for config in self.feedback.telegram.values_mut() {
            config.bot_token = resolve_env_token(config.bot_token.take())
                .or_else(|| env::var("TELEGRAM_BOT_TOKEN").ok());
            config.chat_id = resolve_env_token(Some(config.chat_id.clone())).unwrap_or_default();
        }
        for config in self.feedback.webhook.values_mut() {
            config.url = resolve_env_token(config.url.take());
            config.bearer_token = resolve_env_token(config.bearer_token.take());
        }
        for profile in self.agents.profiles.values_mut() {
            profile.api_key = resolve_agent_api_key(&profile.kind, profile.api_key.clone());
            profile.base_url = resolve_env_token(profile.base_url.take());
            profile.command = resolve_env_token(profile.command.take());
            profile.models_command = resolve_env_token(profile.models_command.take());
            profile.credits_command = resolve_env_token(profile.credits_command.take());
            profile.spending_command = resolve_env_token(profile.spending_command.take());
            profile.env = profile
                .env
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        resolve_env_token(Some(value.clone())).unwrap_or_default(),
                    )
                })
                .collect();
        }
    }

    fn normalize(&mut self) {
        self.workspace.transient_paths = self
            .workspace
            .transient_paths
            .drain(..)
            .filter(|path| !path.trim().is_empty())
            .collect();
        self.agent.max_concurrent_agents_by_state = self
            .agent
            .max_concurrent_agents_by_state
            .drain()
            .filter_map(|(state, limit)| {
                if limit == 0 {
                    None
                } else {
                    Some((state.to_ascii_lowercase(), limit))
                }
            })
            .collect();
        self.agents.by_state = self
            .agents
            .by_state
            .drain()
            .map(|(state, agent)| (state.to_ascii_lowercase(), agent))
            .collect();
        self.agents.by_label = self
            .agents
            .by_label
            .drain()
            .map(|(label, agent)| (label.to_ascii_lowercase(), agent))
            .collect();
        for (name, profile) in &mut self.agents.profiles {
            if profile.kind.trim().is_empty() {
                profile.kind = name.clone();
            }
            if profile.turn_timeout_ms == 0 {
                profile.turn_timeout_ms = 3_600_000;
            }
            if profile.read_timeout_ms == 0 {
                profile.read_timeout_ms = 5_000;
            }
            if profile.stall_timeout_ms == 0 {
                profile.stall_timeout_ms = 300_000;
            }
            if profile.tmux_session_prefix.is_none() {
                profile.tmux_session_prefix = Some(name.clone());
            }
            if profile.idle_timeout_ms == 0 {
                profile.idle_timeout_ms = 5_000;
            }
            profile.fallbacks = profile
                .fallbacks
                .drain(..)
                .filter(|fallback| !fallback.trim().is_empty() && fallback != name)
                .collect();
        }
        if self.automation.git.remote_name.trim().is_empty() {
            self.automation.git.remote_name = "origin".into();
        }
        self.feedback.offered = self
            .feedback
            .offered
            .drain(..)
            .map(|item| item.to_ascii_lowercase())
            .filter(|item| !item.trim().is_empty())
            .collect();
        if self.hooks.timeout_ms == 0 {
            self.hooks.timeout_ms = 60_000;
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.tracker.kind.is_empty() {
            return Err(Error::InvalidConfig("tracker.kind is required".into()));
        }
        if self.tracker.kind == "linear" {
            if self
                .tracker
                .api_key
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "tracker.api_key is required for linear".into(),
                ));
            }
            if self
                .tracker
                .project_slug
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "tracker.project_slug is required for linear".into(),
                ));
            }
        }
        if self.tracker.kind == "github" {
            if self
                .tracker
                .repository
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "tracker.repository is required for github".into(),
                ));
            }
            if self
                .tracker
                .api_key
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "tracker.api_key is required for github".into(),
                ));
            }
        }
        if self.agents.profiles.is_empty() {
            return Err(Error::InvalidConfig(
                "agents.profiles must contain at least one configured agent".into(),
            ));
        }
        let default_agent = self
            .agents
            .default
            .as_deref()
            .ok_or_else(|| Error::InvalidConfig("agents.default is required".into()))?;
        if !self.agents.profiles.contains_key(default_agent) {
            return Err(Error::InvalidConfig(format!(
                "agents.default `{default_agent}` is not defined"
            )));
        }
        for (agent_name, profile) in &self.agents.profiles {
            if matches!(
                infer_agent_transport(profile),
                AgentTransport::AppServer | AgentTransport::LocalCli
            ) && profile
                .command
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(Error::InvalidConfig(format!(
                    "agents.profiles.{agent_name}.command must be non-empty"
                )));
            }
            if matches!(infer_agent_transport(profile), AgentTransport::OpenAiChat)
                && profile
                    .model
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && profile.models.is_empty()
                && !profile.fetch_models
            {
                return Err(Error::InvalidConfig(format!(
                    "agents.profiles.{agent_name}.model is required for openai_chat agents when automatic model discovery is disabled"
                )));
            }
            if matches!(infer_agent_transport(profile), AgentTransport::OpenAiChat)
                && profile
                    .api_key
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
            {
                return Err(Error::InvalidConfig(format!(
                    "agents.profiles.{agent_name}.api_key is required for openai_chat agents"
                )));
            }
            for fallback in &profile.fallbacks {
                if fallback == agent_name {
                    return Err(Error::InvalidConfig(format!(
                        "agents.profiles.{agent_name}.fallbacks cannot contain itself"
                    )));
                }
                if !self.agents.profiles.contains_key(fallback) {
                    return Err(Error::InvalidConfig(format!(
                        "agents.profiles.{agent_name}.fallbacks references unknown agent `{fallback}`"
                    )));
                }
            }
        }
        for configured_agent in self
            .agents
            .by_state
            .values()
            .chain(self.agents.by_label.values())
        {
            if !self.agents.profiles.contains_key(configured_agent) {
                return Err(Error::InvalidConfig(format!(
                    "agent selector references unknown agent `{configured_agent}`"
                )));
            }
        }
        if self.workspace.checkout_kind != "directory"
            && self.workspace.source_repo_path.is_none()
            && self.workspace.clone_url.is_none()
        {
            return Err(Error::InvalidConfig(
                "workspace.source_repo_path or workspace.clone_url is required for git-backed workspaces".into(),
            ));
        }
        if self.automation.enabled {
            if self.tracker.kind != "github" {
                return Err(Error::InvalidConfig(
                    "automation.enabled currently requires tracker.kind = github".into(),
                ));
            }
            if self.workspace.checkout_kind == "directory" {
                return Err(Error::InvalidConfig(
                    "automation.enabled requires a git-backed workspace checkout".into(),
                ));
            }
            if let Some(agent_name) = &self.automation.review_agent
                && !self.agents.profiles.contains_key(agent_name)
            {
                return Err(Error::InvalidConfig(format!(
                    "automation.review_agent `{agent_name}` is not defined"
                )));
            }
        }
        for offered in &self.feedback.offered {
            match offered.as_str() {
                "telegram" | "webhook" => {},
                _ => {
                    return Err(Error::InvalidConfig(format!(
                        "feedback.offered contains unknown channel `{offered}`"
                    )));
                },
            }
        }
        for (name, config) in &self.feedback.telegram {
            if config.chat_id.trim().is_empty() {
                return Err(Error::InvalidConfig(format!(
                    "feedback.telegram.{name}.chat_id must be non-empty"
                )));
            }
            if config
                .bot_token
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(Error::InvalidConfig(format!(
                    "feedback.telegram.{name}.bot_token must be non-empty"
                )));
            }
        }
        for (name, config) in &self.feedback.webhook {
            if config.url.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(Error::InvalidConfig(format!(
                    "feedback.webhook.{name}.url must be non-empty"
                )));
            }
        }
        Ok(())
    }

    pub fn tracker_query(&self) -> TrackerQuery {
        TrackerQuery {
            project_slug: self.tracker.project_slug.clone(),
            repository: self.tracker.repository.clone(),
            active_states: self.tracker.active_states.clone(),
            terminal_states: self.tracker.terminal_states.clone(),
        }
    }

    pub fn is_active_state(&self, state: &str) -> bool {
        self.tracker
            .active_states
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(state))
    }

    pub fn is_terminal_state(&self, state: &str) -> bool {
        self.tracker
            .terminal_states
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(state))
    }

    pub fn workspace_checkout_kind(&self) -> CheckoutKind {
        match self.workspace.checkout_kind.as_str() {
            "linked_worktree" => CheckoutKind::LinkedWorktree,
            "discrete_clone" => CheckoutKind::DiscreteClone,
            _ => CheckoutKind::Directory,
        }
    }

    pub fn state_concurrency_limit(&self, state: &str) -> Option<usize> {
        self.agent
            .max_concurrent_agents_by_state
            .get(&state.to_ascii_lowercase())
            .copied()
    }

    pub fn all_agents(&self) -> Vec<AgentDefinition> {
        self.agents
            .profiles
            .iter()
            .map(|(name, profile)| agent_definition(name, profile))
            .collect()
    }

    pub fn candidate_agents_for_issue(&self, issue: &Issue) -> Result<Vec<AgentDefinition>, Error> {
        let selected_name =
            if let Some(agent_name) = self.agents.by_state.get(&issue.state.to_ascii_lowercase()) {
                agent_name.clone()
            } else if let Some(agent_name) = issue.labels.iter().find_map(|label| {
                self.agents
                    .by_label
                    .get(&label.to_ascii_lowercase())
                    .cloned()
            }) {
                agent_name
            } else {
                self.agents
                    .default
                    .clone()
                    .ok_or_else(|| Error::InvalidConfig("agents.default is required".into()))?
            };

        self.expand_agent_candidates(&selected_name)
    }

    pub fn select_agent_for_issue(&self, issue: &Issue) -> Result<AgentDefinition, Error> {
        self.candidate_agents_for_issue(issue)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidConfig("no candidate agents are configured".into()))
    }

    pub fn review_agent(&self) -> Result<Option<AgentDefinition>, Error> {
        let Some(agent_name) = &self.automation.review_agent else {
            return Ok(None);
        };
        let profile =
            self.agents.profiles.get(agent_name).ok_or_else(|| {
                Error::InvalidConfig(format!("unknown review agent `{agent_name}`"))
            })?;
        Ok(Some(agent_definition(agent_name, profile)))
    }

    fn expand_agent_candidates(&self, selected_name: &str) -> Result<Vec<AgentDefinition>, Error> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![selected_name.to_string()];
        let mut candidates = Vec::new();
        while let Some(agent_name) = stack.pop() {
            if !seen.insert(agent_name.clone()) {
                continue;
            }
            let profile = self.agents.profiles.get(&agent_name).ok_or_else(|| {
                Error::InvalidConfig(format!("unknown selected agent `{agent_name}`"))
            })?;
            stack.extend(profile.fallbacks.iter().rev().cloned());
            candidates.push(agent_definition(&agent_name, profile));
        }
        Ok(candidates)
    }
}

impl AgentsConfig {
    fn is_configured(&self) -> bool {
        self.default.is_some()
            || !self.by_state.is_empty()
            || !self.by_label.is_empty()
            || !self.profiles.is_empty()
    }
}

pub fn render_prompt(
    workflow: &WorkflowDefinition,
    issue: &Issue,
    attempt: Option<u32>,
) -> Result<String, Error> {
    let source = if workflow.prompt_template.trim().is_empty() {
        "You are working on an issue from Linear."
    } else {
        workflow.prompt_template.as_str()
    };
    render_issue_template(source, issue, attempt, Object::new())
}

pub fn render_issue_template(
    source: &str,
    issue: &Issue,
    attempt: Option<u32>,
    extra: Object,
) -> Result<String, Error> {
    let parser = ParserBuilder::with_stdlib()
        .build()
        .map_err(|err| Error::TemplateParse(err.to_string()))?;
    let template = parser
        .parse(source)
        .map_err(|err| Error::TemplateParse(err.to_string()))?;
    let mut globals = object!({
        "issue": issue_to_liquid(issue),
        "attempt": attempt.map(Value::scalar).unwrap_or(Value::Nil),
    });
    for (key, value) in extra {
        globals.insert(key, value);
    }
    template
        .render(&globals)
        .map_err(|err| Error::TemplateRender(err.to_string()))
}

pub fn render_issue_template_with_strings(
    source: &str,
    issue: &Issue,
    attempt: Option<u32>,
    extra: &[(&str, String)],
) -> Result<String, Error> {
    let mut globals = Object::new();
    for (key, value) in extra {
        globals.insert(key.to_string().into(), Value::scalar(value.clone()));
    }
    render_issue_template(source, issue, attempt, globals)
}

fn issue_to_liquid(issue: &Issue) -> Value {
    let mut issue_obj = Object::new();
    issue_obj.insert("id".into(), Value::scalar(issue.id.clone()));
    issue_obj.insert("identifier".into(), Value::scalar(issue.identifier.clone()));
    issue_obj.insert("title".into(), Value::scalar(issue.title.clone()));
    issue_obj.insert(
        "description".into(),
        issue
            .description
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "priority".into(),
        issue.priority.map(Value::scalar).unwrap_or(Value::Nil),
    );
    issue_obj.insert("state".into(), Value::scalar(issue.state.clone()));
    issue_obj.insert(
        "branch_name".into(),
        issue
            .branch_name
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "url".into(),
        issue
            .url
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "author".into(),
        issue
            .author
            .as_ref()
            .map(issue_author_to_liquid)
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "labels".into(),
        Value::Array(
            issue
                .labels
                .iter()
                .cloned()
                .map(Value::scalar)
                .collect::<Array>(),
        ),
    );
    issue_obj.insert(
        "comments".into(),
        Value::Array(
            issue
                .comments
                .iter()
                .map(|comment| {
                    let mut obj = Object::new();
                    obj.insert("id".into(), Value::scalar(comment.id.clone()));
                    obj.insert("body".into(), Value::scalar(comment.body.clone()));
                    obj.insert(
                        "author".into(),
                        comment
                            .author
                            .as_ref()
                            .map(issue_author_to_liquid)
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "url".into(),
                        comment
                            .url
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "created_at".into(),
                        comment
                            .created_at
                            .map(|value| Value::scalar(value.to_rfc3339()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "updated_at".into(),
                        comment
                            .updated_at
                            .map(|value| Value::scalar(value.to_rfc3339()))
                            .unwrap_or(Value::Nil),
                    );
                    Value::Object(obj)
                })
                .collect::<Array>(),
        ),
    );
    issue_obj.insert(
        "blocked_by".into(),
        Value::Array(
            issue
                .blocked_by
                .iter()
                .map(|blocker| {
                    let mut obj = Object::new();
                    obj.insert(
                        "id".into(),
                        blocker
                            .id
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "identifier".into(),
                        blocker
                            .identifier
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "state".into(),
                        blocker
                            .state
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    Value::Object(obj)
                })
                .collect::<Array>(),
        ),
    );
    Value::Object(issue_obj)
}

fn issue_author_to_liquid(author: &polyphony_core::IssueAuthor) -> Value {
    let mut obj = Object::new();
    obj.insert(
        "id".into(),
        author
            .id
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "username".into(),
        author
            .username
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "display_name".into(),
        author
            .display_name
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "role".into(),
        author
            .role
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "trust_level".into(),
        author
            .trust_level
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "url".into(),
        author
            .url
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    Value::Object(obj)
}

fn resolve_env_token(value: Option<String>) -> Option<String> {
    let value = value?;
    if let Some(name) = value.strip_prefix('$') {
        let resolved = env::var(name).ok()?;
        if resolved.is_empty() {
            None
        } else {
            Some(resolved)
        }
    } else if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn resolve_agent_api_key(kind: &str, api_key: Option<String>) -> Option<String> {
    let fallback = match kind {
        "openai" => env::var("OPENAI_API_KEY").ok(),
        "anthropic" | "claude" => env::var("ANTHROPIC_API_KEY").ok(),
        "copilot" | "github-copilot" => env::var("GITHUB_TOKEN")
            .ok()
            .or_else(|| env::var("GH_TOKEN").ok()),
        "kimi" | "kimi-2.5" | "kimi-k2" | "moonshot" | "moonshotai" => env::var("KIMI_API_KEY")
            .ok()
            .or_else(|| env::var("MOONSHOT_API_KEY").ok()),
        _ => None,
    };
    resolve_env_token(api_key.or(fallback))
}

fn shorthand_agent_profile(config: CodexConfig) -> (String, AgentProfileConfig) {
    let kind = normalize_optional_string(config.kind).unwrap_or_else(|| "codex".into());
    let name = kind.clone();
    let command = normalize_optional_string(config.command)
        .or_else(|| default_single_agent_command(&kind).map(str::to_string));
    (name.clone(), AgentProfileConfig {
        kind,
        transport: None,
        command,
        fallbacks: Vec::new(),
        model: None,
        models: Vec::new(),
        models_command: None,
        fetch_models: true,
        base_url: None,
        api_key: None,
        approval_policy: config.approval_policy,
        thread_sandbox: config.thread_sandbox,
        turn_sandbox_policy: config.turn_sandbox_policy,
        turn_timeout_ms: config.turn_timeout_ms,
        read_timeout_ms: config.read_timeout_ms,
        stall_timeout_ms: config.stall_timeout_ms,
        credits_command: config.credits_command,
        spending_command: config.spending_command,
        use_tmux: false,
        tmux_session_prefix: Some(name),
        interaction_mode: None,
        prompt_mode: None,
        idle_timeout_ms: 5_000,
        completion_sentinel: None,
        env: BTreeMap::new(),
    })
}

fn default_mock_agent_profile() -> AgentProfileConfig {
    AgentProfileConfig {
        kind: "mock".into(),
        transport: None,
        command: Some("mock".into()),
        fallbacks: Vec::new(),
        model: None,
        models: Vec::new(),
        models_command: None,
        fetch_models: true,
        base_url: None,
        api_key: None,
        approval_policy: None,
        thread_sandbox: None,
        turn_sandbox_policy: None,
        turn_timeout_ms: 0,
        read_timeout_ms: 0,
        stall_timeout_ms: 0,
        credits_command: None,
        spending_command: None,
        use_tmux: false,
        tmux_session_prefix: Some("mock".into()),
        interaction_mode: None,
        prompt_mode: None,
        idle_timeout_ms: 5_000,
        completion_sentinel: None,
        env: BTreeMap::new(),
    }
}

fn default_single_agent_command(kind: &str) -> Option<&'static str> {
    match kind {
        "codex" => Some("codex app-server"),
        "mock" => Some("mock"),
        _ => None,
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn infer_agent_transport(profile: &AgentProfileConfig) -> AgentTransport {
    match profile.transport.as_deref() {
        Some("mock") => AgentTransport::Mock,
        Some("app_server") => AgentTransport::AppServer,
        Some("local_cli") => AgentTransport::LocalCli,
        Some("openai_chat") => AgentTransport::OpenAiChat,
        _ => match profile.kind.as_str() {
            "mock" => AgentTransport::Mock,
            "codex" => AgentTransport::AppServer,
            "openai" | "openai-compatible" | "openrouter" | "kimi" | "kimi-2.5" | "kimi-k2"
            | "moonshot" | "moonshotai" => AgentTransport::OpenAiChat,
            _ => AgentTransport::LocalCli,
        },
    }
}

fn agent_definition(name: &str, profile: &AgentProfileConfig) -> AgentDefinition {
    AgentDefinition {
        name: name.to_string(),
        kind: profile.kind.clone(),
        transport: infer_agent_transport(profile),
        command: profile.command.clone(),
        fallback_agents: profile.fallbacks.clone(),
        model: profile.model.clone(),
        models: profile.models.clone(),
        models_command: profile.models_command.clone(),
        fetch_models: profile.fetch_models,
        base_url: profile
            .base_url
            .clone()
            .or_else(|| default_agent_base_url(&profile.kind)),
        api_key: profile.api_key.clone(),
        approval_policy: profile.approval_policy.clone(),
        thread_sandbox: profile.thread_sandbox.clone(),
        turn_sandbox_policy: profile.turn_sandbox_policy.clone(),
        turn_timeout_ms: profile.turn_timeout_ms,
        read_timeout_ms: profile.read_timeout_ms,
        stall_timeout_ms: profile.stall_timeout_ms,
        credits_command: profile.credits_command.clone(),
        spending_command: profile.spending_command.clone(),
        use_tmux: profile.use_tmux,
        tmux_session_prefix: profile.tmux_session_prefix.clone(),
        interaction_mode: parse_interaction_mode(profile.interaction_mode.as_deref()),
        prompt_mode: parse_prompt_mode(
            profile.prompt_mode.as_deref(),
            profile.use_tmux,
            profile.interaction_mode.as_deref(),
        ),
        idle_timeout_ms: profile.idle_timeout_ms,
        completion_sentinel: profile.completion_sentinel.clone(),
        env: profile.env.clone(),
    }
}

fn default_agent_base_url(kind: &str) -> Option<String> {
    match kind {
        "kimi" | "kimi-2.5" | "kimi-k2" | "moonshot" | "moonshotai" => {
            Some("https://api.moonshot.ai/v1".into())
        },
        _ => None,
    }
}

fn parse_interaction_mode(value: Option<&str>) -> AgentInteractionMode {
    match value {
        Some("interactive") => AgentInteractionMode::Interactive,
        _ => AgentInteractionMode::OneShot,
    }
}

fn parse_prompt_mode(
    value: Option<&str>,
    use_tmux: bool,
    interaction_mode: Option<&str>,
) -> AgentPromptMode {
    match value {
        Some("stdin") => AgentPromptMode::Stdin,
        Some("tmux_paste") => AgentPromptMode::TmuxPaste,
        _ if interaction_mode == Some("interactive") && use_tmux => AgentPromptMode::TmuxPaste,
        _ if interaction_mode == Some("interactive") => AgentPromptMode::Stdin,
        _ if use_tmux => AgentPromptMode::TmuxPaste,
        _ => AgentPromptMode::Env,
    }
}

fn expand_path_like(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    let expanded = if let Some(name) = value.strip_prefix('$') {
        env::var(name).unwrap_or_default()
    } else {
        value.to_string()
    };
    if expanded.starts_with('~')
        && let Some(home) = dirs::home_dir()
    {
        return home.join(expanded.trim_start_matches("~/"));
    }
    PathBuf::from(expanded)
}

fn config_error(error: config::ConfigError) -> Error {
    Error::Config(error.to_string())
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use {
        super::{ServiceConfig, WorkflowDefinition, render_issue_template},
        polyphony_core::{AgentInteractionMode, AgentPromptMode, AgentTransport, Issue},
        serde_yaml::Value as YamlValue,
    };

    fn sample_issue() -> Issue {
        Issue {
            id: "1".into(),
            identifier: "ISSUE-1".into(),
            title: "Title".into(),
            description: None,
            priority: None,
            state: "Todo".into(),
            branch_name: None,
            url: None,
            author: None,
            labels: Vec::new(),
            comments: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn workspace_defaults_include_reuse_and_transient_paths() {
        let workflow = WorkflowDefinition {
            config: YamlValue::Mapping(Default::default()),
            prompt_template: String::new(),
        };

        let config = ServiceConfig::from_workflow(&workflow).unwrap();

        assert!(config.workspace.sync_on_reuse);
        assert_eq!(config.workspace.transient_paths, vec!["tmp", ".elixir_ls"]);
        assert_eq!(config.agents.default.as_deref(), Some("mock"));
        assert!(config.agents.profiles.contains_key("mock"));
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
    fn render_template_rejects_unknown_variables() {
        let error = render_issue_template(
            "{{ missing_value }}",
            &sample_issue(),
            None,
            Default::default(),
        )
        .unwrap_err();

        assert!(matches!(error, super::Error::TemplateRender(_)));
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

        assert!(matches!(error, super::Error::TemplateParse(_)));
        assert!(error.to_string().contains("Unknown filter"));
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
            description: None,
            priority: None,
            state: "Todo".into(),
            branch_name: None,
            url: None,
            author: None,
            labels: vec!["risky".into()],
            comments: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
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
            description: None,
            priority: None,
            state: "Todo".into(),
            branch_name: None,
            url: None,
            author: None,
            labels: Vec::new(),
            comments: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
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

        let claude_agent = super::agent_definition("claude", claude);
        let claude_tmux_agent = super::agent_definition("claude_tmux", claude_tmux);

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
                description: None,
                priority: None,
                state: "Todo".into(),
                branch_name: None,
                url: None,
                author: None,
                labels: Vec::new(),
                comments: Vec::new(),
                blocked_by: Vec::new(),
                created_at: None,
                updated_at: None,
            })
            .unwrap();

        assert!(matches!(kimi.transport, AgentTransport::OpenAiChat));
        assert_eq!(kimi.base_url.as_deref(), Some("https://api.moonshot.ai/v1"));
        assert_eq!(kimi.model.as_deref(), Some("kimi-2.5"));
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
}
