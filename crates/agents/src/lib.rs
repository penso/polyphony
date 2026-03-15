use std::sync::Arc;

use {
    async_trait::async_trait,
    polyphony_core::{
        AgentDefinition, AgentModelCatalog, AgentProviderRuntime, AgentRunResult, AgentRunSpec,
        AgentRuntime, AgentSession, BudgetSnapshot, Error as CoreError, ToolExecutor,
    },
    tokio::sync::mpsc,
};

#[derive(Default)]
pub struct AgentRegistryRuntime {
    providers: Vec<Arc<dyn AgentProviderRuntime>>,
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
        Self { providers }
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
        self.provider_for(&spec.agent)?
            .start_session(spec, event_tx)
            .await
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
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
            if let Some(catalog) = self.provider_for(agent)?.discover_models(agent).await? {
                catalogs.push(catalog);
            }
        }
        Ok(catalogs)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::AgentRegistryRuntime,
        polyphony_core::{AgentDefinition, AgentRuntime, AgentTransport},
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
}
