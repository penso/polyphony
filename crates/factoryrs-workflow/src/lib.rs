use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use factoryrs_core::Issue;
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
    pub max_concurrent_agents_by_state: std::collections::HashMap<String, usize>,
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
        let root = workflow
            .config
            .as_mapping()
            .ok_or(Error::FrontMatterNotMap)?;
        let tracker = parse_tracker(root)?;
        let polling = parse_polling(root)?;
        let workspace = parse_workspace(root)?;
        let hooks = parse_hooks(root)?;
        let agent = parse_agent(root)?;
        let provider = parse_provider(root)?;
        let server = parse_server(root)?;
        let config = Self {
            tracker,
            polling,
            workspace,
            hooks,
            agent,
            provider,
            server,
        };
        config.validate()?;
        Ok(config)
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

fn parse_tracker(root: &Mapping) -> Result<TrackerConfig, Error> {
    let section = mapping(root, "tracker");
    let kind = string_field(section, "kind").unwrap_or_else(|| "mock".into());
    let endpoint = string_field(section, "endpoint")
        .unwrap_or_else(|| "https://api.linear.app/graphql".into());
    let api_key = string_field(section, "api_key").and_then(resolve_env_token);
    let project_slug = string_field(section, "project_slug");
    Ok(TrackerConfig {
        kind,
        endpoint,
        api_key,
        project_slug,
        repository: string_field(section, "repository"),
        active_states: string_list(section, "active_states", &["Todo", "In Progress"]),
        terminal_states: string_list(
            section,
            "terminal_states",
            &[
                "Closed",
                "Cancelled",
                "Canceled",
                "Duplicate",
                "Done",
                "Human Review",
            ],
        ),
    })
}

fn parse_polling(root: &Mapping) -> Result<PollingConfig, Error> {
    let section = mapping(root, "polling");
    Ok(PollingConfig {
        interval_ms: u64_field(section, "interval_ms").unwrap_or(30_000),
    })
}

fn parse_workspace(root: &Mapping) -> Result<WorkspaceConfig, Error> {
    let section = mapping(root, "workspace");
    let default_root = env::temp_dir().join("symphony_workspaces");
    let root = string_field(section, "root")
        .map(|value| expand_path_like(&value))
        .unwrap_or(default_root);
    Ok(WorkspaceConfig {
        root,
        checkout_kind: string_field(section, "checkout_kind").unwrap_or_else(|| "directory".into()),
        source_repo_path: string_field(section, "source_repo_path")
            .map(|value| expand_path_like(&value)),
        clone_url: string_field(section, "clone_url"),
        default_branch: string_field(section, "default_branch"),
    })
}

fn parse_hooks(root: &Mapping) -> Result<HooksConfig, Error> {
    let section = mapping(root, "hooks");
    Ok(HooksConfig {
        after_create: string_field(section, "after_create"),
        before_run: string_field(section, "before_run"),
        after_run: string_field(section, "after_run"),
        before_remove: string_field(section, "before_remove"),
        timeout_ms: u64_field(section, "timeout_ms")
            .filter(|value| *value > 0)
            .unwrap_or(60_000),
    })
}

fn parse_agent(root: &Mapping) -> Result<AgentConfig, Error> {
    let section = mapping(root, "agent");
    let raw_map = section.and_then(|mapping| {
        mapping
            .get(YamlValue::String(
                "max_concurrent_agents_by_state".to_string(),
            ))
            .and_then(YamlValue::as_mapping)
    });
    let mut by_state = std::collections::HashMap::new();
    if let Some(raw_map) = raw_map {
        for (key, value) in raw_map {
            let state = key.as_str().unwrap_or_default().to_ascii_lowercase();
            let Some(limit) = yaml_to_u64(value).filter(|value| *value > 0) else {
                continue;
            };
            by_state.insert(state, limit as usize);
        }
    }
    Ok(AgentConfig {
        max_concurrent_agents: u64_field(section, "max_concurrent_agents").unwrap_or(10) as usize,
        max_concurrent_agents_by_state: by_state,
        max_retry_backoff_ms: u64_field(section, "max_retry_backoff_ms").unwrap_or(300_000),
        max_turns: u64_field(section, "max_turns").unwrap_or(20) as u32,
    })
}

fn parse_provider(root: &Mapping) -> Result<ProviderConfig, Error> {
    let section = mapping(root, "provider").or_else(|| mapping(root, "codex"));
    Ok(ProviderConfig {
        kind: string_field(section, "kind").unwrap_or_else(|| "mock".into()),
        command: string_field(section, "command").unwrap_or_else(|| "codex app-server".into()),
        approval_policy: string_field(section, "approval_policy"),
        thread_sandbox: string_field(section, "thread_sandbox"),
        turn_sandbox_policy: string_field(section, "turn_sandbox_policy"),
        turn_timeout_ms: u64_field(section, "turn_timeout_ms").unwrap_or(3_600_000),
        read_timeout_ms: u64_field(section, "read_timeout_ms").unwrap_or(5_000),
        stall_timeout_ms: i64_field(section, "stall_timeout_ms").unwrap_or(300_000),
        credits_command: string_field(section, "credits_command"),
        spending_command: string_field(section, "spending_command"),
    })
}

fn parse_server(root: &Mapping) -> Result<ServerConfig, Error> {
    let section = mapping(root, "server");
    Ok(ServerConfig {
        port: u64_field(section, "port").map(|value| value as u16),
    })
}

fn mapping<'a>(root: &'a Mapping, key: &str) -> Option<&'a Mapping> {
    root.get(YamlValue::String(key.to_string()))
        .and_then(YamlValue::as_mapping)
}

fn string_field(section: Option<&Mapping>, key: &str) -> Option<String> {
    section
        .and_then(|mapping| mapping.get(YamlValue::String(key.to_string())))
        .and_then(|value| match value {
            YamlValue::String(text) => Some(text.clone()),
            YamlValue::Number(number) => Some(number.to_string()),
            _ => None,
        })
}

fn string_list(section: Option<&Mapping>, key: &str, default: &[&str]) -> Vec<String> {
    let values = section
        .and_then(|mapping| mapping.get(YamlValue::String(key.to_string())))
        .and_then(YamlValue::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(YamlValue::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if values.is_empty() {
        default.iter().map(|value| value.to_string()).collect()
    } else {
        values
    }
}

fn u64_field(section: Option<&Mapping>, key: &str) -> Option<u64> {
    section
        .and_then(|mapping| mapping.get(YamlValue::String(key.to_string())))
        .and_then(yaml_to_u64)
}

fn i64_field(section: Option<&Mapping>, key: &str) -> Option<i64> {
    section
        .and_then(|mapping| mapping.get(YamlValue::String(key.to_string())))
        .and_then(yaml_to_i64)
}

fn yaml_to_u64(value: &YamlValue) -> Option<u64> {
    match value {
        YamlValue::Number(number) => number.as_u64(),
        YamlValue::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn yaml_to_i64(value: &YamlValue) -> Option<i64> {
    match value {
        YamlValue::Number(number) => number.as_i64(),
        YamlValue::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn resolve_env_token(value: String) -> Option<String> {
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

fn expand_path_like(value: &str) -> PathBuf {
    let mut expanded = value.to_string();
    if let Some(name) = expanded.strip_prefix('$') {
        expanded = env::var(name).unwrap_or_default();
    }
    if expanded.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(expanded.trim_start_matches("~/"));
        }
    }
    PathBuf::from(expanded)
}
