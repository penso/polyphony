use crate::{prelude::*, *};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentModel {
    pub id: String,
    pub display_name: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentModelCatalog {
    pub agent_name: String,
    pub provider_kind: String,
    pub fetched_at: DateTime<Utc>,
    pub selected_model: Option<String>,
    pub models: Vec<AgentModel>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentEventKind {
    SessionStarted,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
    ToolCallStarted,
    ToolCallCompleted,
    ToolCallFailed,
    Notification,
    UsageUpdated,
    RateLimitsUpdated,
    StartupFailed,
    OtherMessage,
    Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub issue_id: String,
    pub issue_identifier: String,
    pub agent_name: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub kind: AgentEventKind,
    pub at: DateTime<Utc>,
    pub message: Option<String>,
    pub usage: Option<TokenUsage>,
    pub rate_limits: Option<Value>,
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub status: AttemptStatus,
    pub turns_completed: u32,
    pub error: Option<String>,
    pub final_issue_state: Option<String>,
}

impl AgentRunResult {
    pub fn succeeded(turns: u32) -> Self {
        Self {
            status: AttemptStatus::Succeeded,
            turns_completed: turns,
            error: None,
            final_issue_state: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: AttemptStatus::Failed,
            turns_completed: 0,
            error: Some(error.into()),
            final_issue_state: None,
        }
    }

    pub fn cancelled(error: impl Into<String>) -> Self {
        Self {
            status: AttemptStatus::CancelledByReconciliation,
            turns_completed: 0,
            error: Some(error.into()),
            final_issue_state: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentTransport {
    #[default]
    Mock,
    AppServer,
    Rpc,
    LocalCli,
    Acp,
    Acpx,
    OpenAiChat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackendKind {
    #[default]
    Host,
    Codex,
    Docker,
}

impl fmt::Display for SandboxBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Host => "host",
            Self::Codex => "codex",
            Self::Docker => "docker",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBackendKind {
    #[default]
    Provider,
    OpenAiCompatible,
    LlamaCpp,
    Ollama,
    LmStudio,
}

impl fmt::Display for RuntimeBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Provider => "provider",
            Self::OpenAiCompatible => "openai_compatible",
            Self::LlamaCpp => "llama_cpp",
            Self::Ollama => "ollama",
            Self::LmStudio => "lm_studio",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentSandboxConfig {
    pub backend: SandboxBackendKind,
    pub profile: Option<String>,
    pub policy: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeConfig {
    pub backend: RuntimeBackendKind,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub model_source: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentInteractionMode {
    #[default]
    OneShot,
    Interactive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromptMode {
    #[default]
    Env,
    Stdin,
    TmuxPaste,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDefinition {
    pub name: String,
    pub kind: String,
    pub transport: AgentTransport,
    pub command: Option<String>,
    pub fallback_agents: Vec<String>,
    pub model: Option<String>,
    pub models: Vec<String>,
    pub models_command: Option<String>,
    pub fetch_models: bool,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub sandbox: AgentSandboxConfig,
    pub runtime: AgentRuntimeConfig,
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
    pub interaction_mode: AgentInteractionMode,
    pub prompt_mode: AgentPromptMode,
    pub idle_timeout_ms: u64,
    pub completion_sentinel: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AgentRunSpec {
    pub issue: Issue,
    pub attempt: Option<u32>,
    pub workspace_path: PathBuf,
    pub prompt: String,
    pub max_turns: u32,
    pub agent: AgentDefinition,
    pub prior_context: Option<AgentContextSnapshot>,
}

#[async_trait]
pub trait AgentSession: Send {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, Error>;

    async fn stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
