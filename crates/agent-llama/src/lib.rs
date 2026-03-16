use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use {
    async_trait::async_trait,
    chrono::Utc,
    polyphony_agent_common::{
        OpenAiCompatibleChatRequest, emit, fetch_budget_for_agent, merge_models, model_from_json,
        run_openai_compatible_chat, selected_model, shell_escape,
    },
    polyphony_core::{
        AgentDefinition, AgentEventKind, AgentModel, AgentModelCatalog, AgentProviderRuntime,
        AgentRunResult, AgentRunSpec, AgentTransport, BudgetSnapshot, Error as CoreError,
        RateLimitSignal, ToolExecutor,
    },
    serde_json::Value,
    tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::{Child, Command},
        sync::mpsc,
        task::JoinHandle,
        time::sleep,
    },
    tracing::{debug, info, warn},
};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8012/v1";
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub struct LlamaRuntime {
    http: reqwest::Client,
    tool_executor: Option<Arc<dyn ToolExecutor>>,
    managed_servers: Arc<Mutex<HashMap<String, ManagedLlamaServer>>>,
}

impl LlamaRuntime {
    pub fn new(tool_executor: Option<Arc<dyn ToolExecutor>>) -> Self {
        Self {
            http: reqwest::Client::new(),
            tool_executor,
            managed_servers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn ensure_server_ready(
        &self,
        agent: &AgentDefinition,
        spec: Option<&AgentRunSpec>,
        event_tx: Option<&mpsc::UnboundedSender<polyphony_core::AgentEvent>>,
    ) -> Result<(), CoreError> {
        let base_url = resolve_base_url(agent);
        if probe_server_ready(&self.http, &base_url).await? {
            return Ok(());
        }

        let spawn_config = SpawnConfig::from_agent(agent)?;
        let Some(spawn_config) = spawn_config else {
            let message = format!(
                "llama.cpp server is not reachable at `{base_url}` and agent `{}` has no command configured to start it",
                agent.name
            );
            emit_startup_failed(spec, event_tx, message.clone());
            return Err(CoreError::Adapter(message));
        };

        let mut started_new_server = false;
        {
            let mut servers = lock_servers(&self.managed_servers)?;
            let server_key = spawn_config.server_key();
            let mut remove_stale_server = false;
            if let Some(server) = servers.get_mut(&server_key) {
                if let Some(status) = server
                    .child
                    .try_wait()
                    .map_err(|error| CoreError::Adapter(error.to_string()))?
                {
                    warn!(
                        agent_name = %agent.name,
                        base_url = %spawn_config.base_url,
                        status = %status,
                        "managed llama.cpp server exited, respawning"
                    );
                    remove_stale_server = true;
                } else if server.spawn_config != spawn_config {
                    return Err(CoreError::Adapter(format!(
                        "managed llama.cpp server at `{}` is already running with a different configuration",
                        spawn_config.base_url
                    )));
                }
            }
            if remove_stale_server {
                servers.remove(&server_key);
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = servers.entry(server_key) {
                emit_server_starting(spec, event_tx, &spawn_config);
                let server = ManagedLlamaServer::spawn(&spawn_config)?;
                entry.insert(server);
                started_new_server = true;
            }
        }

        let startup_timeout = startup_timeout(agent);
        let start = Instant::now();
        loop {
            if probe_server_ready(&self.http, &base_url).await? {
                return Ok(());
            }
            if start.elapsed() >= startup_timeout {
                let status = {
                    let mut servers = lock_servers(&self.managed_servers)?;
                    match servers.get_mut(&spawn_config.server_key()) {
                        Some(server) => server
                            .child
                            .try_wait()
                            .map_err(|error| CoreError::Adapter(error.to_string()))?,
                        None => None,
                    }
                };
                let message = match status {
                    Some(status) => format!(
                        "llama.cpp server at `{base_url}` exited before becoming ready ({status})"
                    ),
                    None if started_new_server => format!(
                        "llama.cpp server at `{base_url}` did not become ready within {} ms",
                        startup_timeout.as_millis()
                    ),
                    None => format!("llama.cpp server at `{base_url}` is not healthy"),
                };
                emit_startup_failed(spec, event_tx, message.clone());
                return Err(CoreError::Adapter(message));
            }
            sleep(HEALTH_POLL_INTERVAL).await;
        }
    }
}

impl Default for LlamaRuntime {
    fn default() -> Self {
        Self::new(None)
    }
}

impl std::fmt::Debug for LlamaRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlamaRuntime")
            .field(
                "tool_executor",
                &self.tool_executor.as_ref().map(|_| "configured"),
            )
            .finish()
    }
}

impl Drop for LlamaRuntime {
    fn drop(&mut self) {
        if let Ok(mut servers) = self.managed_servers.lock() {
            servers.clear();
        }
    }
}

#[async_trait]
impl AgentProviderRuntime for LlamaRuntime {
    fn runtime_key(&self) -> String {
        "agent:llama".into()
    }

    fn supports(&self, agent: &AgentDefinition) -> bool {
        matches!(agent.transport, AgentTransport::LlamaCpp)
            || matches!(agent.kind.as_str(), "llama" | "llama.cpp" | "llama_cpp")
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        self.ensure_server_ready(&spec.agent, Some(&spec), Some(&event_tx))
            .await?;
        let catalog = discover_models_for_agent(&self.http, &spec.agent).await?;
        let base_url = resolve_base_url(&spec.agent);
        let model = spec
            .agent
            .model
            .clone()
            .or_else(|| spec.agent.models.first().cloned())
            .or_else(|| catalog.and_then(|catalog| catalog.selected_model))
            .ok_or_else(|| {
                CoreError::Adapter(format!(
                    "no model configured or discovered for agent `{}`",
                    spec.agent.name
                ))
            })?;
        run_openai_compatible_chat(
            &self.http,
            spec,
            event_tx,
            self.tool_executor.as_ref(),
            OpenAiCompatibleChatRequest {
                base_url,
                model,
                api_key: None,
                startup_message: "llama.cpp chat request started",
                rate_limit_reason: "llama_cpp_chat_429",
                provider_label: "llama.cpp",
            },
        )
        .await
    }

    async fn fetch_budget(
        &self,
        agent: &AgentDefinition,
    ) -> Result<Option<BudgetSnapshot>, CoreError> {
        fetch_budget_for_agent(agent).await
    }

    async fn discover_models(
        &self,
        agent: &AgentDefinition,
    ) -> Result<Option<AgentModelCatalog>, CoreError> {
        self.ensure_server_ready(agent, None, None).await?;
        discover_models_for_agent(&self.http, agent).await
    }
}

#[derive(Debug)]
struct ManagedLlamaServer {
    spawn_config: SpawnConfig,
    child: Child,
    _stdout_task: JoinHandle<()>,
    _stderr_task: JoinHandle<()>,
}

impl ManagedLlamaServer {
    fn spawn(spawn_config: &SpawnConfig) -> Result<Self, CoreError> {
        let mut child = build_server_process(spawn_config)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| CoreError::Adapter(error.to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Adapter("llama.cpp server stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CoreError::Adapter("llama.cpp server stderr unavailable".into()))?;
        let stdout_task = tokio::spawn(forward_server_output(
            BufReader::new(stdout),
            "stdout",
            spawn_config.base_url.clone(),
        ));
        let stderr_task = tokio::spawn(forward_server_output(
            BufReader::new(stderr),
            "stderr",
            spawn_config.base_url.clone(),
        ));
        Ok(Self {
            spawn_config: spawn_config.clone(),
            child,
            _stdout_task: stdout_task,
            _stderr_task: stderr_task,
        })
    }
}

impl Drop for ManagedLlamaServer {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            best_effort_term(pid);
        }
        let _ = self.child.start_kill();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpawnConfig {
    command: String,
    base_url: String,
    host: String,
    port: u16,
    model_path: PathBuf,
    gpu_layers: Option<i64>,
    context_size: Option<u32>,
}

impl SpawnConfig {
    fn from_agent(agent: &AgentDefinition) -> Result<Option<Self>, CoreError> {
        let Some(command) = agent
            .command
            .clone()
            .and_then(|command| normalize_command(&command))
        else {
            return Ok(None);
        };
        let base_url = resolve_base_url(agent);
        let url = reqwest::Url::parse(&base_url)
            .map_err(|error| CoreError::Adapter(format!("invalid llama.cpp base_url: {error}")))?;
        if url.scheme() != "http" {
            return Err(CoreError::Adapter(format!(
                "llama.cpp auto-spawn requires an http base_url, got `{}`",
                url.scheme()
            )));
        }
        let host = url.host_str().ok_or_else(|| {
            CoreError::Adapter(format!("llama.cpp base_url `{base_url}` is missing a host"))
        })?;
        let port = url.port_or_known_default().ok_or_else(|| {
            CoreError::Adapter(format!("llama.cpp base_url `{base_url}` is missing a port"))
        })?;
        let model_path = resolve_spawn_model_path(agent)?;
        Ok(Some(Self {
            command,
            base_url,
            host: host.to_string(),
            port,
            model_path,
            gpu_layers: agent.gpu_layers,
            context_size: agent.context_size,
        }))
    }

    fn server_key(&self) -> String {
        self.base_url.trim_end_matches('/').to_string()
    }

    fn command_line(&self) -> String {
        let mut command = self.command.clone();
        command.push_str(" --host ");
        command.push_str(&shell_escape(&self.host));
        command.push_str(" --port ");
        command.push_str(&self.port.to_string());
        command.push_str(" --model ");
        command.push_str(&shell_escape(&self.model_path.to_string_lossy()));
        if let Some(gpu_layers) = self.gpu_layers {
            command.push_str(" --n-gpu-layers ");
            command.push_str(&gpu_layers.to_string());
        }
        if let Some(context_size) = self.context_size {
            command.push_str(" --ctx-size ");
            command.push_str(&context_size.to_string());
        }
        command
    }
}

fn lock_servers(
    servers: &Arc<Mutex<HashMap<String, ManagedLlamaServer>>>,
) -> Result<std::sync::MutexGuard<'_, HashMap<String, ManagedLlamaServer>>, CoreError> {
    servers
        .lock()
        .map_err(|_| CoreError::Adapter("llama.cpp runtime state lock poisoned".into()))
}

fn resolve_base_url(agent: &AgentDefinition) -> String {
    resolve_base_url_from_option(agent.base_url.clone())
}

fn resolve_base_url_from_option(base_url: Option<String>) -> String {
    base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into())
}

fn normalize_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn startup_timeout(agent: &AgentDefinition) -> Duration {
    let max_timeout = agent.turn_timeout_ms.max(agent.read_timeout_ms.max(5_000));
    Duration::from_millis(max_timeout.min(60_000))
}

fn resolve_spawn_model_path(agent: &AgentDefinition) -> Result<PathBuf, CoreError> {
    let model = agent.model.as_deref().ok_or_else(|| {
        CoreError::Adapter(format!(
            "agent `{}` model is required to auto-start llama.cpp",
            agent.name
        ))
    })?;
    let expanded = expand_home(model);
    let model_path = PathBuf::from(&expanded);
    if model_path.is_absolute() || expanded.contains(std::path::MAIN_SEPARATOR) {
        return Ok(model_path);
    }
    if model_path.exists() {
        return Ok(model_path);
    }

    let models_dir = resolve_models_dir(&agent.env).ok_or_else(|| {
        CoreError::Adapter(format!(
            "agent `{}` model `{model}` is not a path and no llama.cpp models directory is configured",
            agent.name
        ))
    })?;
    Ok(models_dir.join(model))
}

fn resolve_models_dir(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    env.get("LLAMA_MODELS_DIR")
        .or_else(|| env.get("POLYPHONY_LLAMA_MODELS_DIR"))
        .cloned()
        .or_else(|| std::env::var("LLAMA_MODELS_DIR").ok())
        .or_else(|| std::env::var("POLYPHONY_LLAMA_MODELS_DIR").ok())
        .map(|value| PathBuf::from(expand_home(&value)))
        .or_else(|| dirs::cache_dir().map(|dir| dir.join("llama.cpp")))
}

fn expand_home(value: &str) -> String {
    if value == "~" {
        return dirs::home_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| value.to_string());
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().to_string();
    }
    value.to_string()
}

fn build_server_process(spawn_config: &SpawnConfig) -> Command {
    let mut command = Command::new("bash");
    command.arg("-lc").arg(spawn_config.command_line());
    command.env_remove("CLAUDECODE");
    command
}

async fn forward_server_output<R>(reader: BufReader<R>, stream_name: &'static str, base_url: String)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        debug!(
            base_url = %base_url,
            stream = stream_name,
            message = %line,
            "llama.cpp server output"
        );
    }
}

async fn probe_server_ready(client: &reqwest::Client, base_url: &str) -> Result<bool, CoreError> {
    let health_url = format!(
        "{}/health",
        base_url
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/')
    );
    match client
        .get(&health_url)
        .header("User-Agent", "polyphony")
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                return Ok(true);
            }
            if status.as_u16() == 503 || status.as_u16() == 404 || status.as_u16() == 405 {
                return probe_models_endpoint(client, base_url).await;
            }
            Ok(false)
        },
        Err(error) if error.is_connect() || error.is_timeout() => Ok(false),
        Err(error) => Err(CoreError::Adapter(error.to_string())),
    }
}

async fn probe_models_endpoint(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<bool, CoreError> {
    match client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .header("User-Agent", "polyphony")
        .send()
        .await
    {
        Ok(response) => Ok(response.status().is_success()),
        Err(error) if error.is_connect() || error.is_timeout() => Ok(false),
        Err(error) => Err(CoreError::Adapter(error.to_string())),
    }
}

async fn discover_models_for_agent(
    client: &reqwest::Client,
    agent: &AgentDefinition,
) -> Result<Option<AgentModelCatalog>, CoreError> {
    let configured = agent
        .models
        .iter()
        .cloned()
        .map(|id| AgentModel {
            id,
            display_name: None,
            created_at: None,
        })
        .collect::<Vec<_>>();
    let discovered = if agent.fetch_models {
        discover_http_models(client, agent).await?
    } else {
        Vec::new()
    };
    let merged = merge_models(configured, discovered);
    if merged.is_empty() && agent.model.is_none() {
        return Ok(None);
    }
    Ok(Some(AgentModelCatalog {
        agent_name: agent.name.clone(),
        provider_kind: agent.kind.clone(),
        fetched_at: Utc::now(),
        selected_model: selected_model(agent, &merged),
        models: merged,
    }))
}

async fn discover_http_models(
    client: &reqwest::Client,
    agent: &AgentDefinition,
) -> Result<Vec<AgentModel>, CoreError> {
    let base_url = resolve_base_url(agent);
    info!(
        agent_name = %agent.name,
        provider_kind = %agent.kind,
        base_url,
        "discovering llama.cpp models"
    );
    let response = client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .header("User-Agent", "polyphony")
        .send()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let status = response.status();
    if status.as_u16() == 429 {
        return Err(CoreError::RateLimited(Box::new(RateLimitSignal {
            component: format!("agent:{}", agent.name),
            reason: "llama_cpp_models_429".into(),
            limited_at: Utc::now(),
            retry_after_ms: None,
            reset_at: None,
            status_code: Some(429),
            raw: None,
        })));
    }
    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !status.is_success() {
        return Err(CoreError::Adapter(format!(
            "model discovery failed for {}: {status} {payload}",
            agent.name
        )));
    }
    Ok(payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(model_from_json)
        .collect())
}

fn emit_server_starting(
    spec: Option<&AgentRunSpec>,
    event_tx: Option<&mpsc::UnboundedSender<polyphony_core::AgentEvent>>,
    spawn_config: &SpawnConfig,
) {
    if let (Some(spec), Some(event_tx)) = (spec, event_tx) {
        emit(
            event_tx,
            spec,
            AgentEventKind::Notification,
            Some(format!(
                "starting llama.cpp server with {}",
                display_command(&spawn_config.command)
            )),
            None,
            None,
            None,
            None,
        );
    }
}

fn emit_startup_failed(
    spec: Option<&AgentRunSpec>,
    event_tx: Option<&mpsc::UnboundedSender<polyphony_core::AgentEvent>>,
    message: String,
) {
    if let (Some(spec), Some(event_tx)) = (spec, event_tx) {
        emit(
            event_tx,
            spec,
            AgentEventKind::StartupFailed,
            Some(message),
            None,
            None,
            None,
            None,
        );
    }
}

fn display_command(command: &str) -> String {
    Path::new(command)
        .file_name()
        .and_then(OsStr::to_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| command.to_string())
}

fn best_effort_term(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use {
        super::{LlamaRuntime, SpawnConfig, expand_home},
        polyphony_core::{
            AgentDefinition, AgentProviderRuntime, AgentRunSpec, AgentTransport, AttemptStatus,
            Issue,
        },
        tempfile::TempDir,
        tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            sync::mpsc,
        },
    };

    fn llama_agent(base_url: String) -> AgentDefinition {
        AgentDefinition {
            name: "llama".into(),
            kind: "llama".into(),
            transport: AgentTransport::LlamaCpp,
            base_url: Some(base_url),
            model: Some("tiny.gguf".into()),
            fetch_models: false,
            turn_timeout_ms: 5_000,
            read_timeout_ms: 1_000,
            stall_timeout_ms: 60_000,
            idle_timeout_ms: 1_000,
            ..AgentDefinition::default()
        }
    }

    #[test]
    fn supports_llama_transport() {
        let runtime = LlamaRuntime::default();
        assert!(runtime.supports(&AgentDefinition {
            transport: AgentTransport::LlamaCpp,
            ..AgentDefinition::default()
        }));
        assert!(!runtime.supports(&AgentDefinition {
            transport: AgentTransport::OpenAiChat,
            ..AgentDefinition::default()
        }));
    }

    #[test]
    fn spawn_config_builds_expected_command_line() {
        let temp_dir = TempDir::new().unwrap();
        let model_path = temp_dir.path().join("tiny.gguf");
        fs::write(&model_path, b"gguf").unwrap();

        let agent = AgentDefinition {
            name: "llama".into(),
            kind: "llama".into(),
            transport: AgentTransport::LlamaCpp,
            command: Some("llama-server".into()),
            base_url: Some("http://127.0.0.1:8123/v1".into()),
            model: Some(model_path.to_string_lossy().to_string()),
            gpu_layers: Some(32),
            context_size: Some(16384),
            ..AgentDefinition::default()
        };

        let spawn_config = SpawnConfig::from_agent(&agent).unwrap().unwrap();
        let command_line = spawn_config.command_line();
        assert!(command_line.contains("llama-server"));
        assert!(command_line.contains("--host '127.0.0.1'"));
        assert!(command_line.contains("--port 8123"));
        assert!(command_line.contains("--n-gpu-layers 32"));
        assert!(command_line.contains("--ctx-size 16384"));
        assert!(command_line.contains("--model"));
    }

    #[tokio::test]
    async fn discovers_models_from_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(mock_models_server(listener, false));

        let runtime = LlamaRuntime::default();
        let catalog = runtime
            .discover_models(&AgentDefinition {
                fetch_models: true,
                ..llama_agent(format!("http://{addr}/v1"))
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].id, "tiny.gguf");
    }

    #[tokio::test]
    async fn run_uses_openai_compatible_chat_without_api_key() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let model_requests = Arc::new(AtomicUsize::new(0));
        let chat_requests = Arc::new(AtomicUsize::new(0));
        tokio::spawn(mock_chat_server(
            listener,
            Arc::clone(&model_requests),
            Arc::clone(&chat_requests),
        ));

        let runtime = LlamaRuntime::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: Issue {
                        id: "1".into(),
                        identifier: "TEST-1".into(),
                        title: "Test".into(),
                        state: "Todo".into(),
                        ..Issue::default()
                    },
                    attempt: None,
                    workspace_path: std::env::temp_dir(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        model: None,
                        fetch_models: true,
                        ..llama_agent(format!("http://{addr}/v1"))
                    },
                },
                tx,
            )
            .await
            .unwrap();

        assert!(matches!(result.status, AttemptStatus::Succeeded));
        assert_eq!(model_requests.load(Ordering::SeqCst), 1);
        assert_eq!(chat_requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn expands_home_prefix() {
        assert!(expand_home("~/model.gguf").contains("model.gguf"));
    }

    async fn mock_models_server(listener: TcpListener, health_first: bool) {
        let mut requests = 0usize;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let bytes = socket.read(&mut request).await.unwrap();
            let body = String::from_utf8_lossy(&request[..bytes]);
            requests += 1;

            let response = if body.starts_with("GET /health ") {
                if health_first && requests == 1 {
                    "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                }
            } else if body.starts_with("GET /v1/models ") {
                let payload = serde_json::json!({
                    "data": [{
                        "id": "tiny.gguf",
                        "created": 1_700_000_000_i64,
                        "owned_by": "llama.cpp"
                    }]
                })
                .to_string();
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                )
            } else {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_string()
            };
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    }

    async fn mock_chat_server(
        listener: TcpListener,
        model_requests: Arc<AtomicUsize>,
        chat_requests: Arc<AtomicUsize>,
    ) {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let bytes = socket.read(&mut request).await.unwrap();
            let body = String::from_utf8_lossy(&request[..bytes]);
            let response = if body.starts_with("GET /health ") {
                "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
            } else if body.starts_with("GET /v1/models ") {
                model_requests.fetch_add(1, Ordering::SeqCst);
                let payload = serde_json::json!({
                    "data": [{
                        "id": "tiny.gguf",
                        "created": 1_700_000_000_i64,
                        "owned_by": "llama.cpp"
                    }]
                })
                .to_string();
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                )
            } else if body.starts_with("POST /v1/chat/completions ") {
                chat_requests.fetch_add(1, Ordering::SeqCst);
                if body.contains("\"model\":\"tiny.gguf\"") {
                    let payload = serde_json::json!({
                        "choices": [{
                            "message": {
                                "content": "done"
                            }
                        }],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                    })
                    .to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    )
                } else {
                    let payload = serde_json::json!({
                        "error": "expected discovered model in chat request"
                    })
                    .to_string();
                    format!(
                        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    )
                }
            } else {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_string()
            };
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    }
}
