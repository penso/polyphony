use {
    async_trait::async_trait,
    polyphony_agent_local::LocalCliRuntime,
    polyphony_core::{
        AgentDefinition, AgentModelCatalog, AgentProviderRuntime, AgentRunResult, AgentRunSpec,
        BudgetSnapshot, Error as CoreError,
    },
    tokio::sync::mpsc,
};

#[derive(Debug, Clone)]
pub struct ClaudeRuntime {
    local: LocalCliRuntime,
}

impl Default for ClaudeRuntime {
    fn default() -> Self {
        Self {
            local: LocalCliRuntime::new(["claude", "anthropic"]),
        }
    }
}

#[async_trait]
impl AgentProviderRuntime for ClaudeRuntime {
    fn runtime_key(&self) -> String {
        "agent:claude".into()
    }

    fn supports(&self, agent: &AgentDefinition) -> bool {
        self.local.supports(agent)
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        self.local.run(spec, event_tx).await
    }

    async fn fetch_budget(
        &self,
        agent: &AgentDefinition,
    ) -> Result<Option<BudgetSnapshot>, CoreError> {
        self.local.fetch_budget(agent).await
    }

    async fn discover_models(
        &self,
        agent: &AgentDefinition,
    ) -> Result<Option<AgentModelCatalog>, CoreError> {
        self.local.discover_models(agent).await
    }
}

#[cfg(test)]
mod tests {
    use {
        super::ClaudeRuntime,
        polyphony_core::{AgentDefinition, AgentProviderRuntime},
    };

    #[test]
    fn supports_claude_kind() {
        let runtime = ClaudeRuntime::default();
        assert!(runtime.supports(&AgentDefinition {
            kind: "claude".into(),
            ..AgentDefinition::default()
        }));
    }
}
