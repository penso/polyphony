use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::Utc;
use futures_util::StreamExt;
use polyphony_agent_common::{
    fetch_budget_for_agent, merge_models, model_from_json, run_shell_capture, selected_model,
};
use polyphony_core::{
    AgentDefinition, AgentEventKind, AgentModel, AgentModelCatalog, AgentProviderRuntime,
    AgentRunResult, AgentRunSpec, AgentTransport, BudgetSnapshot, Error as CoreError,
    RateLimitSignal, TokenUsage, ToolCallRequest, ToolExecutor,
};
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum Error {
    #[error("openai agent error: {0}")]
    OpenAi(String),
}

/// Resolve the base URL for an OpenAI-compatible agent. Uses the explicit
/// `base_url` when set (typically injected by the workflow layer's
/// `default_agent_base_url`), otherwise falls back to `api.openai.com`.
fn resolve_base_url(agent: &AgentDefinition) -> String {
    agent
        .base_url
        .clone()
        .unwrap_or_else(|| "https://api.openai.com/v1".into())
}

#[derive(Clone)]
pub struct OpenAiRuntime {
    http: reqwest::Client,
    tool_executor: Option<Arc<dyn ToolExecutor>>,
}

impl OpenAiRuntime {
    pub fn new(tool_executor: Option<Arc<dyn ToolExecutor>>) -> Self {
        Self {
            http: reqwest::Client::new(),
            tool_executor,
        }
    }
}

impl Default for OpenAiRuntime {
    fn default() -> Self {
        Self::new(None)
    }
}

impl std::fmt::Debug for OpenAiRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiRuntime")
            .field(
                "tool_executor",
                &self.tool_executor.as_ref().map(|_| "configured"),
            )
            .finish()
    }
}

#[async_trait]
impl AgentProviderRuntime for OpenAiRuntime {
    fn runtime_key(&self) -> String {
        "agent:openai".into()
    }

    fn supports(&self, agent: &AgentDefinition) -> bool {
        matches!(agent.transport, AgentTransport::OpenAiChat)
            || matches!(
                agent.kind.as_str(),
                "openai" | "openai-compatible" | "openrouter"
            )
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        run_openai_chat(&self.http, spec, event_tx, self.tool_executor.as_ref()).await
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
        discover_models_for_agent(&self.http, agent).await
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
    let discovered = if let Some(command) = &agent.models_command {
        polyphony_agent_common::parse_model_list(
            &run_shell_capture(command, None, &agent.env).await?,
        )?
    } else if agent.fetch_models && agent.api_key.is_none() {
        debug!(
            agent_name = %agent.name,
            provider_kind = %agent.kind,
            "skipping OpenAI-compatible model discovery because api_key is not configured"
        );
        Vec::new()
    } else if agent.fetch_models {
        discover_openai_models(client, agent).await?
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

async fn discover_openai_models(
    client: &reqwest::Client,
    agent: &AgentDefinition,
) -> Result<Vec<AgentModel>, CoreError> {
    let api_key = agent.api_key.clone().ok_or_else(|| {
        CoreError::Adapter(format!(
            "agent `{}` api_key is required for model discovery",
            agent.name
        ))
    })?;
    let base_url = resolve_base_url(agent);
    info!(
        agent_name = %agent.name,
        provider_kind = %agent.kind,
        base_url,
        "discovering OpenAI-compatible models"
    );
    let response = client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .bearer_auth(api_key)
        .header("User-Agent", "polyphony")
        .send()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let status = response.status();
    if status.as_u16() == 429 {
        warn!(
            agent_name = %agent.name,
            provider_kind = %agent.kind,
            "OpenAI-compatible model discovery hit rate limit"
        );
        return Err(CoreError::RateLimited(Box::new(RateLimitSignal {
            component: format!("agent:{}", agent.name),
            reason: "models_discovery_429".into(),
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
    let models = payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(model_from_json)
        .collect::<Vec<_>>();
    debug!(
        agent_name = %agent.name,
        discovered_models = models.len(),
        "discovered OpenAI-compatible models"
    );
    Ok(models)
}

async fn run_openai_chat(
    client: &reqwest::Client,
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    tool_executor: Option<&Arc<dyn ToolExecutor>>,
) -> Result<AgentRunResult, CoreError> {
    let api_key = spec
        .agent
        .api_key
        .clone()
        .ok_or_else(|| CoreError::Adapter("openai_chat api_key is required".into()))?;
    let model_catalog = discover_models_for_agent(client, &spec.agent).await?;
    let model = spec
        .agent
        .model
        .clone()
        .or_else(|| spec.agent.models.first().cloned())
        .or_else(|| model_catalog.and_then(|catalog| catalog.selected_model))
        .ok_or_else(|| {
            CoreError::Adapter(format!(
                "no model configured or discovered for agent `{}`",
                spec.agent.name
            ))
        })?;
    let session_id = format!("{}-{}", spec.agent.name, Uuid::new_v4());
    polyphony_agent_common::emit(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some("openai chat request started".into()),
        Some(session_id.clone()),
        None,
        None,
        None,
    );
    polyphony_agent_common::emit(
        &event_tx,
        &spec,
        AgentEventKind::TurnStarted,
        Some("turn started".into()),
        Some(session_id.clone()),
        None,
        None,
        None,
    );

    let base_url = resolve_base_url(&spec.agent);
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut messages = vec![json!({
        "role": "user",
        "content": spec.prompt,
    })];
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        provider_kind = %spec.agent.kind,
        model,
        base_url = %base_url,
        session_id = %session_id,
        "starting OpenAI-compatible agent session"
    );

    for tool_round in 0..4 {
        debug!(
            issue_identifier = %spec.issue.identifier,
            agent_name = %spec.agent.name,
            session_id = %session_id,
            tool_round = tool_round + 1,
            message_count = messages.len(),
            "sending OpenAI-compatible completion request"
        );
        let response = client
            .post(&url)
            .bearer_auth(&api_key)
            .header("User-Agent", "polyphony")
            .json(&json!({
                "model": model,
                "messages": messages,
                "stream": true,
                "stream_options": {"include_usage": true},
            }))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let status = response.status();
        if status.as_u16() == 429 {
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = %session_id,
                "OpenAI-compatible agent turn hit rate limit"
            );
            return Err(CoreError::RateLimited(Box::new(RateLimitSignal {
                component: format!("agent:{}", spec.agent.name),
                reason: "openai_chat_429".into(),
                limited_at: Utc::now(),
                retry_after_ms: None,
                reset_at: None,
                status_code: Some(429),
                raw: None,
            })));
        }
        if !status.is_success() {
            let payload = response
                .text()
                .await
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = %session_id,
                status = %status,
                "OpenAI-compatible agent turn failed"
            );
            return Err(CoreError::Adapter(format!(
                "openai_chat failed with status {status}: {payload}"
            )));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        let turn = if content_type.contains("text/event-stream") {
            consume_sse(response, &spec, &event_tx, &session_id).await?
        } else {
            consume_json(response, &spec, &event_tx, &session_id).await?
        };

        if !turn.text.is_empty() {
            polyphony_agent_common::emit(
                &event_tx,
                &spec,
                AgentEventKind::Notification,
                Some(turn.text.clone()),
                Some(session_id.clone()),
                None,
                None,
                None,
            );
        }
        if let Some(usage) = turn.usage.clone() {
            polyphony_agent_common::emit(
                &event_tx,
                &spec,
                AgentEventKind::UsageUpdated,
                Some("usage updated".into()),
                Some(session_id.clone()),
                Some(usage),
                None,
                None,
            );
        }

        if turn.tool_calls.is_empty() {
            polyphony_agent_common::emit(
                &event_tx,
                &spec,
                AgentEventKind::TurnCompleted,
                Some("turn completed".into()),
                Some(session_id.clone()),
                None,
                None,
                None,
            );
            info!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = %session_id,
                "OpenAI-compatible agent turn completed"
            );
            return Ok(AgentRunResult::succeeded(1));
        }

        messages.push(json!({
            "role": "assistant",
            "content": turn.text,
            "tool_calls": turn.tool_calls,
        }));
        for call in turn.tool_calls {
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unsupported-tool");
            let call_name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            polyphony_agent_common::emit(
                &event_tx,
                &spec,
                AgentEventKind::ToolCallStarted,
                Some(format!("dynamic tool call requested ({call_name})")),
                Some(session_id.clone()),
                None,
                None,
                Some(call.clone()),
            );
            let result = execute_openai_tool_call(
                tool_executor,
                &spec,
                call_name,
                call_id,
                call.pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}"),
                &event_tx,
                &session_id,
                &call,
            )
            .await;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result,
            }));
        }
    }

    warn!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_id = %session_id,
        "OpenAI-compatible tool loop exhausted"
    );
    Err(CoreError::Adapter(
        "openai tool loop exhausted without a terminal response".into(),
    ))
}

async fn execute_openai_tool_call(
    tool_executor: Option<&Arc<dyn ToolExecutor>>,
    spec: &AgentRunSpec,
    call_name: &str,
    call_id: &str,
    raw_arguments: &str,
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    session_id: &str,
    raw_call: &Value,
) -> String {
    let Some(executor) = tool_executor else {
        polyphony_agent_common::emit(
            event_tx,
            spec,
            AgentEventKind::ToolCallFailed,
            Some(format!("unsupported tool call requested: {call_name}")),
            Some(session_id.to_string()),
            None,
            None,
            Some(raw_call.clone()),
        );
        return "{\"success\":false,\"error\":\"unsupported_tool_call\"}".into();
    };
    let arguments = serde_json::from_str::<Value>(raw_arguments).unwrap_or_else(|_| json!({}));
    let supported = executor
        .list_tools(&spec.agent.name)
        .into_iter()
        .any(|tool| tool.name == call_name);
    if !supported {
        polyphony_agent_common::emit(
            event_tx,
            spec,
            AgentEventKind::ToolCallFailed,
            Some(format!("unsupported tool call requested: {call_name}")),
            Some(session_id.to_string()),
            None,
            None,
            Some(raw_call.clone()),
        );
        return "{\"success\":false,\"error\":\"unsupported_tool_call\"}".into();
    }
    match executor
        .execute(ToolCallRequest {
            name: call_name.to_string(),
            arguments,
            issue: spec.issue.clone(),
            workspace_path: spec.workspace_path.clone(),
            agent_name: spec.agent.name.clone(),
            call_id: Some(call_id.to_string()),
            thread_id: None,
            turn_id: None,
        })
        .await
    {
        Ok(result) => {
            polyphony_agent_common::emit(
                event_tx,
                spec,
                if result.success {
                    AgentEventKind::ToolCallCompleted
                } else {
                    AgentEventKind::ToolCallFailed
                },
                Some(format!(
                    "dynamic tool call {} ({call_name})",
                    if result.success {
                        "completed"
                    } else {
                        "failed"
                    }
                )),
                Some(session_id.to_string()),
                None,
                None,
                Some(json!({
                    "tool": call_name,
                    "result": result,
                })),
            );
            json!({
                "success": result.success,
                "output": result.output,
                "contentItems": result.content_items,
            })
            .to_string()
        },
        Err(error) => {
            polyphony_agent_common::emit(
                event_tx,
                spec,
                AgentEventKind::ToolCallFailed,
                Some(format!("dynamic tool call failed ({call_name})")),
                Some(session_id.to_string()),
                None,
                None,
                Some(json!({
                    "tool": call_name,
                    "error": error.to_string(),
                })),
            );
            json!({
                "success": false,
                "output": error.to_string(),
                "contentItems": [{
                    "type": "inputText",
                    "text": error.to_string(),
                }]
            })
            .to_string()
        },
    }
}

#[derive(Default)]
struct StreamedTurn {
    text: String,
    usage: Option<TokenUsage>,
    tool_calls: Vec<Value>,
}

async fn consume_json(
    response: reqwest::Response,
    _spec: &AgentRunSpec,
    _event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    _session_id: &str,
) -> Result<StreamedTurn, CoreError> {
    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    Ok(StreamedTurn {
        text: payload["choices"][0]["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned)
            .or_else(|| payload["output_text"].as_str().map(ToOwned::to_owned))
            .unwrap_or_default(),
        usage: parse_openai_usage(&payload),
        tool_calls: payload["choices"][0]["message"]["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    })
}

async fn consume_sse(
    response: reqwest::Response,
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    session_id: &str,
) -> Result<StreamedTurn, CoreError> {
    let mut turn = StreamedTurn::default();
    let mut tool_builders: BTreeMap<usize, Value> = BTreeMap::new();
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| CoreError::Adapter(error.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();
            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }
            let data = line.trim_start_matches("data:").trim();
            if data == "[DONE]" {
                break;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            if let Some(delta) = value
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                turn.text.push_str(delta);
                polyphony_agent_common::emit(
                    event_tx,
                    spec,
                    AgentEventKind::Notification,
                    Some(delta.to_string()),
                    Some(session_id.to_string()),
                    None,
                    None,
                    Some(value.clone()),
                );
            }
            if let Some(usage) = parse_openai_usage(&value) {
                turn.usage = Some(usage);
            }
            if let Some(tool_calls) = value
                .pointer("/choices/0/delta/tool_calls")
                .and_then(Value::as_array)
            {
                for entry in tool_calls {
                    let index = entry.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let builder = tool_builders.entry(index).or_insert_with(|| json!({
                        "id": entry.get("id").cloned().unwrap_or(Value::Null),
                        "type": entry.get("type").cloned().unwrap_or(Value::String("function".into())),
                        "function": {
                            "name": entry.pointer("/function/name").cloned().unwrap_or(Value::String(String::new())),
                            "arguments": entry.pointer("/function/arguments").cloned().unwrap_or(Value::String(String::new())),
                        }
                    }));
                    if builder.get("id").is_some_and(Value::is_null) {
                        builder["id"] = entry.get("id").cloned().unwrap_or(Value::Null);
                    }
                    if let Some(name) = entry.pointer("/function/name").and_then(Value::as_str) {
                        builder["function"]["name"] = Value::String(name.to_string());
                    }
                    if let Some(args_delta) =
                        entry.pointer("/function/arguments").and_then(Value::as_str)
                    {
                        let existing = builder["function"]["arguments"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        builder["function"]["arguments"] =
                            Value::String(format!("{existing}{args_delta}"));
                    }
                }
            }
        }
    }
    turn.tool_calls = tool_builders.into_values().collect();
    Ok(turn)
}

fn parse_openai_usage(payload: &Value) -> Option<TokenUsage> {
    let usage = payload.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("input_tokens").and_then(Value::as_u64))
            .unwrap_or_default(),
        output_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
            .unwrap_or_default(),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use polyphony_core::{
        AgentDefinition, AgentEventKind, AgentProviderRuntime, AgentRunSpec, AgentTransport,
        Error as CoreError, Issue, ToolCallRequest, ToolCallResult, ToolExecutor, ToolSpec,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc,
    };

    use super::{OpenAiRuntime, parse_openai_usage};

    struct MockToolExecutor;

    #[async_trait]
    impl ToolExecutor for MockToolExecutor {
        fn list_tools(&self, _agent_name: &str) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: "linear_graphql".into(),
                description: "test".into(),
                input_schema: serde_json::json!({}),
            }]
        }

        async fn execute(&self, request: ToolCallRequest) -> Result<ToolCallResult, CoreError> {
            assert_eq!(request.name, "linear_graphql");
            Ok(ToolCallResult::new(
                true,
                "{\"data\":{\"viewer\":{\"id\":\"usr_123\"}}}",
                vec![serde_json::json!({
                    "type": "inputText",
                    "text": "{\"data\":{\"viewer\":{\"id\":\"usr_123\"}}}",
                })],
            ))
        }
    }

    #[test]
    fn parses_openai_usage_payload() {
        let usage = parse_openai_usage(&serde_json::json!({
            "usage": {"prompt_tokens": 12, "completion_tokens": 8, "total_tokens": 20}
        }))
        .unwrap();
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(usage.total_tokens, 20);
    }

    #[tokio::test]
    async fn discovers_models_from_command() {
        let runtime = OpenAiRuntime::default();
        let catalogs = runtime
            .discover_models(&AgentDefinition {
                name: "openai".into(),
                kind: "openai".into(),
                transport: polyphony_core::AgentTransport::OpenAiChat,
                models_command: Some("printf '[\"gpt-4.1\",\"gpt-4.1-mini\"]'".into()),
                fetch_models: true,
                ..AgentDefinition::default()
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(catalogs.models.len(), 2);
        assert_eq!(catalogs.models[0].id, "gpt-4.1");
    }

    #[tokio::test]
    async fn missing_api_key_skips_http_model_discovery_when_model_is_configured() {
        let runtime = OpenAiRuntime::default();
        let catalog = runtime
            .discover_models(&AgentDefinition {
                name: "kimi_fast".into(),
                kind: "kimi".into(),
                transport: AgentTransport::OpenAiChat,
                model: Some("kimi-2.5".into()),
                fetch_models: true,
                ..AgentDefinition::default()
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(catalog.agent_name, "kimi_fast");
        assert_eq!(catalog.selected_model.as_deref(), Some("kimi-2.5"));
        assert!(catalog.models.is_empty());
    }

    #[tokio::test]
    async fn openai_runner_handles_tool_loop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in [
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": "",
                            "tool_calls": [{
                                "id": "call_1",
                                "type": "function",
                                "function": {"name": "unknown_tool", "arguments": "{}"}
                            }]
                        }
                    }]
                }),
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": "done"
                        }
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                }),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let _ = socket.read(&mut request).await.unwrap();
                let payload = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let runtime = OpenAiRuntime::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
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
                        name: "openai".into(),
                        kind: "openai".into(),
                        transport: AgentTransport::OpenAiChat,
                        base_url: Some(format!("http://{addr}/v1")),
                        api_key: Some("test-key".into()),
                        model: Some("gpt-4.1".into()),
                        fetch_models: false,
                        turn_timeout_ms: 5_000,
                        read_timeout_ms: 1_000,
                        stall_timeout_ms: 60_000,
                        idle_timeout_ms: 1_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await
            .unwrap();

        assert!(matches!(
            result.status,
            polyphony_core::AttemptStatus::Succeeded
        ));
        let mut saw_tool_warning = false;
        while let Ok(event) = rx.try_recv() {
            if event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("unsupported tool call requested"))
            {
                saw_tool_warning = true;
            }
        }
        assert!(saw_tool_warning);
    }

    #[tokio::test]
    async fn openai_runner_executes_supported_tools() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in [
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": "",
                            "tool_calls": [{
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "linear_graphql",
                                    "arguments": "{\"query\":\"query Viewer { viewer { id } }\"}"
                                }
                            }]
                        }
                    }]
                }),
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": "done"
                        }
                    }]
                }),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let _ = socket.read(&mut request).await.unwrap();
                let payload = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let runtime = OpenAiRuntime::new(Some(Arc::new(MockToolExecutor)));
        let (tx, mut rx) = mpsc::unbounded_channel();
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
                        name: "openai".into(),
                        kind: "openai".into(),
                        transport: AgentTransport::OpenAiChat,
                        base_url: Some(format!("http://{addr}/v1")),
                        api_key: Some("test-key".into()),
                        model: Some("gpt-4.1".into()),
                        fetch_models: false,
                        turn_timeout_ms: 5_000,
                        read_timeout_ms: 1_000,
                        stall_timeout_ms: 60_000,
                        idle_timeout_ms: 1_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await
            .unwrap();

        assert!(matches!(
            result.status,
            polyphony_core::AttemptStatus::Succeeded
        ));
        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, AgentEventKind::ToolCallCompleted))
        );
    }
}
