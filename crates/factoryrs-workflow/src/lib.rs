use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use config::{Config, Environment, File, FileFormat};
use factoryrs_core::{CheckoutKind, Issue, TrackerQuery};
use liquid::model::{Array, Object, Value};
use liquid::{ParserBuilder, object};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value as YamlValue};
use thiserror::Error;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    pub command: String,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: i64,
    pub credits_command: Option<String>,
    pub spending_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub provider: ProviderConfig,
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
            .set_default(
                "tracker.terminal_states",
                vec![
                    "Closed",
                    "Cancelled",
                    "Canceled",
                    "Duplicate",
                    "Done",
                    "Human Review",
                ],
            )
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
            .set_default(
                "workspace.transient_paths",
                vec!["tmp".to_string(), ".elixir_ls".to_string()],
            )
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
            .set_default("provider.kind", "mock")
            .map_err(config_error)?
            .set_default("provider.command", "codex app-server")
            .map_err(config_error)?
            .set_default("provider.turn_timeout_ms", 3_600_000)
            .map_err(config_error)?
            .set_default("provider.read_timeout_ms", 5_000)
            .map_err(config_error)?
            .set_default("provider.stall_timeout_ms", 300_000)
            .map_err(config_error)?
            .set_default("server", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .add_source(File::from_str(&front_matter, FileFormat::Yaml))
            .add_source(
                Environment::with_prefix("FACTORYRS")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .map_err(config_error)?;
        let mut config = built
            .try_deserialize::<ServiceConfig>()
            .map_err(config_error)?;
        config.resolve();
        config.normalize();
        config.validate()?;
        Ok(config)
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
        if self.provider.command.trim().is_empty() {
            return Err(Error::InvalidConfig(
                "provider.command must be non-empty".into(),
            ));
        }
        if self.workspace.checkout_kind != "directory"
            && self.workspace.source_repo_path.is_none()
            && self.workspace.clone_url.is_none()
        {
            return Err(Error::InvalidConfig(
                "workspace.source_repo_path or workspace.clone_url is required for git-backed workspaces".into(),
            ));
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
    let parser = ParserBuilder::with_stdlib()
        .build()
        .map_err(|err| Error::TemplateParse(err.to_string()))?;
    let template = parser
        .parse(source)
        .map_err(|err| Error::TemplateParse(err.to_string()))?;
    let globals = object!({
        "issue": issue_to_liquid(issue),
        "attempt": attempt.unwrap_or_default(),
    });
    template
        .render(&globals)
        .map_err(|err| Error::TemplateRender(err.to_string()))
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

fn issue_author_to_liquid(author: &factoryrs_core::IssueAuthor) -> Value {
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

fn expand_path_like(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    let expanded = if let Some(name) = value.strip_prefix('$') {
        env::var(name).unwrap_or_default()
    } else {
        value.to_string()
    };
    if expanded.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(expanded.trim_start_matches("~/"));
        }
    }
    PathBuf::from(expanded)
}

fn config_error(error: config::ConfigError) -> Error {
    Error::Config(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{ServiceConfig, WorkflowDefinition};
    use serde_yaml::Value as YamlValue;

    #[test]
    fn workspace_defaults_include_reuse_and_transient_paths() {
        let workflow = WorkflowDefinition {
            config: YamlValue::Mapping(Default::default()),
            prompt_template: String::new(),
        };

        let config = ServiceConfig::from_workflow(&workflow).unwrap();

        assert!(config.workspace.sync_on_reuse);
        assert_eq!(config.workspace.transient_paths, vec!["tmp", ".elixir_ls"]);
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
}
