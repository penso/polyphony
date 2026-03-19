use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use polyphony_core::{
    AgentDefinition, AgentModelCatalog, AgentProviderRuntime, AgentRunResult, AgentRunSpec,
    AgentRuntime, AgentSession, BudgetSnapshot, Error as CoreError, ToolExecutor,
};
use tokio::sync::mpsc;

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
        let mut cached: BTreeMap<String, BudgetSnapshot> = BTreeMap::new();
        for agent in agents {
            let provider = self.provider_for(agent)?;
            let cache_key = format!(
                "{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
                provider.runtime_key(),
                agent.kind,
                agent.base_url,
                agent.api_key,
                agent.credits_command,
                agent.spending_command,
                agent.env,
            );
            let fetched = if let Some(snapshot) = cached.get(&cache_key) {
                snapshot.clone()
            } else {
                let Some(snapshot) = provider.fetch_budget(agent).await? else {
                    continue;
                };
                cached.insert(cache_key, snapshot.clone());
                snapshot
            };
            let mut snapshot = fetched;
            snapshot.component = format!("agent:{}", agent.name);
            if let Some(raw) = snapshot.raw.as_mut()
                && raw.get("agent_name").is_none()
            {
                raw["agent_name"] = agent.name.clone().into();
            }
            snapshots.push(snapshot);
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use polyphony_core::{
        AgentDefinition, AgentProviderRuntime, AgentRuntime, AgentTransport, BudgetSnapshot,
        Error as CoreError,
    };
    use serde_json::json;

    use super::AgentRegistryRuntime;

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
    struct CountingBudgetProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AgentProviderRuntime for CountingBudgetProvider {
        fn runtime_key(&self) -> String {
            "agent:test".into()
        }

        fn supports(&self, agent: &AgentDefinition) -> bool {
            agent.kind == "test"
        }

        async fn run(
            &self,
            _spec: polyphony_core::AgentRunSpec,
            _event_tx: tokio::sync::mpsc::UnboundedSender<polyphony_core::AgentEvent>,
        ) -> Result<polyphony_core::AgentRunResult, CoreError> {
            Err(CoreError::Adapter("unused".into()))
        }

        async fn fetch_budget(
            &self,
            _agent: &AgentDefinition,
        ) -> Result<Option<BudgetSnapshot>, CoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(BudgetSnapshot {
                component: "agent:shared".into(),
                captured_at: chrono::Utc::now(),
                credits_remaining: Some(42.0),
                credits_total: Some(100.0),
                spent_usd: None,
                soft_limit_usd: None,
                hard_limit_usd: None,
                reset_at: None,
                raw: Some(json!({"provider": "test"})),
            }))
        }
    }

    #[tokio::test]
    async fn registry_reuses_shared_budget_fetches_for_equivalent_agents() {
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = AgentRegistryRuntime {
            providers: vec![Arc::new(CountingBudgetProvider {
                calls: Arc::clone(&calls),
            })],
        };
        let budgets = runtime
            .fetch_budgets(&[
                AgentDefinition {
                    name: "router".into(),
                    kind: "test".into(),
                    ..AgentDefinition::default()
                },
                AgentDefinition {
                    name: "reviewer".into(),
                    kind: "test".into(),
                    ..AgentDefinition::default()
                },
            ])
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(budgets.len(), 2);
        assert_eq!(budgets[0].component, "agent:router");
        assert_eq!(budgets[1].component, "agent:reviewer");
    }
}
