use liquid::{ParserBuilder, model::Value as LiquidValue, object};
use serde::Deserialize;

use crate::{prelude::*, *};

/// Maximum length for issue descriptions in the heartbeat context to keep token cost low.
const MAX_DESCRIPTION_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// Heartbeat dispatch decision (parsed from LLM JSON response)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HeartbeatDispatchDecision {
    #[serde(default)]
    pub dispatch: Vec<HeartbeatDispatchItem>,
    #[serde(default)]
    pub skip: Vec<HeartbeatSkipItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HeartbeatDispatchItem {
    pub issue_id: String,
    pub agent: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HeartbeatSkipItem {
    pub issue_id: String,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Context structs for prompt rendering
// ---------------------------------------------------------------------------

struct CandidateContext {
    identifier: String,
    issue_id: String,
    state: String,
    title: String,
    description: String,
    priority: Option<i32>,
    labels: Vec<String>,
}

struct RunningTaskContext {
    issue_identifier: String,
    agent_name: String,
    elapsed: String,
}

struct AgentContext {
    name: String,
    model: Option<String>,
    description: Option<String>,
}

// ---------------------------------------------------------------------------
// Build heartbeat context and render prompt
// ---------------------------------------------------------------------------

impl RuntimeService {
    /// Render the heartbeat prompt with the given orchestrator state context.
    pub(crate) fn render_heartbeat_prompt(
        &self,
        candidates: &[(LoadedWorkflow, Issue)],
    ) -> Result<String, Error> {
        let workflow = self.workflow_rx.borrow();
        let prompt_source = workflow
            .config
            .heartbeat
            .prompt
            .as_deref()
            .unwrap_or(DEFAULT_HEARTBEAT_PROMPT);

        let candidate_contexts = self.build_candidate_contexts(candidates);
        let running_contexts = self.build_running_contexts();
        let agent_contexts = self.build_agent_contexts();
        let slot_limit = workflow.config.agent.max_concurrent_agents;
        let running_count = self.state.running.len();
        let available_slots = slot_limit.saturating_sub(running_count);
        let dispatch_mode = format!("{}", self.state.dispatch_mode);

        let candidates_liquid = candidate_contexts
            .iter()
            .map(|c| {
                let mut obj = liquid::model::Object::new();
                obj.insert(
                    "identifier".into(),
                    LiquidValue::scalar(c.identifier.clone()),
                );
                obj.insert("issue_id".into(), LiquidValue::scalar(c.issue_id.clone()));
                obj.insert("state".into(), LiquidValue::scalar(c.state.clone()));
                obj.insert("title".into(), LiquidValue::scalar(c.title.clone()));
                obj.insert(
                    "description".into(),
                    LiquidValue::scalar(c.description.clone()),
                );
                obj.insert(
                    "priority".into(),
                    c.priority
                        .map(LiquidValue::scalar)
                        .unwrap_or(LiquidValue::Nil),
                );
                let labels: liquid::model::Array = c
                    .labels
                    .iter()
                    .map(|l| LiquidValue::scalar(l.clone()))
                    .collect();
                obj.insert("labels".into(), LiquidValue::Array(labels));
                LiquidValue::Object(obj)
            })
            .collect::<liquid::model::Array>();

        let running_liquid = running_contexts
            .iter()
            .map(|r| {
                let mut obj = liquid::model::Object::new();
                obj.insert(
                    "issue_identifier".into(),
                    LiquidValue::scalar(r.issue_identifier.clone()),
                );
                obj.insert(
                    "agent_name".into(),
                    LiquidValue::scalar(r.agent_name.clone()),
                );
                obj.insert("elapsed".into(), LiquidValue::scalar(r.elapsed.clone()));
                LiquidValue::Object(obj)
            })
            .collect::<liquid::model::Array>();

        let agents_liquid = agent_contexts
            .iter()
            .map(|a| {
                let mut obj = liquid::model::Object::new();
                obj.insert("name".into(), LiquidValue::scalar(a.name.clone()));
                obj.insert(
                    "model".into(),
                    a.model
                        .as_ref()
                        .map(|m| LiquidValue::scalar(m.clone()))
                        .unwrap_or(LiquidValue::Nil),
                );
                obj.insert(
                    "description".into(),
                    a.description
                        .as_ref()
                        .map(|d| LiquidValue::scalar(d.clone()))
                        .unwrap_or(LiquidValue::Nil),
                );
                LiquidValue::Object(obj)
            })
            .collect::<liquid::model::Array>();

        let globals = object!({
            "candidates": candidates_liquid,
            "running_tasks": running_liquid,
            "available_agents": agents_liquid,
            "slot_limit": slot_limit,
            "running_count": running_count,
            "available_slots": available_slots,
            "dispatch_mode": dispatch_mode,
        });

        let parser = ParserBuilder::with_stdlib()
            .build()
            .map_err(|err| polyphony_workflow::Error::TemplateParse(err.to_string()))?;
        let template = parser
            .parse(prompt_source)
            .map_err(|err| polyphony_workflow::Error::TemplateParse(err.to_string()))?;
        let rendered = template
            .render(&globals)
            .map_err(|err| polyphony_workflow::Error::TemplateRender(err.to_string()))?;

        Ok(rendered)
    }

    fn build_candidate_contexts(
        &self,
        candidates: &[(LoadedWorkflow, Issue)],
    ) -> Vec<CandidateContext> {
        candidates
            .iter()
            .map(|(_, issue)| {
                let desc = issue
                    .description
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(MAX_DESCRIPTION_CHARS)
                    .collect::<String>();
                CandidateContext {
                    identifier: issue.identifier.clone(),
                    issue_id: issue.id.clone(),
                    state: issue.state.clone(),
                    title: issue.title.clone(),
                    description: desc,
                    priority: issue.priority,
                    labels: issue.labels.clone(),
                }
            })
            .collect()
    }

    fn build_running_contexts(&self) -> Vec<RunningTaskContext> {
        self.state
            .running
            .values()
            .map(|running| {
                let elapsed = Utc::now()
                    .signed_duration_since(running.started_at)
                    .to_std()
                    .unwrap_or_default();
                let mins = elapsed.as_secs() / 60;
                let elapsed_str = if mins > 0 {
                    format!("{mins}m")
                } else {
                    format!("{}s", elapsed.as_secs())
                };
                RunningTaskContext {
                    issue_identifier: running.issue.identifier.clone(),
                    agent_name: running.agent_name.clone(),
                    elapsed: elapsed_str,
                }
            })
            .collect()
    }

    fn build_agent_contexts(&self) -> Vec<AgentContext> {
        self.workflow_rx
            .borrow()
            .config
            .agents
            .profiles
            .iter()
            .map(|(name, profile)| AgentContext {
                name: name.clone(),
                model: profile.model.clone(),
                description: profile.description.clone(),
            })
            .collect()
    }

    /// Run the heartbeat dispatch: render prompt, call the LLM, parse the JSON response.
    /// Returns `Ok(Some(decision))` on success, `Ok(None)` if heartbeat is not configured
    /// or the LLM call fails (fallback to rule-based), `Err` on fatal errors.
    pub(crate) async fn run_heartbeat_dispatch(
        &mut self,
        candidates: &[(LoadedWorkflow, Issue)],
    ) -> Result<Option<HeartbeatDispatchDecision>, Error> {
        let workflow = self.workflow_rx.borrow().clone();
        if !workflow.config.heartbeat.enabled {
            return Ok(None);
        }

        let heartbeat_agent_name = match &workflow.config.heartbeat.agent {
            Some(name) => name.clone(),
            None => return Ok(None),
        };

        let agent_def = workflow
            .config
            .expand_agent_candidates(&heartbeat_agent_name)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                polyphony_workflow::Error::InvalidConfig(format!(
                    "heartbeat.agent `{heartbeat_agent_name}` could not be resolved"
                ))
            })?;

        let prompt = self.render_heartbeat_prompt(candidates)?;

        self.state.heartbeat_status.enabled = true;
        self.state.heartbeat_status.agent_name = Some(heartbeat_agent_name.clone());

        let start = Instant::now();

        // Create a minimal Issue for the AgentRunSpec (the heartbeat is not issue-specific,
        // but the agent runtime interface requires one).
        let heartbeat_issue = Issue {
            id: "__heartbeat__".to_string(),
            identifier: "heartbeat".to_string(),
            title: "Heartbeat dispatch decision".to_string(),
            state: "system".to_string(),
            ..Default::default()
        };

        let workspace_path = env::temp_dir().join("polyphony_heartbeat");
        let spec = AgentRunSpec {
            issue: heartbeat_issue,
            attempt: None,
            workspace_path,
            prompt,
            max_turns: 1,
            agent: agent_def,
            prior_context: None,
        };

        // Create a channel for agent events (response text + token usage).
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let agent = self.agent.clone();
        let result = agent.run(spec, event_tx).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        // Collect response text and token usage from the event stream.
        let mut response_text = String::new();
        let mut tokens_used: u64 = 0;
        while let Ok(event) = event_rx.try_recv() {
            if let Some(msg) = &event.message
                && matches!(
                    event.kind,
                    polyphony_core::AgentEventKind::TurnCompleted
                        | polyphony_core::AgentEventKind::Outcome
                        | polyphony_core::AgentEventKind::OtherMessage
                )
                && !msg.is_empty()
            {
                if !response_text.is_empty() {
                    response_text.push('\n');
                }
                response_text.push_str(msg);
            }
            if let Some(usage) = &event.usage {
                tokens_used = usage.total_tokens;
            }
        }

        match result {
            Ok(_run_result) => {
                if response_text.is_empty() {
                    self.state.heartbeat_status.fallback_count += 1;
                    warn!(
                        "heartbeat agent returned no response text, falling back to rule-based dispatch"
                    );
                    self.push_event(
                        EventScope::Heartbeat,
                        "heartbeat: no response text from agent".to_string(),
                    );
                    return Ok(None);
                }
                match parse_heartbeat_response(&response_text) {
                    Ok(decision) => {
                        let summary = polyphony_core::HeartbeatDecisionSummary {
                            at: Utc::now(),
                            dispatched: decision
                                .dispatch
                                .iter()
                                .map(|d| polyphony_core::HeartbeatDispatchEntry {
                                    issue_id: d.issue_id.clone(),
                                    agent: d.agent.clone(),
                                    reason: d.reason.clone(),
                                })
                                .collect(),
                            skipped: decision
                                .skip
                                .iter()
                                .map(|s| polyphony_core::HeartbeatSkipEntry {
                                    issue_id: s.issue_id.clone(),
                                    reason: s.reason.clone(),
                                })
                                .collect(),
                            tokens_used,
                            duration_ms,
                        };

                        self.state.heartbeat_status.last_run_at = Some(Utc::now());
                        self.state.heartbeat_status.total_tokens_used += tokens_used;
                        self.state.heartbeat_status.last_decision = Some(summary);

                        self.push_event(
                            EventScope::Heartbeat,
                            format!(
                                "heartbeat: dispatch {} issues, skip {} ({tokens_used} tokens, {duration_ms}ms)",
                                decision.dispatch.len(),
                                decision.skip.len(),
                            ),
                        );

                        for skip in &decision.skip {
                            self.push_event(
                                EventScope::Heartbeat,
                                format!("heartbeat skip {}: {}", skip.issue_id, skip.reason),
                            );
                        }

                        Ok(Some(decision))
                    },
                    Err(parse_err) => {
                        self.state.heartbeat_status.fallback_count += 1;
                        warn!(
                            %parse_err,
                            "heartbeat response parse failed, falling back to rule-based dispatch"
                        );
                        self.push_event(
                            EventScope::Heartbeat,
                            format!("heartbeat parse error: {parse_err}"),
                        );
                        Ok(None)
                    },
                }
            },
            Err(error) => {
                self.state.heartbeat_status.fallback_count += 1;
                warn!(
                    %error,
                    "heartbeat LLM call failed, falling back to rule-based dispatch"
                );
                self.push_event(
                    EventScope::Heartbeat,
                    format!("heartbeat LLM error: {error}"),
                );
                Ok(None)
            },
        }
    }
}

/// Parse the heartbeat LLM response, extracting JSON from the response text.
/// Handles responses that may include markdown code fences.
fn parse_heartbeat_response(text: &str) -> Result<HeartbeatDispatchDecision, String> {
    let trimmed = text.trim();

    // Try to extract JSON from markdown code fences first
    let json_str = if let Some(start) = trimmed.find("```json") {
        let after_fence = &trimmed[start + 7..];
        if let Some(end) = after_fence.find("```") {
            after_fence[..end].trim()
        } else {
            after_fence.trim()
        }
    } else if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        if let Some(end) = after_fence.find("```") {
            after_fence[..end].trim()
        } else {
            after_fence.trim()
        }
    } else {
        trimmed
    };

    serde_json::from_str::<HeartbeatDispatchDecision>(json_str)
        .map_err(|err| format!("invalid JSON: {err}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_dispatch_response() {
        let json = r#"{
            "dispatch": [
                { "issue_id": "ABC-123", "agent": "implementer", "reason": "high priority" }
            ],
            "skip": [
                { "issue_id": "ABC-456", "reason": "blocked by ABC-123" }
            ]
        }"#;
        let decision = parse_heartbeat_response(json).unwrap();
        assert_eq!(decision.dispatch.len(), 1);
        assert_eq!(decision.dispatch[0].issue_id, "ABC-123");
        assert_eq!(decision.dispatch[0].agent, "implementer");
        assert_eq!(decision.skip.len(), 1);
        assert_eq!(decision.skip[0].issue_id, "ABC-456");
    }

    #[test]
    fn parse_response_with_code_fence() {
        let text = r#"Here is my decision:

```json
{
    "dispatch": [
        { "issue_id": "X-1", "agent": "coder", "reason": "ready to go" }
    ],
    "skip": []
}
```"#;
        let decision = parse_heartbeat_response(text).unwrap();
        assert_eq!(decision.dispatch.len(), 1);
        assert_eq!(decision.dispatch[0].issue_id, "X-1");
    }

    #[test]
    fn parse_empty_response() {
        let json = r#"{ "dispatch": [], "skip": [] }"#;
        let decision = parse_heartbeat_response(json).unwrap();
        assert!(decision.dispatch.is_empty());
        assert!(decision.skip.is_empty());
    }

    #[test]
    fn parse_invalid_json() {
        let text = "this is not json";
        assert!(parse_heartbeat_response(text).is_err());
    }

    #[test]
    fn parse_partial_fields_default() {
        let json = r#"{ "dispatch": [{ "issue_id": "A-1", "agent": "dev", "reason": "go" }] }"#;
        let decision = parse_heartbeat_response(json).unwrap();
        assert_eq!(decision.dispatch.len(), 1);
        assert!(decision.skip.is_empty());
    }
}
