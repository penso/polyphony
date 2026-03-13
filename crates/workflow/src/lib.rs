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

const DEFAULT_USER_CONFIG_TEMPLATE: &str = include_str!("../../../templates/config.toml");
const DEFAULT_WORKFLOW_TEMPLATE: &str = include_str!("../../../templates/WORKFLOW.md");
const DEFAULT_REPO_CONFIG_TEMPLATE: &str = include_str!("../../../templates/repo-config.toml");
const DEFAULT_TRACKER_KIND: &str = "none";
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
    pub kind: String,
    pub profile: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
struct TrackerProfileConfig {
    pub kind: Option<String>,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub project_slug: Option<String>,
    pub project_owner: Option<String>,
    pub project_number: Option<u32>,
    pub project_status_field: Option<String>,
    pub repository: Option<String>,
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
    pub checkout_kind: String,
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
    #[serde(default = "default_true")]
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
pub struct AgentsConfig {
    pub default: Option<String>,
    pub by_state: HashMap<String, String>,
    pub by_label: HashMap<String, String>,
    pub profiles: HashMap<String, AgentProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub port: Option<u16>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct RawServiceConfig {
    pub tracker: TrackerConfig,
    #[serde(default)]
    pub trackers: TrackersConfig,
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
    load_workflow_with_user_config(path, None)
}

pub fn load_workflow_with_user_config(
    path: impl AsRef<Path>,
    user_config_path: Option<&Path>,
) -> Result<LoadedWorkflow, Error> {
    let path = path.as_ref().to_path_buf();
    let raw = fs::read_to_string(&path).map_err(|_| Error::MissingWorkflowFile(path.clone()))?;
    let definition = parse_workflow(&raw)?;
    let repo_config_path = repo_config_path(&path)?;
    let config = ServiceConfig::from_workflow_with_configs(
        &definition,
        user_config_path,
        Some(&repo_config_path),
    )?;
    Ok(LoadedWorkflow {
        definition,
        config,
        path,
    })
}

pub fn user_config_path() -> Result<PathBuf, Error> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("could not resolve ~/.config/polyphony/config.toml".into()))?;
    Ok(home.join(".config").join("polyphony").join("config.toml"))
}

pub fn repo_config_path(path: impl AsRef<Path>) -> Result<PathBuf, Error> {
    Ok(workflow_root_dir(path.as_ref())?
        .join(".polyphony")
        .join("config.toml"))
}

pub fn ensure_user_config_file(path: impl AsRef<Path>) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "config path")?;
    fs::write(path, default_user_config_toml())
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

pub fn ensure_workflow_file(path: impl AsRef<Path>) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "workflow path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "workflow path")?;
    fs::write(path, default_workflow_md())
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

pub fn ensure_repo_config_file(
    path: impl AsRef<Path>,
    source_repo_path: &Path,
) -> Result<bool, Error> {
    let path = path.as_ref();
    if path.exists() {
        if path.is_file() {
            return Ok(false);
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            path.display()
        )));
    }
    ensure_parent_dir(path, "repo config path")?;
    fs::write(path, default_repo_config_toml(source_repo_path))
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))?;
    Ok(true)
}

fn ensure_parent_dir(path: &Path, label: &str) -> Result<(), Error> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|error| {
        Error::Config(format!(
            "creating `{}` for {label} failed: {error}",
            parent.display()
        ))
    })
}

pub fn default_user_config_toml() -> &'static str {
    DEFAULT_USER_CONFIG_TEMPLATE
}

pub fn default_workflow_md() -> &'static str {
    DEFAULT_WORKFLOW_TEMPLATE
}

pub fn default_repo_config_toml(source_repo_path: &Path) -> String {
    DEFAULT_REPO_CONFIG_TEMPLATE.replace(
        "{{SOURCE_REPO_PATH}}",
        &source_repo_path.display().to_string(),
    )
}

fn default_active_states() -> Vec<String> {
    vec!["Todo".to_string(), "In Progress".to_string()]
}

fn default_terminal_states() -> Vec<String> {
    vec![
        "Closed".to_string(),
        "Cancelled".to_string(),
        "Canceled".to_string(),
        "Duplicate".to_string(),
        "Done".to_string(),
        "Human Review".to_string(),
    ]
}

fn apply_tracker_profile(
    mut tracker: TrackerConfig,
    profiles: &HashMap<String, TrackerProfileConfig>,
) -> Result<TrackerConfig, Error> {
    let Some(profile_name) = resolve_env_token(tracker.profile.clone()) else {
        return Ok(tracker);
    };
    tracker.profile = Some(profile_name.clone());
    let profile = profiles.get(&profile_name).ok_or_else(|| {
        Error::InvalidConfig(format!("tracker.profile `{profile_name}` is not defined"))
    })?;

    if is_default_tracker_kind(&tracker.kind)
        && let Some(kind) = &profile.kind
    {
        tracker.kind = kind.clone();
    }
    if is_default_tracker_endpoint(&tracker.endpoint)
        && let Some(endpoint) = &profile.endpoint
    {
        tracker.endpoint = endpoint.clone();
    }
    if tracker.api_key.is_none() {
        tracker.api_key = profile.api_key.clone();
    }
    if tracker.project_slug.is_none() {
        tracker.project_slug = profile.project_slug.clone();
    }
    if tracker.project_owner.is_none() {
        tracker.project_owner = profile.project_owner.clone();
    }
    if tracker.project_number.is_none() {
        tracker.project_number = profile.project_number;
    }
    if tracker.project_status_field.is_none() {
        tracker.project_status_field = profile.project_status_field.clone();
    }
    if tracker.repository.is_none() {
        tracker.repository = profile.repository.clone();
    }
    if (tracker.active_states.is_empty() || tracker.active_states == default_active_states())
        && !profile.active_states.is_empty()
    {
        tracker.active_states = profile.active_states.clone();
    }
    if (tracker.terminal_states.is_empty()
        || tracker.terminal_states == default_terminal_states())
        && !profile.terminal_states.is_empty()
    {
        tracker.terminal_states = profile.terminal_states.clone();
    }
    Ok(tracker)
}

fn is_default_tracker_kind(kind: &str) -> bool {
    kind.trim().is_empty() || kind == DEFAULT_TRACKER_KIND
}

fn is_default_tracker_endpoint(endpoint: &str) -> bool {
    endpoint.trim().is_empty() || endpoint == DEFAULT_LINEAR_ENDPOINT
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
        Self::from_workflow_with_configs(workflow, None, None)
    }

    fn from_workflow_with_configs(
        workflow: &WorkflowDefinition,
        user_config_path: Option<&Path>,
        repo_config_path: Option<&Path>,
    ) -> Result<Self, Error> {
        let front_matter = serde_yaml::to_string(&workflow.config)
            .map_err(|err| Error::WorkflowParse(err.to_string()))?;
        let mut builder = Config::builder()
            .set_default("tracker.kind", DEFAULT_TRACKER_KIND)
            .map_err(config_error)?
            .set_default("tracker.endpoint", DEFAULT_LINEAR_ENDPOINT)
            .map_err(config_error)?
            .set_default("tracker.active_states", default_active_states())
            .map_err(config_error)?
            .set_default("tracker.terminal_states", default_terminal_states())
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
            .map_err(config_error)?;
        if let Some(path) = user_config_path {
            let path = path.to_string_lossy().to_string();
            builder = builder.add_source(File::new(&path, FileFormat::Toml).required(false));
        }
        builder = builder.add_source(File::from_str(&front_matter, FileFormat::Yaml));
        if let Some(path) = repo_config_path {
            let path = path.to_string_lossy().to_string();
            builder = builder.add_source(File::new(&path, FileFormat::Toml).required(false));
        }
        let built = builder
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
        let tracker = apply_tracker_profile(raw.tracker, &raw.trackers.profiles)?;
        let mut config = ServiceConfig {
            tracker,
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
            if profile.stall_timeout_ms.is_none() {
                profile.stall_timeout_ms = Some(300_000);
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
        if self.tracker.kind == "github"
            && self
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
        if self.agents.profiles.is_empty() {
            if let Some(default_agent) = self.agents.default.as_deref() {
                return Err(Error::InvalidConfig(format!(
                    "agents.default `{default_agent}` is not defined"
                )));
            }
            if !self.agents.by_state.is_empty() || !self.agents.by_label.is_empty() {
                return Err(Error::InvalidConfig(
                    "agent selectors require at least one configured agent profile".into(),
                ));
            }
        } else {
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

    pub fn has_dispatch_agents(&self) -> bool {
        !self.agents.profiles.is_empty()
    }

    pub fn candidate_agents_for_issue(&self, issue: &Issue) -> Result<Vec<AgentDefinition>, Error> {
        if self.agents.profiles.is_empty() {
            return Ok(Vec::new());
        }
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
    render_turn_prompt(workflow, issue, attempt, 1, 1)
}

pub fn render_turn_prompt(
    workflow: &WorkflowDefinition,
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
) -> Result<String, Error> {
    let source = if workflow.prompt_template.trim().is_empty() {
        "You are working on an issue from Linear."
    } else {
        workflow.prompt_template.as_str()
    };
    render_turn_template(source, issue, attempt, turn_number, max_turns)
}

pub fn render_turn_template(
    source: &str,
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
) -> Result<String, Error> {
    let mut extra = Object::new();
    extra.insert("turn_number".into(), Value::scalar(turn_number));
    extra.insert("max_turns".into(), Value::scalar(max_turns));
    extra.insert("is_continuation".into(), Value::scalar(turn_number > 1));
    render_issue_template(source, issue, attempt, extra)
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
        stall_timeout_ms: profile.stall_timeout_ms.unwrap_or(300_000),
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

fn workflow_root_dir(path: &Path) -> Result<PathBuf, Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    match parent {
        Some(parent) => Ok(parent.to_path_buf()),
        None => env::current_dir().map_err(|error| {
            Error::Config(format!("resolving workflow directory failed: {error}"))
        }),
    }
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
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use {
        super::{
            ServiceConfig, WorkflowDefinition, load_workflow_with_user_config,
            render_issue_template, render_turn_template, repo_config_path,
        },
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
        assert_eq!(config.workspace.transient_paths, vec!["tmp", ".elixir_ls"]);
        assert_eq!(config.tracker.kind, "none");
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

        assert_eq!(config.tracker.kind, "github");
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
        assert_eq!(config.tracker.kind, "github");
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

        assert!(matches!(error, super::Error::InvalidConfig(_)));
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

        assert_eq!(config.tracker.kind, "github");
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

        let created = super::ensure_user_config_file(&user_config_path).unwrap();
        let contents = fs::read_to_string(&user_config_path).unwrap();
        let created_again = super::ensure_user_config_file(&user_config_path).unwrap();
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

        let created = super::ensure_workflow_file(&workflow_path).unwrap();
        let contents = fs::read_to_string(&workflow_path).unwrap();
        let created_again = super::ensure_workflow_file(&workflow_path).unwrap();
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

        let created = super::ensure_workflow_file(&workflow_path).unwrap();
        let contents = fs::read_to_string(&workflow_path).unwrap();
        let created_again = super::ensure_workflow_file(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);

        assert!(created);
        assert!(!created_again);
        assert!(contents.contains("# Polyphony Workflow"));
        assert!(contents.contains("Local tracker identity and repo wiring can live"));
    }

    #[test]
    fn ensure_repo_config_file_writes_template_once() {
        let root = unique_temp_path("repo-config-root", "d");
        fs::create_dir_all(&root).unwrap();
        let repo_config_path = root.join(".polyphony").join("config.toml");

        let created = super::ensure_repo_config_file(&repo_config_path, &root).unwrap();
        let contents = fs::read_to_string(&repo_config_path).unwrap();
        let created_again = super::ensure_repo_config_file(&repo_config_path, &root).unwrap();
        let _ = fs::remove_dir_all(&root);

        assert!(created);
        assert!(!created_again);
        assert!(contents.contains("Polyphony repo-local config."));
        assert!(contents.contains("checkout_kind = \"linked_worktree\""));
        assert!(contents.contains(&format!("source_repo_path = \"{}\"", root.display())));
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

        assert_eq!(workflow.config.tracker.kind, "github");
        assert_eq!(
            workflow.config.tracker.repository.as_deref(),
            Some("penso/polyphony")
        );
        assert_eq!(workflow.config.workspace.checkout_kind, "linked_worktree");
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
}
