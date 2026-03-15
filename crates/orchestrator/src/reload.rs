use crate::{prelude::*, *};

impl RuntimeService {
    pub(crate) fn workflow(&self) -> LoadedWorkflow {
        self.workflow_rx.borrow().clone()
    }

    pub(crate) fn workflow_reload_error(&self) -> Option<&str> {
        self.reload_support
            .as_ref()
            .and_then(|support| support.reload_error.as_deref())
    }

    pub(crate) async fn reload_workflow_from_disk(&mut self, force: bool, reason: &str) {
        let Some(reload_support) = self.reload_support.as_ref() else {
            return;
        };
        let workflow_path = reload_support.workflow_path.clone();
        let user_config_path = reload_support.user_config_path.clone();
        let workflow_tx = reload_support.workflow_tx.clone();
        let component_factory = reload_support.component_factory.clone();
        let last_seen_fingerprint = reload_support.last_seen_fingerprint.clone();

        let fingerprint =
            match workflow_inputs_fingerprint(&workflow_path, user_config_path.as_deref()) {
                Ok(fingerprint) => Some(fingerprint),
                Err(error) => {
                    self.note_workflow_reload_failure(
                        None,
                        format!("workflow fingerprint failed: {error}"),
                        reason,
                    );
                    return;
                },
            };
        if !force && fingerprint == last_seen_fingerprint {
            return;
        }

        match load_workflow_with_user_config(&workflow_path, user_config_path.as_deref()) {
            Ok(workflow) => {
                let current_workflow = self.workflow();
                let recovered = self.clear_workflow_reload_error(fingerprint.clone());
                if workflow.definition == current_workflow.definition
                    && workflow.config == current_workflow.config
                {
                    if recovered {
                        self.push_event(
                            EventScope::Workflow,
                            format!("workflow reload recovered via {reason}"),
                        );
                        info!(%reason, "workflow reload recovered without config changes");
                    }
                    return;
                }

                match component_factory(&workflow) {
                    Ok(components) => {
                        self.apply_reloaded_components(&current_workflow, &workflow, components);
                        let _ = workflow_tx.send(workflow);
                        self.push_event(
                            EventScope::Workflow,
                            format!("workflow reloaded via {reason}"),
                        );
                        info!(%reason, path = %workflow_path.display(), "workflow reloaded");
                    },
                    Err(error) => {
                        self.note_workflow_reload_failure(
                            fingerprint,
                            format!("component rebuild failed: {error}"),
                            reason,
                        );
                    },
                }
            },
            Err(error) => {
                self.note_workflow_reload_failure(fingerprint, error.to_string(), reason);
            },
        }
    }

    pub(crate) fn clear_workflow_reload_error(
        &mut self,
        fingerprint: Option<WorkflowInputsFingerprint>,
    ) -> bool {
        let Some(reload_support) = self.reload_support.as_mut() else {
            return false;
        };
        if let Some(fingerprint) = fingerprint {
            reload_support.last_seen_fingerprint = Some(fingerprint);
        }
        reload_support.reload_error.take().is_some()
    }

    pub(crate) fn note_workflow_reload_failure(
        &mut self,
        fingerprint: Option<WorkflowInputsFingerprint>,
        error: String,
        reason: &str,
    ) {
        let changed = {
            let Some(reload_support) = self.reload_support.as_mut() else {
                return;
            };
            if let Some(fingerprint) = fingerprint {
                reload_support.last_seen_fingerprint = Some(fingerprint);
            }
            let changed = reload_support.reload_error.as_deref() != Some(error.as_str());
            reload_support.reload_error = Some(error.clone());
            changed
        };
        if changed {
            self.push_event(
                EventScope::Workflow,
                format!("workflow reload failed via {reason}: {error}"),
            );
        }
        warn!(%error, %reason, "workflow reload failed; keeping last good config");
    }

    pub(crate) fn apply_reloaded_components(
        &mut self,
        current_workflow: &LoadedWorkflow,
        new_workflow: &LoadedWorkflow,
        components: RuntimeComponents,
    ) {
        let old_tracker_key = self.tracker.component_key();
        let new_tracker_key = components.tracker.component_key();
        let old_review_source_key = self
            .pull_request_trigger_source
            .as_ref()
            .map(|source| source.component_key());
        let new_review_source_key = components
            .pull_request_trigger_source
            .as_ref()
            .map(|source| source.component_key());
        let old_agent_runtime_key = self.agent.component_key();
        let new_agent_runtime_key = components.agent.component_key();
        let old_agent_names = current_workflow
            .config
            .all_agents()
            .into_iter()
            .map(|agent| agent.name)
            .collect::<HashSet<_>>();
        let new_agent_names = new_workflow
            .config
            .all_agents()
            .into_iter()
            .map(|agent| agent.name)
            .collect::<HashSet<_>>();
        let removed_agents = old_agent_names
            .difference(&new_agent_names)
            .cloned()
            .collect::<Vec<_>>();
        let affected_agent_budgets = old_agent_names
            .union(&new_agent_names)
            .map(|name| format!("agent:{name}"))
            .collect::<HashSet<_>>();

        self.tracker = components.tracker;
        self.pull_request_trigger_source = components.pull_request_trigger_source;
        self.agent = components.agent;
        self.committer = components.committer;
        self.pull_request_manager = components.pull_request_manager;
        self.pull_request_commenter = components.pull_request_commenter;
        self.feedback = components.feedback;

        if old_tracker_key != new_tracker_key {
            self.state.throttles.remove(&old_tracker_key);
            self.state.budgets.remove(&old_tracker_key);
            self.state.tracker_connection = None;
        }
        if old_review_source_key != new_review_source_key
            && let Some(component_key) = old_review_source_key
        {
            self.state.throttles.remove(&component_key);
            self.state.budgets.remove(&component_key);
        }
        if old_agent_runtime_key != new_agent_runtime_key {
            self.state.throttles.remove(&old_agent_runtime_key);
        }
        for agent_name in removed_agents {
            self.state.throttles.remove(&format!("agent:{agent_name}"));
            self.state.agent_catalogs.remove(&agent_name);
        }
        self.state
            .budgets
            .retain(|component, _| !affected_agent_budgets.contains(component));
        self.state.agent_catalogs.clear();
        self.state.last_tracker_poll_at = None;
        self.state.last_tracker_connection_poll_at = None;
        self.state.last_budget_poll_at = None;
        self.state.last_model_discovery_at = None;
    }
}
