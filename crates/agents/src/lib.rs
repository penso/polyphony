use std::sync::Arc;

use {
    async_trait::async_trait,
    polyphony_core::{
        AgentDefinition, AgentModelCatalog, AgentProviderRuntime, AgentRunResult, AgentRunSpec,
        AgentRuntime, AgentSession, BudgetSnapshot, Error as CoreError, RuntimeBackend,
        RuntimeBackendKind, SandboxBackend, SandboxBackendKind, ToolExecutor,
    },
    tokio::sync::mpsc,
};

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
struct ProviderRuntimeBackend;

#[derive(Clone, Copy)]
struct OpenAiStyleRuntimeBackend {
    kind: RuntimeBackendKind,
}

fn apply_runtime_overrides(mut spec: AgentRunSpec) -> AgentRunSpec {
    if spec.agent.base_url.is_none() {
        spec.agent.base_url = spec.agent.runtime.endpoint.clone();
    }
    if spec.agent.model.is_none() {
        spec.agent.model = spec.agent.runtime.model.clone();
    }
    spec.agent.env.extend(spec.agent.runtime.env.clone());
    spec
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
        let sandbox_backends: Vec<Arc<dyn SandboxBackend>> =
            vec![Arc::new(HostSandboxBackend), Arc::new(CodexSandboxBackend)];
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
impl RuntimeBackend for ProviderRuntimeBackend {
    fn backend_key(&self) -> String {
        "runtime:provider".into()
    }

    fn kind(&self) -> RuntimeBackendKind {
        RuntimeBackendKind::Provider
    }

    async fn prepare_run(&self, spec: AgentRunSpec) -> Result<AgentRunSpec, CoreError> {
        Ok(apply_runtime_overrides(spec))
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
        Ok(apply_runtime_overrides(spec))
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
        super::{AgentRegistryRuntime, CodexSandboxBackend, ProviderRuntimeBackend},
        async_trait::async_trait,
        polyphony_core::{
            AgentDefinition, AgentModelCatalog, AgentProviderRuntime, AgentRunResult, AgentRunSpec,
            AgentRuntime, AgentTransport, BudgetSnapshot, Error as CoreError, RuntimeBackend,
            RuntimeBackendKind, SandboxBackend, SandboxBackendKind,
        },
        std::sync::{Arc, Mutex},
        tokio::sync::mpsc,
    };

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
}
