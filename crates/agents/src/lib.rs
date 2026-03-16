mod docker_sandbox;

use std::sync::Arc;

use {
    crate::docker_sandbox::rewrite_spec_for_docker,
    async_trait::async_trait,
    chrono::Utc,
    polyphony_agent_common::{
        merge_models, model_from_json, parse_model_list, run_shell_capture, selected_model,
    },
    polyphony_core::{
        AgentDefinition, AgentModel, AgentModelCatalog, AgentProviderRuntime, AgentRunResult,
        AgentRunSpec, AgentRuntime, AgentSession, BudgetSnapshot, Error as CoreError,
        RuntimeBackend, RuntimeBackendKind, SandboxBackend, SandboxBackendKind, ToolExecutor,
    },
    serde::{Deserialize, de::DeserializeOwned},
    serde_json::Value,
    tokio::sync::mpsc,
};

pub use crate::docker_sandbox::run_docker_sandbox_manifest;

const OLLAMA_DEFAULT_BASE_URL: &str = "http://localhost:11434";
const LM_STUDIO_DEFAULT_BASE_URL: &str = "http://localhost:1234";

#[derive(Default)]
pub struct AgentRegistryRuntime {
    providers: Vec<Arc<dyn AgentProviderRuntime>>,
    sandbox_backends: Vec<Arc<dyn SandboxBackend>>,
    runtime_backends: Vec<Arc<dyn RuntimeBackend>>,
}

#[derive(Default)]
struct HostSandboxBackend;

#[derive(Default)]
struct CodexSandboxBackend;

#[derive(Default)]
struct DockerSandboxBackend;

#[derive(Default)]
struct ProviderRuntimeBackend;

#[derive(Clone, Copy)]
struct OpenAiStyleRuntimeBackend {
    kind: RuntimeBackendKind,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelEntry {
    name: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<Value>,
}

fn default_runtime_base_url(kind: RuntimeBackendKind) -> Option<&'static str> {
    match kind {
        RuntimeBackendKind::Ollama => Some(OLLAMA_DEFAULT_BASE_URL),
        RuntimeBackendKind::LmStudio => Some(LM_STUDIO_DEFAULT_BASE_URL),
        _ => None,
    }
}

fn apply_runtime_overrides(mut spec: AgentRunSpec, default_base_url: Option<&str>) -> AgentRunSpec {
    if spec.agent.base_url.is_none() {
        spec.agent.base_url = spec
            .agent
            .runtime
            .endpoint
            .clone()
            .or_else(|| default_base_url.map(ToOwned::to_owned));
    }
    if spec.agent.model.is_none() {
        spec.agent.model = spec.agent.runtime.model.clone();
    }
    spec.agent.env.extend(spec.agent.runtime.env.clone());
    spec
}

fn configured_models(agent: &AgentDefinition) -> Vec<AgentModel> {
    agent
        .models
        .iter()
        .cloned()
        .map(|id| AgentModel {
            id,
            display_name: None,
            created_at: None,
        })
        .collect()
}

fn resolve_runtime_base_url(agent: &AgentDefinition, kind: RuntimeBackendKind) -> Option<String> {
    agent
        .base_url
        .clone()
        .or_else(|| agent.runtime.endpoint.clone())
        .or_else(|| default_runtime_base_url(kind).map(ToOwned::to_owned))
}

fn resolve_discovery_url(agent: &AgentDefinition, kind: RuntimeBackendKind) -> Option<String> {
    let base_url = resolve_runtime_base_url(agent, kind)?;
    let normalized = base_url.trim_end_matches('/');
    match kind {
        RuntimeBackendKind::Ollama => Some(format!(
            "{}/api/tags",
            normalized.strip_suffix("/v1").unwrap_or(normalized)
        )),
        RuntimeBackendKind::LmStudio => {
            if normalized.ends_with("/v1") {
                Some(format!("{normalized}/models"))
            } else {
                Some(format!("{normalized}/v1/models"))
            }
        },
        _ => None,
    }
}

async fn fetch_json<T: DeserializeOwned>(agent_name: &str, url: &str) -> Result<T, CoreError> {
    let response = reqwest::Client::new()
        .get(url)
        .header("User-Agent", "polyphony")
        .send()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !status.is_success() {
        return Err(CoreError::Adapter(format!(
            "model discovery failed for {agent_name}: {status} {}",
            String::from_utf8_lossy(&body)
        )));
    }
    serde_json::from_slice(&body).map_err(|error| {
        CoreError::Adapter(format!(
            "invalid model discovery response for {agent_name} from {url}: {error}"
        ))
    })
}

async fn discover_ollama_models(agent: &AgentDefinition) -> Result<Vec<AgentModel>, CoreError> {
    let url = resolve_discovery_url(agent, RuntimeBackendKind::Ollama).ok_or_else(|| {
        CoreError::Adapter(format!(
            "agent `{}` does not have a resolvable Ollama base URL",
            agent.name
        ))
    })?;
    let payload = fetch_json::<OllamaTagsResponse>(&agent.name, &url).await?;
    Ok(payload
        .models
        .into_iter()
        .map(|model| AgentModel {
            id: model.name,
            display_name: None,
            created_at: None,
        })
        .collect())
}

async fn discover_lm_studio_models(agent: &AgentDefinition) -> Result<Vec<AgentModel>, CoreError> {
    let url = resolve_discovery_url(agent, RuntimeBackendKind::LmStudio).ok_or_else(|| {
        CoreError::Adapter(format!(
            "agent `{}` does not have a resolvable LM Studio base URL",
            agent.name
        ))
    })?;
    let payload = fetch_json::<OpenAiModelsResponse>(&agent.name, &url).await?;
    Ok(payload
        .data
        .into_iter()
        .filter_map(|model| model_from_json(&model))
        .collect())
}

async fn discover_openai_style_runtime_models(
    kind: RuntimeBackendKind,
    agent: &AgentDefinition,
) -> Result<Option<AgentModelCatalog>, CoreError> {
    let configured = configured_models(agent);
    let discovered = if let Some(command) = &agent.models_command {
        parse_model_list(&run_shell_capture(command, None, &agent.env).await?)?
    } else if agent.fetch_models {
        match kind {
            RuntimeBackendKind::Ollama => discover_ollama_models(agent).await?,
            RuntimeBackendKind::LmStudio => discover_lm_studio_models(agent).await?,
            _ => Vec::new(),
        }
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

impl AgentRegistryRuntime {
    #[allow(clippy::vec_init_then_push)]
    pub fn new() -> Self {
        Self::new_with_tools(None)
    }

    #[allow(clippy::vec_init_then_push)]
    pub fn new_with_tools(tool_executor: Option<Arc<dyn ToolExecutor>>) -> Self {
        #[allow(unused_mut)]
        let mut providers = Vec::new();
        #[cfg(feature = "acp")]
        providers.push(Arc::new(polyphony_agent_acp::AcpRuntime) as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "acpx")]
        providers
            .push(Arc::new(polyphony_agent_acpx::AcpxRuntime) as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "pi")]
        providers.push(Arc::new(polyphony_agent_pi::PiRuntime) as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "codex")]
        providers.push(Arc::new(polyphony_agent_codex::CodexRuntime::new(
            tool_executor.clone(),
        )) as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "claude")]
        providers.push(Arc::new(polyphony_agent_claude::ClaudeRuntime::default())
            as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "copilot")]
        providers.push(Arc::new(polyphony_agent_copilot::CopilotRuntime::default())
            as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "openai")]
        providers.push(Arc::new(polyphony_agent_openai::OpenAiRuntime::new(
            tool_executor.clone(),
        )) as Arc<dyn AgentProviderRuntime>);
        #[cfg(feature = "local")]
        providers.push(
            Arc::new(polyphony_agent_local::LocalCliRuntime::fallback_transport())
                as Arc<dyn AgentProviderRuntime>,
        );
        let sandbox_backends: Vec<Arc<dyn SandboxBackend>> = vec![
            Arc::new(HostSandboxBackend),
            Arc::new(CodexSandboxBackend),
            Arc::new(DockerSandboxBackend),
        ];
        let runtime_backends: Vec<Arc<dyn RuntimeBackend>> = vec![
            Arc::new(ProviderRuntimeBackend),
            Arc::new(OpenAiStyleRuntimeBackend {
                kind: RuntimeBackendKind::OpenAiCompatible,
            }),
            Arc::new(OpenAiStyleRuntimeBackend {
                kind: RuntimeBackendKind::Ollama,
            }),
            Arc::new(OpenAiStyleRuntimeBackend {
                kind: RuntimeBackendKind::LmStudio,
            }),
        ];
        Self::from_components(providers, sandbox_backends, runtime_backends)
    }

    pub fn from_components(
        providers: Vec<Arc<dyn AgentProviderRuntime>>,
        sandbox_backends: Vec<Arc<dyn SandboxBackend>>,
        runtime_backends: Vec<Arc<dyn RuntimeBackend>>,
    ) -> Self {
        Self {
            providers,
            sandbox_backends,
            runtime_backends,
        }
    }

    fn provider_for(
        &self,
        agent: &AgentDefinition,
    ) -> Result<&Arc<dyn AgentProviderRuntime>, CoreError> {
        self.providers
            .iter()
            .find(|provider| provider.supports(agent))
            .ok_or_else(|| {
                CoreError::Adapter(format!(
                    "no provider runtime registered for agent `{}` ({})",
                    agent.name, agent.kind
                ))
            })
    }

    fn sandbox_backend_for(
        &self,
        agent: &AgentDefinition,
    ) -> Result<&Arc<dyn SandboxBackend>, CoreError> {
        self.sandbox_backends
            .iter()
            .find(|backend| backend.supports(agent))
            .ok_or_else(|| {
                CoreError::Adapter(format!(
                    "no sandbox backend registered for agent `{}` ({})",
                    agent.name, agent.sandbox.backend
                ))
            })
    }

    fn runtime_backend_for(
        &self,
        agent: &AgentDefinition,
    ) -> Result<&Arc<dyn RuntimeBackend>, CoreError> {
        self.runtime_backends
            .iter()
            .find(|backend| backend.supports(agent))
            .ok_or_else(|| {
                CoreError::Adapter(format!(
                    "no runtime backend registered for agent `{}` ({})",
                    agent.name, agent.runtime.backend
                ))
            })
    }

    async fn prepare_spec(&self, spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        let spec = self
            .runtime_backend_for(&spec.agent)?
            .prepare_run(spec)
            .await?;
        self.sandbox_backend_for(&spec.agent)?
            .prepare_run(spec)
            .await
    }
}

#[async_trait]
impl SandboxBackend for HostSandboxBackend {
    fn backend_key(&self) -> String {
        "sandbox:host".into()
    }

    fn kind(&self) -> SandboxBackendKind {
        SandboxBackendKind::Host
    }

    async fn prepare_run(&self, mut spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        spec.agent.env.extend(spec.agent.sandbox.env.clone());
        Ok(spec)
    }
}

#[async_trait]
impl SandboxBackend for CodexSandboxBackend {
    fn backend_key(&self) -> String {
        "sandbox:codex".into()
    }

    fn kind(&self) -> SandboxBackendKind {
        SandboxBackendKind::Codex
    }

    async fn prepare_run(&self, mut spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        if spec.agent.thread_sandbox.is_none() {
            spec.agent.thread_sandbox = spec.agent.sandbox.profile.clone();
        }
        if spec.agent.turn_sandbox_policy.is_none() {
            spec.agent.turn_sandbox_policy = spec.agent.sandbox.policy.clone();
        }
        spec.agent.env.extend(spec.agent.sandbox.env.clone());
        Ok(spec)
    }
}

#[async_trait]
impl SandboxBackend for DockerSandboxBackend {
    fn backend_key(&self) -> String {
        "sandbox:docker".into()
    }

    fn kind(&self) -> SandboxBackendKind {
        SandboxBackendKind::Docker
    }

    async fn prepare_run(&self, spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        rewrite_spec_for_docker(spec).await
    }
}

#[async_trait]
impl RuntimeBackend for ProviderRuntimeBackend {
    fn backend_key(&self) -> String {
        "runtime:provider".into()
    }

    fn kind(&self) -> RuntimeBackendKind {
        RuntimeBackendKind::Provider
    }

    async fn prepare_run(&self, spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        Ok(apply_runtime_overrides(spec, None))
    }
}

#[async_trait]
impl RuntimeBackend for OpenAiStyleRuntimeBackend {
    fn backend_key(&self) -> String {
        format!("runtime:{}", self.kind)
    }

    fn kind(&self) -> RuntimeBackendKind {
        self.kind
    }

    async fn prepare_run(&self, spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        Ok(apply_runtime_overrides(
            spec,
            default_runtime_base_url(self.kind),
        ))
    }

    async fn discover_models(
        &self,
        agent: &AgentDefinition,
    ) -> Result<Option<AgentModelCatalog>, CoreError> {
        match self.kind {
            RuntimeBackendKind::Ollama | RuntimeBackendKind::LmStudio => {
                discover_openai_style_runtime_models(self.kind, agent).await
            },
            _ => Ok(None),
        }
    }
}

#[async_trait]
impl AgentRuntime for AgentRegistryRuntime {
    fn component_key(&self) -> String {
        "agent:registry".into()
    }

    async fn start_session(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, CoreError> {
        let spec = self.prepare_spec(spec).await?;
        self.provider_for(&spec.agent)?
            .start_session(spec, event_tx)
            .await
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        let spec = self.prepare_spec(spec).await?;
        self.provider_for(&spec.agent)?.run(spec, event_tx).await
    }

    async fn fetch_budgets(
        &self,
        agents: &[AgentDefinition],
    ) -> Result<Vec<BudgetSnapshot>, CoreError> {
        let mut snapshots = Vec::new();
        for agent in agents {
            if let Some(snapshot) = self.provider_for(agent)?.fetch_budget(agent).await? {
                snapshots.push(snapshot);
            }
        }
        Ok(snapshots)
    }

    async fn discover_models(
        &self,
        agents: &[AgentDefinition],
    ) -> Result<Vec<AgentModelCatalog>, CoreError> {
        let mut catalogs = Vec::new();
        for agent in agents {
            if let Some(catalog) = self
                .runtime_backend_for(agent)?
                .discover_models(agent)
                .await?
            {
                catalogs.push(catalog);
                continue;
            }
            if let Some(catalog) = self.provider_for(agent)?.discover_models(agent).await? {
                catalogs.push(catalog);
            }
        }
        Ok(catalogs)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::{
            AgentRegistryRuntime, CodexSandboxBackend, DockerSandboxBackend,
            OpenAiStyleRuntimeBackend, ProviderRuntimeBackend, resolve_discovery_url,
        },
        async_trait::async_trait,
        polyphony_core::{
            AgentDefinition, AgentModelCatalog, AgentProviderRuntime, AgentRunResult, AgentRunSpec,
            AgentRuntime, AgentTransport, BudgetSnapshot, Error as CoreError, RuntimeBackend,
            RuntimeBackendKind, SandboxBackend, SandboxBackendKind,
        },
        std::sync::{Arc, Mutex},
        tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            sync::mpsc,
        },
    };

    async fn spawn_json_server(
        body: serde_json::Value,
    ) -> (
        String,
        Arc<Mutex<Option<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request_line = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&request_line);
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let read = socket.read(&mut request).await.unwrap();
            let text = String::from_utf8_lossy(&request[..read]);
            let line = text.lines().next().map(str::to_owned);
            *captured.lock().unwrap() = line;
            let payload = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{addr}"), request_line, handle)
    }

    #[tokio::test]
    async fn registry_discovers_models_from_provider() {
        let runtime = AgentRegistryRuntime::new();
        let catalogs = runtime
            .discover_models(&[AgentDefinition {
                name: "claude".into(),
                kind: "claude".into(),
                transport: AgentTransport::LocalCli,
                models_command: Some("printf '[\"claude-sonnet\"]'".into()),
                fetch_models: true,
                ..AgentDefinition::default()
            }])
            .await
            .unwrap();
        assert_eq!(catalogs.len(), 1);
        assert_eq!(catalogs[0].models[0].id, "claude-sonnet");
    }

    #[derive(Default)]
    struct TestProviderRuntime {
        captured: Arc<Mutex<Vec<AgentDefinition>>>,
    }

    #[async_trait]
    impl AgentProviderRuntime for TestProviderRuntime {
        fn runtime_key(&self) -> String {
            "provider:test".into()
        }

        fn supports(&self, agent: &AgentDefinition) -> bool {
            matches!(
                agent.transport,
                AgentTransport::Mock | AgentTransport::AppServer
            )
        }

        async fn run(
            &self,
            spec: AgentRunSpec,
            _event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
        ) -> Result<AgentRunResult, CoreError> {
            self.captured.lock().unwrap().push(spec.agent);
            Ok(AgentRunResult::succeeded(1))
        }

        async fn fetch_budget(
            &self,
            _agent: &AgentDefinition,
        ) -> Result<Option<BudgetSnapshot>, CoreError> {
            Ok(None)
        }
    }

    struct PassthroughSandboxBackend;

    #[async_trait]
    impl SandboxBackend for PassthroughSandboxBackend {
        fn backend_key(&self) -> String {
            "sandbox:test".into()
        }

        fn kind(&self) -> SandboxBackendKind {
            SandboxBackendKind::Host
        }
    }

    struct RecordingRuntimeBackend;

    #[async_trait]
    impl RuntimeBackend for RecordingRuntimeBackend {
        fn backend_key(&self) -> String {
            "runtime:test".into()
        }

        fn kind(&self) -> RuntimeBackendKind {
            RuntimeBackendKind::LlamaCpp
        }

        async fn prepare_run(&self, mut spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
            spec.agent.base_url = Some("http://127.0.0.1:8080/v1".into());
            spec.agent
                .env
                .insert("POLYPHONY_RUNTIME".into(), "llama_cpp".into());
            Ok(spec)
        }

        async fn discover_models(
            &self,
            agent: &AgentDefinition,
        ) -> Result<Option<AgentModelCatalog>, CoreError> {
            Ok(Some(AgentModelCatalog {
                agent_name: agent.name.clone(),
                provider_kind: agent.kind.clone(),
                selected_model: Some("qwen2.5-coder".into()),
                ..AgentModelCatalog::default()
            }))
        }
    }

    #[derive(Default)]
    struct OrderedRuntimeBackend {
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl RuntimeBackend for OrderedRuntimeBackend {
        fn backend_key(&self) -> String {
            "runtime:ordered".into()
        }

        fn kind(&self) -> RuntimeBackendKind {
            RuntimeBackendKind::LlamaCpp
        }

        async fn prepare_run(&self, mut spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
            self.order.lock().unwrap().push("runtime");
            spec.agent
                .env
                .insert("POLYPHONY_RUNTIME".into(), "ready".into());
            Ok(spec)
        }
    }

    #[derive(Default)]
    struct OrderedSandboxBackend {
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl SandboxBackend for OrderedSandboxBackend {
        fn backend_key(&self) -> String {
            "sandbox:ordered".into()
        }

        fn kind(&self) -> SandboxBackendKind {
            SandboxBackendKind::Host
        }

        async fn prepare_run(&self, mut spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
            assert_eq!(
                spec.agent.env.get("POLYPHONY_RUNTIME").map(String::as_str),
                Some("ready")
            );
            self.order.lock().unwrap().push("sandbox");
            spec.agent
                .env
                .insert("POLYPHONY_SANDBOX".into(), "prepared".into());
            Ok(spec)
        }
    }

    #[tokio::test]
    async fn registry_prepares_spec_with_runtime_backends_before_provider_run() {
        let provider = TestProviderRuntime::default();
        let captured = provider.captured.clone();
        let runtime = AgentRegistryRuntime::from_components(
            vec![Arc::new(provider)],
            vec![Arc::new(PassthroughSandboxBackend)],
            vec![Arc::new(RecordingRuntimeBackend)],
        );
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let workspace_dir = tempfile::tempdir().unwrap();

        runtime
            .run(
                AgentRunSpec {
                    issue: polyphony_core::Issue {
                        id: "1".into(),
                        identifier: "ISSUE-1".into(),
                        title: "Title".into(),
                        state: "todo".into(),
                        ..polyphony_core::Issue::default()
                    },
                    attempt: None,
                    workspace_path: workspace_dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    agent: AgentDefinition {
                        name: "local".into(),
                        kind: "openai".into(),
                        transport: AgentTransport::Mock,
                        runtime: polyphony_core::AgentRuntimeConfig {
                            backend: RuntimeBackendKind::LlamaCpp,
                            ..polyphony_core::AgentRuntimeConfig::default()
                        },
                        sandbox: polyphony_core::AgentSandboxConfig {
                            backend: SandboxBackendKind::Host,
                            ..polyphony_core::AgentSandboxConfig::default()
                        },
                        ..AgentDefinition::default()
                    },
                    prior_context: None,
                },
                event_tx,
            )
            .await
            .unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].base_url.as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(
            captured[0].env.get("POLYPHONY_RUNTIME").map(String::as_str),
            Some("llama_cpp")
        );
    }

    #[tokio::test]
    async fn provider_runtime_backend_bridges_runtime_config_into_provider_fields() {
        let provider = TestProviderRuntime::default();
        let captured = provider.captured.clone();
        let runtime = AgentRegistryRuntime::from_components(
            vec![Arc::new(provider)],
            vec![Arc::new(PassthroughSandboxBackend)],
            vec![Arc::new(ProviderRuntimeBackend)],
        );
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let workspace_dir = tempfile::tempdir().unwrap();

        runtime
            .run(
                AgentRunSpec {
                    issue: polyphony_core::Issue {
                        id: "1".into(),
                        identifier: "ISSUE-1".into(),
                        title: "Title".into(),
                        state: "todo".into(),
                        ..polyphony_core::Issue::default()
                    },
                    attempt: None,
                    workspace_path: workspace_dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    agent: AgentDefinition {
                        name: "provider".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Mock,
                        runtime: polyphony_core::AgentRuntimeConfig {
                            backend: RuntimeBackendKind::Provider,
                            endpoint: Some("http://127.0.0.1:8080/v1".into()),
                            model: Some("sonnet".into()),
                            env: [("POLYPHONY_RUNTIME".into(), "provider".into())]
                                .into_iter()
                                .collect(),
                            ..polyphony_core::AgentRuntimeConfig::default()
                        },
                        sandbox: polyphony_core::AgentSandboxConfig {
                            backend: SandboxBackendKind::Host,
                            ..polyphony_core::AgentSandboxConfig::default()
                        },
                        ..AgentDefinition::default()
                    },
                    prior_context: None,
                },
                event_tx,
            )
            .await
            .unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].base_url.as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(captured[0].model.as_deref(), Some("sonnet"));
        assert_eq!(
            captured[0].env.get("POLYPHONY_RUNTIME").map(String::as_str),
            Some("provider")
        );
    }

    #[tokio::test]
    async fn ollama_runtime_backend_sets_default_base_url() {
        let prepared = OpenAiStyleRuntimeBackend {
            kind: RuntimeBackendKind::Ollama,
        }
        .prepare_run(AgentRunSpec {
            issue: polyphony_core::Issue::default(),
            attempt: None,
            workspace_path: std::env::temp_dir(),
            prompt: "hello".into(),
            max_turns: 1,
            agent: AgentDefinition {
                runtime: polyphony_core::AgentRuntimeConfig {
                    backend: RuntimeBackendKind::Ollama,
                    ..polyphony_core::AgentRuntimeConfig::default()
                },
                ..AgentDefinition::default()
            },
            prior_context: None,
        })
        .await
        .unwrap();

        assert_eq!(
            prepared.agent.base_url.as_deref(),
            Some("http://localhost:11434")
        );
    }

    #[test]
    fn ollama_discovery_url_strips_openai_compatible_suffix() {
        let agent = AgentDefinition {
            runtime: polyphony_core::AgentRuntimeConfig {
                backend: RuntimeBackendKind::Ollama,
                endpoint: Some("http://127.0.0.1:11434/v1".into()),
                ..polyphony_core::AgentRuntimeConfig::default()
            },
            ..AgentDefinition::default()
        };

        assert_eq!(
            resolve_discovery_url(&agent, RuntimeBackendKind::Ollama).as_deref(),
            Some("http://127.0.0.1:11434/api/tags")
        );
    }

    #[test]
    fn lm_studio_discovery_url_preserves_single_v1_prefix() {
        let agent = AgentDefinition {
            runtime: polyphony_core::AgentRuntimeConfig {
                backend: RuntimeBackendKind::LmStudio,
                endpoint: Some("http://127.0.0.1:1234/v1".into()),
                ..polyphony_core::AgentRuntimeConfig::default()
            },
            ..AgentDefinition::default()
        };

        assert_eq!(
            resolve_discovery_url(&agent, RuntimeBackendKind::LmStudio).as_deref(),
            Some("http://127.0.0.1:1234/v1/models")
        );
    }

    #[tokio::test]
    async fn registry_prefers_runtime_backend_model_discovery() {
        let runtime = AgentRegistryRuntime::from_components(
            Vec::new(),
            vec![Arc::new(PassthroughSandboxBackend)],
            vec![Arc::new(RecordingRuntimeBackend)],
        );

        let catalogs = runtime
            .discover_models(&[AgentDefinition {
                name: "local".into(),
                kind: "openai".into(),
                transport: AgentTransport::Mock,
                runtime: polyphony_core::AgentRuntimeConfig {
                    backend: RuntimeBackendKind::LlamaCpp,
                    ..polyphony_core::AgentRuntimeConfig::default()
                },
                sandbox: polyphony_core::AgentSandboxConfig {
                    backend: SandboxBackendKind::Host,
                    ..polyphony_core::AgentSandboxConfig::default()
                },
                ..AgentDefinition::default()
            }])
            .await
            .unwrap();

        assert_eq!(catalogs.len(), 1);
        assert_eq!(catalogs[0].selected_model.as_deref(), Some("qwen2.5-coder"));
    }

    #[tokio::test]
    async fn ollama_runtime_backend_discovers_models_from_api_tags() {
        let (base_url, request_line, handle) = spawn_json_server(serde_json::json!({
            "models": [
                {"name": "qwen2.5-coder:latest"},
                {"name": "llama3.2:latest"}
            ]
        }))
        .await;
        let runtime = OpenAiStyleRuntimeBackend {
            kind: RuntimeBackendKind::Ollama,
        };

        let catalog = runtime
            .discover_models(&AgentDefinition {
                name: "ollama".into(),
                kind: "openai".into(),
                fetch_models: true,
                runtime: polyphony_core::AgentRuntimeConfig {
                    backend: RuntimeBackendKind::Ollama,
                    endpoint: Some(format!("{base_url}/v1")),
                    ..polyphony_core::AgentRuntimeConfig::default()
                },
                ..AgentDefinition::default()
            })
            .await
            .unwrap()
            .unwrap();

        handle.await.unwrap();
        assert_eq!(
            request_line.lock().unwrap().as_deref(),
            Some("GET /api/tags HTTP/1.1")
        );
        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["qwen2.5-coder:latest", "llama3.2:latest"]
        );
        assert_eq!(
            catalog.selected_model.as_deref(),
            Some("qwen2.5-coder:latest")
        );
    }

    #[tokio::test]
    async fn lm_studio_runtime_backend_discovers_models_from_v1_models() {
        let (base_url, request_line, handle) = spawn_json_server(serde_json::json!({
            "data": [
                {"id": "qwen2.5-coder-32b", "name": "Qwen 2.5 Coder 32B", "object": "model"},
                {"id": "deepseek-r1-distill", "name": "DeepSeek R1 Distill", "object": "model"}
            ]
        }))
        .await;
        let runtime = OpenAiStyleRuntimeBackend {
            kind: RuntimeBackendKind::LmStudio,
        };

        let catalog = runtime
            .discover_models(&AgentDefinition {
                name: "lmstudio".into(),
                kind: "openai".into(),
                fetch_models: true,
                runtime: polyphony_core::AgentRuntimeConfig {
                    backend: RuntimeBackendKind::LmStudio,
                    endpoint: Some(base_url),
                    ..polyphony_core::AgentRuntimeConfig::default()
                },
                ..AgentDefinition::default()
            })
            .await
            .unwrap()
            .unwrap();

        handle.await.unwrap();
        assert_eq!(
            request_line.lock().unwrap().as_deref(),
            Some("GET /v1/models HTTP/1.1")
        );
        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["qwen2.5-coder-32b", "deepseek-r1-distill"]
        );
        assert_eq!(
            catalog.models[0].display_name.as_deref(),
            Some("Qwen 2.5 Coder 32B")
        );
        assert_eq!(
            catalog.models[1].display_name.as_deref(),
            Some("DeepSeek R1 Distill")
        );
    }

    #[tokio::test]
    async fn registry_applies_runtime_then_sandbox_backends_before_provider_run() {
        let provider = TestProviderRuntime::default();
        let captured = provider.captured.clone();
        let runtime_backend = OrderedRuntimeBackend::default();
        let runtime_order = runtime_backend.order.clone();
        let sandbox_backend = OrderedSandboxBackend::default();
        let sandbox_order = sandbox_backend.order.clone();
        let runtime = AgentRegistryRuntime::from_components(
            vec![Arc::new(provider)],
            vec![Arc::new(sandbox_backend)],
            vec![Arc::new(runtime_backend)],
        );
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let workspace_dir = tempfile::tempdir().unwrap();

        runtime
            .run(
                AgentRunSpec {
                    issue: polyphony_core::Issue {
                        id: "1".into(),
                        identifier: "ISSUE-1".into(),
                        title: "Title".into(),
                        state: "todo".into(),
                        ..polyphony_core::Issue::default()
                    },
                    attempt: None,
                    workspace_path: workspace_dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    agent: AgentDefinition {
                        name: "local".into(),
                        kind: "openai".into(),
                        transport: AgentTransport::Mock,
                        runtime: polyphony_core::AgentRuntimeConfig {
                            backend: RuntimeBackendKind::LlamaCpp,
                            ..polyphony_core::AgentRuntimeConfig::default()
                        },
                        sandbox: polyphony_core::AgentSandboxConfig {
                            backend: SandboxBackendKind::Host,
                            ..polyphony_core::AgentSandboxConfig::default()
                        },
                        ..AgentDefinition::default()
                    },
                    prior_context: None,
                },
                event_tx,
            )
            .await
            .unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].env.get("POLYPHONY_RUNTIME").map(String::as_str),
            Some("ready")
        );
        assert_eq!(
            captured[0].env.get("POLYPHONY_SANDBOX").map(String::as_str),
            Some("prepared")
        );
        drop(captured);

        assert_eq!(*runtime_order.lock().unwrap(), vec!["runtime"]);
        assert_eq!(*sandbox_order.lock().unwrap(), vec!["sandbox"]);
    }

    #[tokio::test]
    async fn codex_sandbox_backend_bridges_nested_sandbox_fields_for_provider_runtime() {
        let provider = TestProviderRuntime::default();
        let captured = provider.captured.clone();
        let runtime = AgentRegistryRuntime::from_components(
            vec![Arc::new(provider)],
            vec![Arc::new(CodexSandboxBackend)],
            vec![Arc::new(ProviderRuntimeBackend)],
        );
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let workspace_dir = tempfile::tempdir().unwrap();

        runtime
            .run(
                AgentRunSpec {
                    issue: polyphony_core::Issue {
                        id: "1".into(),
                        identifier: "ISSUE-1".into(),
                        title: "Title".into(),
                        state: "todo".into(),
                        ..polyphony_core::Issue::default()
                    },
                    attempt: None,
                    workspace_path: workspace_dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    agent: AgentDefinition {
                        name: "codex".into(),
                        kind: "codex".into(),
                        transport: AgentTransport::AppServer,
                        runtime: polyphony_core::AgentRuntimeConfig {
                            backend: RuntimeBackendKind::Provider,
                            ..polyphony_core::AgentRuntimeConfig::default()
                        },
                        sandbox: polyphony_core::AgentSandboxConfig {
                            backend: SandboxBackendKind::Codex,
                            profile: Some("workspace-write".into()),
                            policy: Some("allow-network".into()),
                            env: [("POLYPHONY_SANDBOX".into(), "codex".into())]
                                .into_iter()
                                .collect(),
                        },
                        ..AgentDefinition::default()
                    },
                    prior_context: None,
                },
                event_tx,
            )
            .await
            .unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].thread_sandbox.as_deref(),
            Some("workspace-write")
        );
        assert_eq!(
            captured[0].turn_sandbox_policy.as_deref(),
            Some("allow-network")
        );
        assert_eq!(
            captured[0].env.get("POLYPHONY_SANDBOX").map(String::as_str),
            Some("codex")
        );
    }

    #[tokio::test]
    async fn docker_sandbox_backend_rewrites_command_for_provider_runtime() {
        let provider = TestProviderRuntime::default();
        let captured = provider.captured.clone();
        let runtime = AgentRegistryRuntime::from_components(
            vec![Arc::new(provider)],
            vec![Arc::new(DockerSandboxBackend)],
            vec![Arc::new(ProviderRuntimeBackend)],
        );
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let workspace_dir = tempfile::tempdir().unwrap();

        runtime
            .run(
                AgentRunSpec {
                    issue: polyphony_core::Issue {
                        id: "1".into(),
                        identifier: "ISSUE-1".into(),
                        title: "Title".into(),
                        state: "todo".into(),
                        ..polyphony_core::Issue::default()
                    },
                    attempt: None,
                    workspace_path: workspace_dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    agent: AgentDefinition {
                        name: "dockerized".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Mock,
                        command: Some("claude --print \"$POLYPHONY_PROMPT\"".into()),
                        runtime: polyphony_core::AgentRuntimeConfig {
                            backend: RuntimeBackendKind::Provider,
                            ..polyphony_core::AgentRuntimeConfig::default()
                        },
                        sandbox: polyphony_core::AgentSandboxConfig {
                            backend: SandboxBackendKind::Docker,
                            profile: Some("workspace-write".into()),
                            policy: Some("allow-network".into()),
                            env: [(
                                "POLYPHONY_SANDBOX_DOCKER_IMAGE".into(),
                                "ghcr.io/polyphony/agent:latest".into(),
                            )]
                            .into_iter()
                            .collect(),
                        },
                        ..AgentDefinition::default()
                    },
                    prior_context: None,
                },
                event_tx,
            )
            .await
            .unwrap();

        let (command, sandbox_kind, manifest_path) = {
            let captured = captured.lock().unwrap();
            assert_eq!(captured.len(), 1);
            (
                captured[0].command.clone().unwrap(),
                captured[0].env.get("POLYPHONY_SANDBOX_KIND").cloned(),
                captured[0]
                    .env
                    .get("POLYPHONY_SANDBOX_DOCKER_MANIFEST")
                    .cloned()
                    .unwrap(),
            )
        };
        assert!(command.contains("internal docker-sandbox-run --manifest"));
        assert_eq!(sandbox_kind.as_deref(), Some("docker"));

        let manifest = tokio::fs::read_to_string(&manifest_path).await.unwrap();
        assert!(manifest.contains("ghcr.io/polyphony/agent:latest"));
        assert!(manifest.contains("claude --print"));
    }

    #[test]
    fn registry_registers_only_requested_default_runtime_backends() {
        let runtime = AgentRegistryRuntime::new();
        let kinds = runtime
            .runtime_backends
            .iter()
            .map(|backend| backend.kind())
            .collect::<Vec<_>>();

        assert_eq!(kinds, vec![
            RuntimeBackendKind::Provider,
            RuntimeBackendKind::OpenAiCompatible,
            RuntimeBackendKind::Ollama,
            RuntimeBackendKind::LmStudio,
        ]);
    }

    #[test]
    fn registry_registers_default_sandbox_backends() {
        let runtime = AgentRegistryRuntime::new();
        let kinds = runtime
            .sandbox_backends
            .iter()
            .map(|backend| backend.kind())
            .collect::<Vec<_>>();

        assert_eq!(kinds, vec![
            SandboxBackendKind::Host,
            SandboxBackendKind::Codex,
            SandboxBackendKind::Docker,
        ]);
    }
}
