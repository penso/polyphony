use std::{process::Stdio, time::Duration};

use {
    async_trait::async_trait,
    chrono::Utc,
    polyphony_agent_common::{
        discover_models_from_command, emit_with_metadata, fetch_budget_for_agent,
        prepare_context_file, prepare_prompt_file, selected_model_hint, shell_command,
    },
    polyphony_core::{
        AgentEventKind, AgentProviderRuntime, AgentRunResult, AgentRunSpec, AgentSession,
        BudgetSnapshot, Error as CoreError, RateLimitSignal, TokenUsage,
    },
    serde_json::{Value, json},
    thiserror::Error,
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::{Child, ChildStdin, ChildStdout},
        sync::mpsc,
        task::JoinHandle,
    },
    tracing::{debug, info, warn},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("codex agent error: {0}")]
    Codex(String),
}

#[derive(Debug, Default, Clone)]
pub struct CodexRuntime;

#[async_trait]
impl AgentProviderRuntime for CodexRuntime {
    fn runtime_key(&self) -> String {
        "agent:codex".into()
    }

    fn supports(&self, agent: &polyphony_core::AgentDefinition) -> bool {
        agent.kind.eq_ignore_ascii_case("codex")
            || matches!(agent.transport, polyphony_core::AgentTransport::AppServer)
    }

    async fn start_session(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, CoreError> {
        Ok(Some(Box::new(launch_codex_session(spec, event_tx).await?)))
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        run_codex_app_server(spec, event_tx).await
    }

    async fn fetch_budget(
        &self,
        agent: &polyphony_core::AgentDefinition,
    ) -> Result<Option<BudgetSnapshot>, CoreError> {
        fetch_budget_for_agent(agent).await
    }

    async fn discover_models(
        &self,
        agent: &polyphony_core::AgentDefinition,
    ) -> Result<Option<polyphony_core::AgentModelCatalog>, CoreError> {
        let mut resolved = agent.clone();
        if resolved.models_command.is_none() && resolved.fetch_models {
            resolved.models_command = Some("codex models --json".into());
        }
        discover_models_from_command(&resolved).await
    }
}

async fn run_codex_app_server(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
) -> Result<AgentRunResult, CoreError> {
    let prompt = spec.prompt.clone();
    let mut session = launch_codex_session(spec, event_tx).await?;
    let result = session.run_turn(prompt).await;
    let _ = session.stop().await;
    result
}

fn adapter_error(kind: &'static str) -> CoreError {
    CoreError::Adapter(kind.into())
}

struct CodexAppServerSession {
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_forward: Option<JoinHandle<()>>,
    thread_id: String,
    codex_app_server_pid: Option<String>,
    next_request_id: u64,
    emitted_session_started: bool,
}

#[derive(Debug, Clone, Default)]
struct CodexEventMetadata {
    session_id: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    codex_app_server_pid: Option<String>,
}

#[async_trait]
impl AgentSession for CodexAppServerSession {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, CoreError> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let thread_id = self.thread_id.clone();
        let title = format!("{}: {}", self.spec.issue.identifier, self.spec.issue.title);
        let workspace_path = self.spec.workspace_path.clone();
        let approval_policy = self.spec.agent.approval_policy.clone();
        let sandbox_policy = self
            .spec
            .agent
            .turn_sandbox_policy
            .as_ref()
            .map(|policy| json!({"type": policy}));
        info!(
            issue_identifier = %self.spec.issue.identifier,
            thread_id = %self.thread_id,
            request_id,
            "starting codex app-server turn"
        );
        let request_metadata = self.event_metadata(None);
        write_json_line(
            &mut self.stdin,
            &json!({
                "id": request_id,
                "method": "turn/start",
                "params": {
                    "threadId": thread_id,
                    "input": [{"type": "text", "text": prompt}],
                    "cwd": workspace_path,
                    "title": title,
                    "approvalPolicy": approval_policy,
                    "sandboxPolicy": sandbox_policy,
                }
            }),
        )
        .await?;
        let turn_response = wait_for_response(
            &self.spec,
            &self.event_tx,
            &mut self.child,
            &mut self.stdin,
            &mut self.lines,
            request_id,
            &request_metadata,
        )
        .await?;
        let turn_id = turn_response["result"]["turn"]["id"]
            .as_str()
            .or_else(|| turn_response["result"]["id"].as_str())
            .ok_or_else(|| adapter_error("response_error"))?
            .to_string();
        let event_metadata = self.event_metadata(Some(turn_id.as_str()));
        if !self.emitted_session_started {
            emit_codex(
                &self.event_tx,
                &self.spec,
                AgentEventKind::SessionStarted,
                Some("codex app-server session started".into()),
                &event_metadata,
                None,
                None,
                Some(turn_response.clone()),
            );
            self.emitted_session_started = true;
        }
        emit_codex(
            &self.event_tx,
            &self.spec,
            AgentEventKind::TurnStarted,
            Some("turn started".into()),
            &event_metadata,
            None,
            None,
            Some(turn_response),
        );
        self.consume_turn_stream(event_metadata).await
    }

    async fn stop(&mut self) -> Result<(), CoreError> {
        info!(
            issue_identifier = %self.spec.issue.identifier,
            thread_id = %self.thread_id,
            "stopping codex app-server session"
        );
        best_effort_stop_child(&mut self.child).await;
        if let Some(stderr_forward) = self.stderr_forward.take() {
            let _ = stderr_forward.await;
        }
        Ok(())
    }
}

impl CodexAppServerSession {
    fn event_metadata(&self, turn_id: Option<&str>) -> CodexEventMetadata {
        let turn_id = turn_id.map(ToOwned::to_owned);
        CodexEventMetadata {
            session_id: turn_id
                .as_ref()
                .map(|turn_id| format!("{}-{turn_id}", self.thread_id)),
            thread_id: Some(self.thread_id.clone()),
            turn_id,
            codex_app_server_pid: self.codex_app_server_pid.clone(),
        }
    }

    async fn consume_turn_stream(
        &mut self,
        event_metadata: CodexEventMetadata,
    ) -> Result<AgentRunResult, CoreError> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(self.spec.agent.turn_timeout_ms);
        loop {
            let next_line = tokio::time::timeout_at(deadline, self.lines.next_line())
                .await
                .map_err(|_| adapter_error("turn_timeout"))?;
            let Some(line) = next_line.map_err(normalize_io_error)? else {
                return Err(classify_child_exit(&mut self.child));
            };
            let value = match serde_json::from_str::<Value>(&line) {
                Ok(value) => value,
                Err(error) => {
                    emit_codex(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::OtherMessage,
                        Some(format!("malformed stdout JSON: {error}")),
                        &event_metadata,
                        None,
                        None,
                        Some(json!({"line": line})),
                    );
                    continue;
                },
            };
            if maybe_auto_respond(&mut self.stdin, &value).await? {
                continue;
            }
            if let Some(signal) = extract_rate_limit_signal(&self.spec, &value) {
                return Err(CoreError::RateLimited(Box::new(signal)));
            }
            if let Some(usage) = extract_usage(&value) {
                emit_codex(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::UsageUpdated,
                    Some("usage updated".into()),
                    &event_metadata,
                    Some(usage),
                    extract_rate_limits(&value),
                    Some(value.clone()),
                );
            } else if let Some(message) = extract_message(&value) {
                emit_codex(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(message),
                    &event_metadata,
                    None,
                    extract_rate_limits(&value),
                    Some(value.clone()),
                );
            }
            match value["method"].as_str() {
                Some("turn/completed") => {
                    emit_codex(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::TurnCompleted,
                        Some("turn completed".into()),
                        &event_metadata,
                        None,
                        extract_rate_limits(&value),
                        Some(value),
                    );
                    debug!(
                        issue_identifier = %self.spec.issue.identifier,
                        session_id = event_metadata.session_id.as_deref().unwrap_or("unknown"),
                        "codex turn completed"
                    );
                    return Ok(AgentRunResult {
                        status: polyphony_core::AttemptStatus::Succeeded,
                        turns_completed: 1,
                        error: None,
                        final_issue_state: None,
                    });
                },
                Some("turn/failed") => {
                    emit_codex(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::TurnFailed,
                        Some("turn failed".into()),
                        &event_metadata,
                        None,
                        extract_rate_limits(&value),
                        Some(value.clone()),
                    );
                    info!(
                        issue_identifier = %self.spec.issue.identifier,
                        session_id = event_metadata.session_id.as_deref().unwrap_or("unknown"),
                        "codex turn failed"
                    );
                    return Ok(AgentRunResult {
                        status: polyphony_core::AttemptStatus::Failed,
                        turns_completed: 0,
                        error: extract_message(&value).or_else(|| Some("turn_failed".into())),
                        final_issue_state: None,
                    });
                },
                Some("turn/cancelled") => {
                    emit_codex(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::TurnCancelled,
                        Some("turn cancelled".into()),
                        &event_metadata,
                        None,
                        extract_rate_limits(&value),
                        Some(value.clone()),
                    );
                    info!(
                        issue_identifier = %self.spec.issue.identifier,
                        session_id = event_metadata.session_id.as_deref().unwrap_or("unknown"),
                        "codex turn cancelled"
                    );
                    return Ok(AgentRunResult {
                        status: polyphony_core::AttemptStatus::CancelledByReconciliation,
                        turns_completed: 0,
                        error: extract_message(&value).or_else(|| Some("turn_cancelled".into())),
                        final_issue_state: None,
                    });
                },
                Some(method)
                    if method.contains("requestUserInput") || method.contains("input_required") =>
                {
                    return Err(adapter_error("turn_input_required"));
                },
                _ => {},
            }
        }
    }
}

async fn launch_codex_session(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
) -> Result<CodexAppServerSession, CoreError> {
    let workspace_is_dir = tokio::fs::metadata(&spec.workspace_path)
        .await
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    if !workspace_is_dir {
        let startup_metadata = CodexEventMetadata::default();
        emit_startup_failed(&event_tx, &spec, &startup_metadata, "invalid_workspace_cwd");
        return Err(adapter_error("invalid_workspace_cwd"));
    }
    let command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("app_server command is required".into()))?;
    let prompt_file = prepare_prompt_file(&spec).await?;
    let context_file = prepare_context_file(&spec).await?;
    let model = selected_model_hint(&spec.agent);
    let mut child = shell_command(
        &command,
        &spec.workspace_path,
        &spec.agent.env,
        &spec,
        &prompt_file,
        context_file.as_deref(),
        model.as_deref(),
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|_| adapter_error("response_error"))?;
    let codex_app_server_pid = child.id().map(|pid| pid.to_string());
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Adapter("app-server stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Adapter("app-server stdout unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::Adapter("app-server stderr unavailable".into()))?;
    let mut lines = BufReader::new(stdout).lines();
    let stderr_forward = tokio::spawn(forward_codex_stderr(
        BufReader::new(stderr),
        event_tx.clone(),
        spec.clone(),
        codex_app_server_pid.clone(),
    ));
    let startup_metadata = CodexEventMetadata {
        codex_app_server_pid: codex_app_server_pid.clone(),
        ..CodexEventMetadata::default()
    };

    let handshake = async {
        write_json_line(
            &mut stdin,
            &json!({
                "id": 1u64,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "polyphony", "version": "0.1.0"},
                    "capabilities": {}
                }
            }),
        )
        .await?;
        wait_for_response(
            &spec,
            &event_tx,
            &mut child,
            &mut stdin,
            &mut lines,
            1,
            &startup_metadata,
        )
        .await?;
        write_json_line(&mut stdin, &json!({"method": "initialized", "params": {}})).await?;

        write_json_line(
            &mut stdin,
            &json!({
                "id": 2u64,
                "method": "thread/start",
                "params": {
                    "approvalPolicy": spec.agent.approval_policy,
                    "sandbox": spec.agent.thread_sandbox,
                    "cwd": spec.workspace_path,
                }
            }),
        )
        .await?;
        let thread_response = wait_for_response(
            &spec,
            &event_tx,
            &mut child,
            &mut stdin,
            &mut lines,
            2,
            &startup_metadata,
        )
        .await?;
        let thread_id = thread_response["result"]["thread"]["id"]
            .as_str()
            .or_else(|| thread_response["result"]["id"].as_str())
            .ok_or_else(|| adapter_error("response_error"))?
            .to_string();
        Ok::<String, CoreError>(thread_id)
    }
    .await;

    let thread_id = match handshake {
        Ok(thread_id) => thread_id,
        Err(error) => {
            warn!(
                issue_identifier = %spec.issue.identifier,
                %error,
                "codex app-server startup failed"
            );
            let message = normalized_error_message(&error);
            emit_startup_failed(&event_tx, &spec, &startup_metadata, &message);
            best_effort_stop_child(&mut child).await;
            let _ = stderr_forward.await;
            return Err(error);
        },
    };
    info!(
        issue_identifier = %spec.issue.identifier,
        thread_id = %thread_id,
        "codex app-server session established"
    );

    Ok(CodexAppServerSession {
        spec,
        event_tx,
        child,
        stdin,
        lines,
        stderr_forward: Some(stderr_forward),
        thread_id,
        codex_app_server_pid,
        next_request_id: 3,
        emitted_session_started: false,
    })
}

async fn wait_for_response(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    child: &mut Child,
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
    event_metadata: &CodexEventMetadata,
) -> Result<Value, CoreError> {
    loop {
        let next_line = tokio::time::timeout(
            Duration::from_millis(spec.agent.read_timeout_ms),
            lines.next_line(),
        )
        .await
        .map_err(|_| adapter_error("response_timeout"))?;
        let Some(line) = next_line.map_err(normalize_io_error)? else {
            return Err(classify_child_exit(child));
        };
        let value: Value =
            serde_json::from_str(&line).map_err(|_| adapter_error("response_error"))?;
        if maybe_auto_respond(stdin, &value).await? {
            continue;
        }
        if value["id"].as_u64() == Some(id) {
            if value.get("error").is_some() {
                return Err(adapter_error("response_error"));
            }
            return Ok(value);
        }
        if let Some(message) = extract_message(&value) {
            emit_codex(
                event_tx,
                spec,
                AgentEventKind::Notification,
                Some(message),
                event_metadata,
                extract_usage(&value),
                extract_rate_limits(&value),
                Some(value),
            );
        }
    }
}

fn emit_codex(
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    spec: &AgentRunSpec,
    kind: AgentEventKind,
    message: Option<String>,
    event_metadata: &CodexEventMetadata,
    usage: Option<TokenUsage>,
    rate_limits: Option<Value>,
    raw: Option<Value>,
) {
    emit_with_metadata(
        event_tx,
        spec,
        kind,
        message,
        event_metadata.session_id.clone(),
        event_metadata.thread_id.clone(),
        event_metadata.turn_id.clone(),
        event_metadata.codex_app_server_pid.clone(),
        usage,
        rate_limits,
        raw,
    );
}

fn emit_startup_failed(
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    spec: &AgentRunSpec,
    event_metadata: &CodexEventMetadata,
    message: &str,
) {
    emit_with_metadata(
        event_tx,
        spec,
        AgentEventKind::StartupFailed,
        Some(message.to_string()),
        event_metadata.session_id.clone(),
        event_metadata.thread_id.clone(),
        event_metadata.turn_id.clone(),
        event_metadata.codex_app_server_pid.clone(),
        None,
        None,
        None,
    );
}

fn normalize_io_error(error: std::io::Error) -> CoreError {
    match error.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof => {
            adapter_error("port_exit")
        },
        _ => adapter_error("response_error"),
    }
}

fn classify_child_exit(child: &mut Child) -> CoreError {
    match child.try_wait() {
        Ok(Some(status)) if status.code() == Some(127) => adapter_error("codex_not_found"),
        Ok(Some(_)) | Ok(None) | Err(_) => adapter_error("port_exit"),
    }
}

fn normalized_error_message(error: &CoreError) -> String {
    match error {
        CoreError::Adapter(message) => message.clone(),
        CoreError::RateLimited(_) => "rate_limited".into(),
        _ => error.to_string(),
    }
}

async fn forward_codex_stderr<R>(
    reader: BufReader<R>,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    spec: AgentRunSpec,
    codex_app_server_pid: Option<String>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        emit_with_metadata(
            &event_tx,
            &spec,
            AgentEventKind::Notification,
            Some(format!("stderr: {line}")),
            None,
            None,
            None,
            codex_app_server_pid.clone(),
            None,
            None,
            None,
        );
    }
}

async fn maybe_auto_respond(
    stdin: &mut tokio::process::ChildStdin,
    value: &Value,
) -> Result<bool, CoreError> {
    let Some(method) = value["method"].as_str() else {
        return Ok(false);
    };
    let Some(id) = value["id"].as_u64() else {
        return Ok(false);
    };
    if method.contains("approval") {
        write_json_line(stdin, &json!({"id": id, "result": {"approved": true}})).await?;
        return Ok(true);
    }
    if method.contains("item/tool/call") {
        write_json_line(
            stdin,
            &json!({"id": id, "result": {"success": false, "error": "unsupported_tool_call"}}),
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    value: &Value,
) -> Result<(), CoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| adapter_error("response_error"))?;
    stdin.write_all(&bytes).await.map_err(normalize_io_error)?;
    stdin.write_all(b"\n").await.map_err(normalize_io_error)?;
    stdin.flush().await.map_err(normalize_io_error)?;
    Ok(())
}

async fn best_effort_stop_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {},
        Ok(None) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        },
        Err(_) => {},
    }
}

fn extract_usage(value: &Value) -> Option<TokenUsage> {
    let absolute_usage = value
        .pointer("/params/token_counts/total_token_usage")
        .or_else(|| value.pointer("/params/token_counts/totalTokenUsage"))
        .or_else(|| value.pointer("/result/token_counts/total_token_usage"))
        .or_else(|| value.pointer("/result/token_counts/totalTokenUsage"))
        .or_else(|| value.pointer("/params/total_token_usage"))
        .or_else(|| value.pointer("/params/totalTokenUsage"))
        .or_else(|| value.pointer("/result/total_token_usage"))
        .or_else(|| value.pointer("/result/totalTokenUsage"))
        .or_else(|| value.pointer("/params/token_usage"))
        .or_else(|| value.pointer("/params/tokenUsage"))
        .or_else(|| value.pointer("/result/token_usage"))
        .or_else(|| value.pointer("/result/tokenUsage"));
    if let Some(usage) = absolute_usage {
        return parse_token_usage(usage);
    }

    if has_delta_only_usage(value) {
        return None;
    }

    if supports_generic_usage_totals(value) {
        return value
            .pointer("/params/usage")
            .or_else(|| value.pointer("/result/usage"))
            .and_then(parse_token_usage);
    }

    None
}

fn parse_token_usage(usage: &Value) -> Option<TokenUsage> {
    let input_tokens = token_count_field(usage, &["input_tokens", "inputTokens"]);
    let output_tokens = token_count_field(usage, &["output_tokens", "outputTokens"]);
    let total_tokens = token_count_field(usage, &["total_tokens", "totalTokens"])
        .or_else(|| Some(input_tokens.unwrap_or_default() + output_tokens.unwrap_or_default()));
    if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
        return None;
    }
    Some(TokenUsage {
        input_tokens: input_tokens.unwrap_or_default(),
        output_tokens: output_tokens.unwrap_or_default(),
        total_tokens: total_tokens.unwrap_or_default(),
    })
}

fn token_count_field(usage: &Value, field_names: &[&str]) -> Option<u64> {
    field_names
        .iter()
        .find_map(|field_name| usage.get(*field_name).and_then(Value::as_u64))
}

fn has_delta_only_usage(value: &Value) -> bool {
    value
        .pointer("/params/token_counts/last_token_usage")
        .is_some()
        || value
            .pointer("/params/token_counts/lastTokenUsage")
            .is_some()
        || value
            .pointer("/result/token_counts/last_token_usage")
            .is_some()
        || value
            .pointer("/result/token_counts/lastTokenUsage")
            .is_some()
        || value.pointer("/params/last_token_usage").is_some()
        || value.pointer("/params/lastTokenUsage").is_some()
        || value.pointer("/result/last_token_usage").is_some()
        || value.pointer("/result/lastTokenUsage").is_some()
}

fn supports_generic_usage_totals(value: &Value) -> bool {
    matches!(
        value["method"].as_str(),
        Some("turn/completed" | "turn/failed" | "turn/cancelled")
    ) || value
        .pointer("/result/turn/id")
        .and_then(Value::as_str)
        .is_some()
}

fn extract_rate_limits(value: &Value) -> Option<Value> {
    value
        .pointer("/params/rate_limits")
        .cloned()
        .or_else(|| value.pointer("/params/rateLimits").cloned())
        .or_else(|| value.pointer("/result/rate_limits").cloned())
        .or_else(|| value.pointer("/result/rateLimits").cloned())
}

fn extract_rate_limit_signal(spec: &AgentRunSpec, value: &Value) -> Option<RateLimitSignal> {
    let status_code = value
        .pointer("/error/status")
        .and_then(Value::as_u64)
        .or_else(|| value.pointer("/error/code").and_then(Value::as_u64))
        .or_else(|| value.pointer("/status").and_then(Value::as_u64))? as u16;
    if status_code != 429 {
        return None;
    }
    Some(RateLimitSignal {
        component: format!("agent:{}", spec.agent.name),
        reason: extract_message(value).unwrap_or_else(|| "agent rate limited".into()),
        limited_at: Utc::now(),
        retry_after_ms: None,
        reset_at: None,
        status_code: Some(status_code),
        raw: Some(value.clone()),
    })
}

fn extract_message(value: &Value) -> Option<String> {
    value
        .pointer("/params/message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .pointer("/params/text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            value
                .pointer("/result/message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use {
        super::{CodexRuntime, extract_usage},
        polyphony_core::{
            AgentDefinition, AgentEventKind, AgentProviderRuntime, AgentRunSpec, AgentTransport,
            Error as CoreError, Issue,
        },
        serde_json::json,
        tempfile::tempdir,
        tokio::sync::mpsc,
    };

    fn test_issue() -> Issue {
        Issue {
            id: "1".into(),
            identifier: "TEST-1".into(),
            title: "Test".into(),
            description: None,
            priority: None,
            state: "Todo".into(),
            branch_name: None,
            url: None,
            author: None,
            labels: Vec::new(),
            comments: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn app_server_runner_completes_handshake_and_tool_rejection() {
        let runtime = CodexRuntime;
        let dir = tempdir().unwrap();
        let script = dir.path().join("mock-app-server.sh");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env bash
while IFS= read -r line; do
  if [[ "$line" == *'"id":1'* ]]; then
    echo '{"id":1,"result":{"ok":true}}'
  elif [[ "$line" == *'"method":"thread/start"'* ]]; then
    echo '{"id":2,"result":{"thread":{"id":"thread-1"}}}'
  elif [[ "$line" == *'"method":"turn/start"'* ]]; then
    echo '{"id":3,"result":{"turn":{"id":"turn-1"}}}'
    echo '{"id":4,"method":"item/tool/call","params":{"name":"unsupported"}}'
    read -r tool_result
    if [[ "$tool_result" == *'unsupported_tool_call'* ]]; then
      echo '{"method":"turn/completed","params":{"message":"done","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}'
      exit 0
    fi
  fi
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: None,
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "codex".into(),
                        kind: "codex".into(),
                        transport: AgentTransport::AppServer,
                        command: Some(script.display().to_string()),
                        turn_timeout_ms: 2_000,
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
        assert_eq!(result.turns_completed, 1);
    }

    #[tokio::test]
    async fn app_server_session_reuses_same_thread_across_turns() {
        let runtime = CodexRuntime;
        let dir = tempdir().unwrap();
        let script = dir.path().join("mock-app-server.sh");
        let thread_count = dir.path().join("thread-count.txt");
        let turn_count = dir.path().join("turn-count.txt");
        std::fs::write(
            &script,
            format!(
                r#"#!/usr/bin/env bash
set -euo pipefail

thread_count=0
turn_count=0

while IFS= read -r line; do
  if [[ "$line" == *'"id":1'* ]]; then
    echo '{{"id":1,"result":{{"ok":true}}}}'
  elif [[ "$line" == *'"method":"thread/start"'* ]]; then
    thread_count=$((thread_count + 1))
    printf '%s' "$thread_count" > '{}'
    echo '{{"id":2,"result":{{"thread":{{"id":"thread-1"}}}}}}'
  elif [[ "$line" == *'"method":"turn/start"'* ]]; then
    if [[ "$line" != *'"threadId":"thread-1"'* ]]; then
      echo "unexpected thread id" >&2
      exit 9
    fi
    turn_count=$((turn_count + 1))
    printf '%s' "$turn_count" > '{}'
    echo '{{"id":'"$((turn_count + 2))"',"result":{{"turn":{{"id":"turn-'"$turn_count"'"}}}}}}'
    echo '{{"method":"turn/completed","params":{{"message":"done"}}}}'
  fi
done
"#,
                thread_count.display(),
                turn_count.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let spec = AgentRunSpec {
            issue: test_issue(),
            attempt: None,
            workspace_path: dir.path().to_path_buf(),
            prompt: "hello".into(),
            max_turns: 2,
            prior_context: None,
            agent: AgentDefinition {
                name: "codex".into(),
                kind: "codex".into(),
                transport: AgentTransport::AppServer,
                command: Some(script.display().to_string()),
                turn_timeout_ms: 2_000,
                read_timeout_ms: 1_000,
                stall_timeout_ms: 60_000,
                idle_timeout_ms: 1_000,
                ..AgentDefinition::default()
            },
        };

        let mut session = runtime.start_session(spec, tx).await.unwrap().unwrap();
        let first = session.run_turn("hello".into()).await.unwrap();
        let second = session.run_turn("continue".into()).await.unwrap();
        session.stop().await.unwrap();

        assert!(matches!(
            first.status,
            polyphony_core::AttemptStatus::Succeeded
        ));
        assert!(matches!(
            second.status,
            polyphony_core::AttemptStatus::Succeeded
        ));
        assert_eq!(std::fs::read_to_string(thread_count).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(turn_count).unwrap(), "2");

        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        let session_started = events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::SessionStarted))
            .collect::<Vec<_>>();
        let turn_started = events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnStarted))
            .collect::<Vec<_>>();
        assert_eq!(session_started.len(), 1);
        assert_eq!(turn_started.len(), 2);
        let pid = turn_started[0]
            .codex_app_server_pid
            .as_deref()
            .expect("expected codex app-server pid");
        assert_eq!(session_started[0].thread_id.as_deref(), Some("thread-1"));
        assert_eq!(session_started[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            session_started[0].session_id.as_deref(),
            Some("thread-1-turn-1")
        );
        assert_eq!(
            session_started[0].codex_app_server_pid.as_deref(),
            Some(pid)
        );
        assert_eq!(turn_started[0].thread_id.as_deref(), Some("thread-1"));
        assert_eq!(turn_started[1].thread_id.as_deref(), Some("thread-1"));
        assert_eq!(turn_started[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(turn_started[1].turn_id.as_deref(), Some("turn-2"));
        assert_eq!(
            turn_started[0].session_id.as_deref(),
            Some("thread-1-turn-1")
        );
        assert_eq!(
            turn_started[1].session_id.as_deref(),
            Some("thread-1-turn-2")
        );
        assert_eq!(turn_started[0].codex_app_server_pid.as_deref(), Some(pid));
        assert_eq!(turn_started[1].codex_app_server_pid.as_deref(), Some(pid));
    }

    #[tokio::test]
    async fn app_server_runner_normalizes_missing_codex_binary() {
        let runtime = CodexRuntime;
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: None,
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "codex".into(),
                        kind: "codex".into(),
                        transport: AgentTransport::AppServer,
                        command: Some("command_that_definitely_does_not_exist_12345".into()),
                        turn_timeout_ms: 2_000,
                        read_timeout_ms: 1_000,
                        stall_timeout_ms: 60_000,
                        idle_timeout_ms: 1_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await;

        match result {
            Err(CoreError::Adapter(message)) => assert_eq!(message, "codex_not_found"),
            other => panic!("expected codex_not_found, got {other:?}"),
        }

        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        let startup_failed = events
            .iter()
            .find(|event| matches!(event.kind, AgentEventKind::StartupFailed))
            .expect("expected startup failed event");
        assert_eq!(startup_failed.message.as_deref(), Some("codex_not_found"));
    }

    #[tokio::test]
    async fn app_server_runner_normalizes_invalid_handshake_json() {
        let runtime = CodexRuntime;
        let dir = tempdir().unwrap();
        let script = dir.path().join("mock-app-server.sh");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env bash
while IFS= read -r line; do
  if [[ "$line" == *'"id":1'* ]]; then
    echo 'not-json'
    exit 0
  fi
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: None,
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "codex".into(),
                        kind: "codex".into(),
                        transport: AgentTransport::AppServer,
                        command: Some(script.display().to_string()),
                        turn_timeout_ms: 2_000,
                        read_timeout_ms: 1_000,
                        stall_timeout_ms: 60_000,
                        idle_timeout_ms: 1_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await;

        match result {
            Err(CoreError::Adapter(message)) => assert_eq!(message, "response_error"),
            other => panic!("expected response_error, got {other:?}"),
        }

        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        let startup_failed = events
            .iter()
            .find(|event| matches!(event.kind, AgentEventKind::StartupFailed))
            .expect("expected startup failed event");
        assert_eq!(startup_failed.message.as_deref(), Some("response_error"));
    }

    #[test]
    fn extracts_usage_from_thread_token_usage_updates() {
        let usage = extract_usage(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "tokenUsage": {
                    "inputTokens": 21,
                    "outputTokens": 13,
                    "totalTokens": 34
                }
            }
        }))
        .expect("expected usage");

        assert_eq!(usage.input_tokens, 21);
        assert_eq!(usage.output_tokens, 13);
        assert_eq!(usage.total_tokens, 34);
    }

    #[test]
    fn extracts_total_token_usage_from_wrapper_payloads() {
        let usage = extract_usage(&json!({
            "method": "notification",
            "params": {
                "token_counts": {
                    "total_token_usage": {
                        "input_tokens": 55,
                        "output_tokens": 34,
                        "total_tokens": 89
                    },
                    "last_token_usage": {
                        "input_tokens": 3,
                        "output_tokens": 2,
                        "total_tokens": 5
                    }
                }
            }
        }))
        .expect("expected usage");

        assert_eq!(usage.input_tokens, 55);
        assert_eq!(usage.output_tokens, 34);
        assert_eq!(usage.total_tokens, 89);
    }

    #[test]
    fn ignores_delta_only_last_token_usage_payloads() {
        let usage = extract_usage(&json!({
            "method": "notification",
            "params": {
                "token_counts": {
                    "last_token_usage": {
                        "input_tokens": 3,
                        "output_tokens": 2,
                        "total_tokens": 5
                    }
                }
            }
        }));

        assert!(usage.is_none());
    }

    #[test]
    fn ignores_generic_usage_on_non_terminal_events() {
        let usage = extract_usage(&json!({
            "method": "notification",
            "params": {
                "usage": {
                    "input_tokens": 8,
                    "output_tokens": 5,
                    "total_tokens": 13
                }
            }
        }));

        assert!(usage.is_none());
    }
}
