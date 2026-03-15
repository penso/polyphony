use std::{path::PathBuf, process::Stdio};

use {
    async_trait::async_trait,
    chrono::Utc,
    polyphony_agent_common::{
        emit, extract_text_rate_limit_signal, fetch_budget_for_agent, sanitize_session_fragment,
        shell_escape,
    },
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentModel, AgentModelCatalog,
        AgentProviderRuntime, AgentRunResult, AgentRunSpec, AgentSession, AgentTransport,
        AttemptStatus, BudgetSnapshot, Error as CoreError, TokenUsage,
    },
    serde_json::{Value, json},
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::{Child, ChildStdin, ChildStdout, Command},
        sync::mpsc,
        task::JoinHandle,
        time::{Duration, Instant},
    },
    tracing::{debug, info},
};

#[derive(Debug, Default, Clone)]
pub struct PiRuntime;

struct PiSession {
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_forward: Option<JoinHandle<String>>,
    next_request_id: u64,
    session_id: Option<String>,
    session_name: Option<String>,
}

#[derive(Default)]
struct PiTurnState {
    transcript: String,
    last_assistant_error: Option<String>,
    last_assistant_usage: Option<TokenUsage>,
}

#[async_trait]
impl AgentProviderRuntime for PiRuntime {
    fn runtime_key(&self) -> String {
        "agent:pi".into()
    }

    fn supports(&self, agent: &AgentDefinition) -> bool {
        agent.kind.eq_ignore_ascii_case("pi") || matches!(agent.transport, AgentTransport::Rpc)
    }

    async fn start_session(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, CoreError> {
        Ok(Some(Box::new(launch_pi_session(spec, event_tx).await?)))
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        let prompt = spec.prompt.clone();
        let mut session = launch_pi_session(spec, event_tx).await?;
        let result = session.run_turn(prompt).await;
        let _ = session.stop().await;
        result
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
        discover_pi_models(agent).await
    }
}

#[async_trait]
impl AgentSession for PiSession {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, CoreError> {
        emit(
            &self.event_tx,
            &self.spec,
            AgentEventKind::TurnStarted,
            Some("turn started".into()),
            self.session_id.clone(),
            None,
            None,
            None,
        );
        info!(
            issue_identifier = %self.spec.issue.identifier,
            agent_name = %self.spec.agent.name,
            session_id = ?self.session_id,
            "starting pi rpc turn"
        );

        let request_id = self.next_request_id;
        self.next_request_id += 1;
        write_json_line(
            &mut self.stdin,
            &json!({
                "id": format!("req_{request_id}"),
                "type": "prompt",
                "message": prompt,
            }),
        )
        .await?;
        let deadline =
            Instant::now() + Duration::from_millis(self.spec.agent.turn_timeout_ms.max(1_000));
        wait_for_response(
            &self.spec,
            &self.event_tx,
            &mut self.child,
            &mut self.stdin,
            &mut self.lines,
            &mut self.stderr_forward,
            format!("req_{request_id}"),
            self.session_id.as_deref(),
            deadline,
        )
        .await?;

        let mut state = PiTurnState::default();
        loop {
            let line = next_line_with_timeout(&mut self.lines, deadline).await?;
            let Some(value) = parse_json_line(&line) else {
                continue;
            };
            if value.get("type").and_then(Value::as_str) == Some("response") {
                continue;
            }
            if let Some(result) = handle_pi_event(
                &self.spec,
                &self.event_tx,
                self.session_id.as_deref(),
                &value,
                &mut state,
            )? {
                return Ok(result);
            }
        }
    }

    async fn stop(&mut self) -> Result<(), CoreError> {
        if self.child.id().is_some() {
            let _ = write_json_line(
                &mut self.stdin,
                &json!({
                    "id": format!("req_{}", self.next_request_id),
                    "type": "abort",
                }),
            )
            .await;
            self.next_request_id += 1;
            let _ = self.child.start_kill();
        }
        let _ = self.child.wait().await;
        if let Some(stderr_forward) = self.stderr_forward.take() {
            let _ = stderr_forward.await;
        }
        Ok(())
    }
}

async fn launch_pi_session(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<PiSession, CoreError> {
    let session_root = spec.workspace_path.join(".polyphony").join("pi");
    tokio::fs::create_dir_all(&session_root)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let session_name = format!(
        "{}-{}-{}",
        spec.agent.name,
        sanitize_session_fragment(&spec.issue.identifier),
        spec.attempt.unwrap_or(0)
    );
    let session_dir = session_root.join(&session_name);
    tokio::fs::create_dir_all(&session_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;

    let mut child = spawn_pi_rpc_child(&spec, &session_dir)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Adapter("pi stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Adapter("pi stdout unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::Adapter("pi stderr unavailable".into()))?;
    let mut session = PiSession {
        spec,
        event_tx,
        child,
        stdin,
        lines: BufReader::new(stdout).lines(),
        stderr_forward: Some(tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut buffer = String::new();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if !buffer.is_empty() {
                            buffer.push('\n');
                        }
                        buffer.push_str(&line);
                    },
                    Ok(None) => break,
                    Err(error) => {
                        if !buffer.is_empty() {
                            buffer.push('\n');
                        }
                        buffer.push_str(&error.to_string());
                        break;
                    },
                }
            }
            buffer
        })),
        next_request_id: 1,
        session_id: None,
        session_name: Some(session_name.clone()),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let state = session.get_state(deadline).await?;
    session.session_id = state
        .get("data")
        .and_then(|data| data.get("sessionId"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    session.session_name = state
        .get("data")
        .and_then(|data| data.get("sessionName"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or(session.session_name);

    emit(
        &session.event_tx,
        &session.spec,
        AgentEventKind::SessionStarted,
        Some("pi rpc session started".into()),
        session.session_id.clone(),
        None,
        None,
        Some(state),
    );
    Ok(session)
}

impl PiSession {
    async fn get_state(&mut self, deadline: Instant) -> Result<Value, CoreError> {
        let request_id = format!("req_{}", self.next_request_id);
        self.next_request_id += 1;
        write_json_line(
            &mut self.stdin,
            &json!({
                "id": request_id,
                "type": "get_state",
            }),
        )
        .await?;
        wait_for_response(
            &self.spec,
            &self.event_tx,
            &mut self.child,
            &mut self.stdin,
            &mut self.lines,
            &mut self.stderr_forward,
            request_id,
            self.session_id.as_deref(),
            deadline,
        )
        .await
    }
}

fn spawn_pi_rpc_child(
    spec: &AgentRunSpec,
    session_dir: &std::path::Path,
) -> Result<Child, CoreError> {
    let base_command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("pi command is required".into()))?;
    let mut command = base_command;
    command.push_str(" --mode rpc");
    command.push_str(" --session-dir ");
    command.push_str(&shell_escape(session_dir.to_string_lossy().as_ref()));
    if let Some(model) = spec.agent.model.as_deref() {
        command.push_str(" --model ");
        command.push_str(&shell_escape(model));
    }
    let mut child = Command::new("bash");
    child
        .arg("-lc")
        .arg(command)
        .current_dir(&spec.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &spec.agent.env {
        child.env(key, value);
    }
    child
        .spawn()
        .map_err(|error| CoreError::Adapter(error.to_string()))
}

async fn discover_pi_models(
    agent: &AgentDefinition,
) -> Result<Option<AgentModelCatalog>, CoreError> {
    let base_command = agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("pi command is required".into()))?;
    let temp_dir = tempfile_dir_path("polyphony-pi-models");
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let mut child = {
        let mut command = base_command;
        command.push_str(" --mode rpc --session-dir ");
        command.push_str(&shell_escape(temp_dir.to_string_lossy().as_ref()));
        let mut child = Command::new("bash");
        child
            .arg("-lc")
            .arg(command)
            .current_dir(&temp_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &agent.env {
            child.env(key, value);
        }
        child
            .spawn()
            .map_err(|error| CoreError::Adapter(error.to_string()))?
    };
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Adapter("pi stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Adapter("pi stdout unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::Adapter("pi stderr unavailable".into()))?;
    let mut lines = BufReader::new(stdout).lines();
    let stderr_task = tokio::spawn(async move {
        let mut stderr_lines = BufReader::new(stderr).lines();
        let mut buffer = String::new();
        while let Ok(Some(line)) = stderr_lines.next_line().await {
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(&line);
        }
        buffer
    });
    write_json_line(
        &mut stdin,
        &json!({
            "id": "req_models",
            "type": "get_available_models",
        }),
    )
    .await?;
    let response = loop {
        let line =
            next_line_with_timeout(&mut lines, Instant::now() + Duration::from_secs(5)).await?;
        let Some(value) = parse_json_line(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("response")
            && value.get("id").and_then(Value::as_str) == Some("req_models")
        {
            break value;
        }
    };
    let _ = stdin
        .write_all(b"{\"id\":\"req_abort\",\"type\":\"abort\"}\n")
        .await;
    let _ = child.start_kill();
    let _ = child.wait().await;
    let _ = stderr_task.await;
    let models = response
        .get("data")
        .and_then(|data| data.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|model| {
            let provider = model.get("provider").and_then(Value::as_str)?;
            let id = model.get("id").and_then(Value::as_str)?;
            Some(AgentModel {
                id: format!("{provider}/{id}"),
                display_name: None,
                created_at: None,
            })
        })
        .collect();
    Ok(Some(AgentModelCatalog {
        agent_name: agent.name.clone(),
        provider_kind: agent.kind.clone(),
        fetched_at: Utc::now(),
        selected_model: agent.model.clone(),
        models,
    }))
}

async fn wait_for_response(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    child: &mut Child,
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_forward: &mut Option<JoinHandle<String>>,
    request_id: String,
    session_id: Option<&str>,
    deadline: Instant,
) -> Result<Value, CoreError> {
    loop {
        let line = next_line_with_timeout(lines, deadline).await?;
        let Some(value) = parse_json_line(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("response")
            && value.get("id").and_then(Value::as_str) == Some(request_id.as_str())
        {
            if value.get("success").and_then(Value::as_bool) == Some(true) {
                return Ok(value);
            }
            return Err(CoreError::Adapter(
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("pi rpc response failed")
                    .to_string(),
            ));
        }
        if let Some(result) = handle_pi_event(
            spec,
            event_tx,
            session_id,
            &value,
            &mut PiTurnState::default(),
        )? {
            let _ = write_json_line(
                stdin,
                &json!({
                    "id": format!("{request_id}_abort"),
                    "type": "abort",
                }),
            )
            .await;
            let _ = child.start_kill();
            if let Some(stderr_task) = stderr_forward.take() {
                let _ = stderr_task.await;
            }
            return Err(CoreError::Adapter(
                result
                    .error
                    .unwrap_or_else(|| "pi session ended early".into()),
            ));
        }
    }
}

fn handle_pi_event(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    session_id: Option<&str>,
    value: &Value,
    state: &mut PiTurnState,
) -> Result<Option<AgentRunResult>, CoreError> {
    match value.get("type").and_then(Value::as_str) {
        Some("message_update") => {
            if let Some(text) = value
                .get("assistantMessageEvent")
                .and_then(|event| {
                    (event.get("type").and_then(Value::as_str) == Some("text_delta"))
                        .then_some(event)
                })
                .and_then(|event| event.get("delta"))
                .and_then(Value::as_str)
                && !text.is_empty()
            {
                state.transcript.push_str(text);
                emit(
                    event_tx,
                    spec,
                    AgentEventKind::Notification,
                    Some(text.to_string()),
                    session_id.map(ToOwned::to_owned),
                    None,
                    None,
                    Some(value.clone()),
                );
            }
        },
        Some("message_end") => {
            if let Some(message) = value.get("message") {
                handle_message_end(spec, event_tx, session_id, message, value, state);
            }
        },
        Some("tool_execution_start") => {
            let name = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            emit(
                event_tx,
                spec,
                AgentEventKind::Notification,
                Some(format!("{name} started")),
                session_id.map(ToOwned::to_owned),
                None,
                None,
                Some(value.clone()),
            );
        },
        Some("tool_execution_end") => {
            let name = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let is_error = value
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            let message = if is_error {
                format!("{name} failed: {}", summarize_value(&result))
            } else {
                format!("{name} completed")
            };
            emit(
                event_tx,
                spec,
                if is_error {
                    AgentEventKind::OtherMessage
                } else {
                    AgentEventKind::Notification
                },
                Some(message),
                session_id.map(ToOwned::to_owned),
                None,
                None,
                Some(value.clone()),
            );
        },
        Some("agent_end") => {
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let combined = messages
                .iter()
                .filter_map(message_text)
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(signal) = extract_text_rate_limit_signal(spec, &combined) {
                return Err(CoreError::RateLimited(Box::new(signal)));
            }
            if let Some(error) = state.last_assistant_error.clone() {
                emit(
                    event_tx,
                    spec,
                    AgentEventKind::TurnFailed,
                    Some(error.clone()),
                    session_id.map(ToOwned::to_owned),
                    None,
                    None,
                    Some(value.clone()),
                );
                return Ok(Some(AgentRunResult {
                    status: AttemptStatus::Failed,
                    turns_completed: 0,
                    error: Some(error),
                    final_issue_state: None,
                }));
            }
            emit(
                event_tx,
                spec,
                AgentEventKind::TurnCompleted,
                Some("turn completed".into()),
                session_id.map(ToOwned::to_owned),
                state.last_assistant_usage.clone(),
                None,
                Some(value.clone()),
            );
            debug!(
                issue_identifier = %spec.issue.identifier,
                session_id = ?session_id,
                "pi rpc turn completed"
            );
            return Ok(Some(AgentRunResult::succeeded(1)));
        },
        _ => {},
    }
    Ok(None)
}

fn handle_message_end(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    session_id: Option<&str>,
    message: &Value,
    raw: &Value,
    state: &mut PiTurnState,
) {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if role != "assistant" {
        return;
    }
    let text = message_text(message).unwrap_or_default();
    if !text.is_empty() {
        state.transcript.push_str(&text);
        state.transcript.push('\n');
        emit(
            event_tx,
            spec,
            AgentEventKind::Notification,
            Some(text.clone()),
            session_id.map(ToOwned::to_owned),
            None,
            None,
            Some(raw.clone()),
        );
    }
    if let Some(usage) = message_usage(message) {
        state.last_assistant_usage = Some(usage.clone());
        emit(
            event_tx,
            spec,
            AgentEventKind::UsageUpdated,
            Some("usage updated".into()),
            session_id.map(ToOwned::to_owned),
            Some(usage),
            None,
            Some(raw.clone()),
        );
    }
    let stop_reason = message
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    if matches!(stop_reason, "error" | "aborted") {
        let error = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| (!text.is_empty()).then_some(text));
        state.last_assistant_error = error;
    }
}

fn message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    parts.push(text.to_string());
                }
            },
            Some("thinking") => {
                if let Some(thinking) = block.get("thinking").and_then(Value::as_str)
                    && !thinking.trim().is_empty()
                {
                    parts.push(thinking.to_string());
                }
            },
            Some("toolCall") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                parts.push(format!("[tool call: {name}]"));
            },
            _ => {},
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn message_usage(message: &Value) -> Option<TokenUsage> {
    let usage = message.get("usage")?;
    let input = usage
        .get("input")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let output = usage
        .get("output")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cache_read = usage
        .get("cacheRead")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cache_write = usage
        .get("cacheWrite")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: input + output + cache_read + cache_write,
    })
}

async fn next_line_with_timeout(
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    deadline: Instant,
) -> Result<String, CoreError> {
    let next_line = tokio::time::timeout_at(deadline, lines.next_line())
        .await
        .map_err(|_| CoreError::Adapter("turn_timeout".into()))?;
    let Some(line) = next_line.map_err(|error| CoreError::Adapter(error.to_string()))? else {
        return Err(CoreError::Adapter("pi rpc stream ended".into()));
    };
    Ok(line)
}

fn parse_json_line(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line).ok()
}

async fn write_json_line(stdin: &mut ChildStdin, value: &Value) -> Result<(), CoreError> {
    let line =
        serde_json::to_string(value).map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))
}

fn summarize_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "unavailable".into()),
    }
}

fn tempfile_dir_path(prefix: &str) -> PathBuf {
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    std::env::temp_dir().join(format!("{prefix}-{nanos}"))
}

#[cfg(test)]
mod tests {
    use std::{
        fmt::Write as _,
        io::Write as _,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use {
        super::PiRuntime,
        polyphony_core::{
            AgentDefinition, AgentProviderRuntime, AgentRunSpec, AgentTransport,
            Error as CoreError, Issue,
        },
        tempfile::tempdir,
        tokio::sync::mpsc,
    };

    fn test_issue() -> Issue {
        Issue {
            id: "1".into(),
            identifier: "TEST-1".into(),
            title: "Test".into(),
            state: "Todo".into(),
            ..Issue::default()
        }
    }

    #[test]
    fn supports_pi_kind() {
        let runtime = PiRuntime;
        assert!(runtime.supports(&AgentDefinition {
            kind: "pi".into(),
            ..AgentDefinition::default()
        }));
    }

    #[tokio::test]
    async fn pi_runtime_runs_turn_over_rpc() {
        let dir = tempdir().unwrap();
        let script = write_fake_pi(dir.path(), "hello from pi", None);
        let runtime = PiRuntime;
        let (tx, mut rx) = mpsc::unbounded_channel();

        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: Some(0),
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "pi".into(),
                        kind: "pi".into(),
                        transport: AgentTransport::Rpc,
                        command: Some(script.display().to_string()),
                        turn_timeout_ms: 5_000,
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
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| {
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("hello from pi"))
        }));
    }

    #[tokio::test]
    async fn pi_runtime_detects_rate_limit_messages() {
        let dir = tempdir().unwrap();
        let script = write_fake_pi(
            dir.path(),
            "You've hit your limit · resets 2am (Europe/Lisbon)",
            Some("error"),
        );
        let runtime = PiRuntime;
        let (tx, _rx) = mpsc::unbounded_channel();

        let error = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: Some(0),
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "pi".into(),
                        kind: "pi".into(),
                        transport: AgentTransport::Rpc,
                        command: Some(script.display().to_string()),
                        turn_timeout_ms: 5_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await
            .unwrap_err();

        match error {
            CoreError::RateLimited(signal) => assert!(signal.reason.contains("hit your limit")),
            other => panic!("expected rate limit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pi_runtime_discovers_models_via_rpc() {
        let dir = tempdir().unwrap();
        let script = write_fake_pi(dir.path(), "hello", None);
        let runtime = PiRuntime;
        let catalog = runtime
            .discover_models(&AgentDefinition {
                name: "pi".into(),
                kind: "pi".into(),
                transport: AgentTransport::Rpc,
                command: Some(script.display().to_string()),
                ..AgentDefinition::default()
            })
            .await
            .unwrap()
            .unwrap();

        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.id == "anthropic/claude-sonnet-4-5")
        );
    }

    fn write_fake_pi(dir: &Path, message: &str, stop_reason: Option<&str>) -> PathBuf {
        let script_path = dir.join("fake-pi.sh");
        let final_stop_reason = stop_reason.unwrap_or("stop");
        let message_update = serde_json::json!({
            "type": "message_update",
            "message": {
                "role": "assistant",
                "content": [],
            },
            "assistantMessageEvent": {
                "type": "text_delta",
                "delta": message,
            }
        })
        .to_string();
        let assistant_message = if final_stop_reason == "error" {
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": message}],
                "usage": {"input": 12, "output": 34, "cacheRead": 0, "cacheWrite": 0},
                "stopReason": final_stop_reason,
                "errorMessage": message,
            })
        } else {
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": message}],
                "usage": {"input": 12, "output": 34, "cacheRead": 0, "cacheWrite": 0},
                "stopReason": final_stop_reason,
            })
        };
        let message_end = serde_json::json!({
            "type": "message_end",
            "message": assistant_message.clone(),
        })
        .to_string();
        let agent_end = serde_json::json!({
            "type": "agent_end",
            "messages": [assistant_message],
        })
        .to_string();
        let mut script = String::new();
        writeln!(&mut script, "#!/usr/bin/env bash").unwrap();
        writeln!(&mut script, "set -euo pipefail").unwrap();
        writeln!(&mut script, "while IFS= read -r line; do").unwrap();
        writeln!(
            &mut script,
            "  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p')"
        )
        .unwrap();
        writeln!(
            &mut script,
            "  if [[ \"$line\" == *'\"type\":\"get_state\"'* ]]; then"
        )
        .unwrap();
        writeln!(
            &mut script,
            "    printf '%s\\n' '{{\"id\":\"'\"$id\"'\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{{\"sessionId\":\"pi-session-1\",\"sessionName\":\"polyphony\"}}}}'"
        )
        .unwrap();
        writeln!(
            &mut script,
            "  elif [[ \"$line\" == *'\"type\":\"get_available_models\"'* ]]; then"
        )
        .unwrap();
        writeln!(
            &mut script,
            "    printf '%s\\n' '{{\"id\":\"'\"$id\"'\",\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{{\"models\":[{{\"provider\":\"anthropic\",\"id\":\"claude-sonnet-4-5\",\"contextWindow\":200000,\"reasoning\":true}}]}}}}'"
        )
        .unwrap();
        writeln!(
            &mut script,
            "  elif [[ \"$line\" == *'\"type\":\"prompt\"'* ]]; then"
        )
        .unwrap();
        writeln!(
            &mut script,
            "    printf '%s\\n' '{{\"id\":\"'\"$id\"'\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}}'"
        )
        .unwrap();
        writeln!(&mut script, "    cat <<'EOF'").unwrap();
        writeln!(&mut script, "{message_update}").unwrap();
        writeln!(&mut script, "EOF").unwrap();
        writeln!(&mut script, "    cat <<'EOF'").unwrap();
        writeln!(&mut script, "{message_end}").unwrap();
        writeln!(&mut script, "EOF").unwrap();
        writeln!(&mut script, "    cat <<'EOF'").unwrap();
        writeln!(&mut script, "{agent_end}").unwrap();
        writeln!(&mut script, "EOF").unwrap();
        writeln!(
            &mut script,
            "  elif [[ \"$line\" == *'\"type\":\"abort\"'* ]]; then"
        )
        .unwrap();
        writeln!(
            &mut script,
            "    printf '%s\\n' '{{\"id\":\"'\"$id\"'\",\"type\":\"response\",\"command\":\"abort\",\"success\":true}}'"
        )
        .unwrap();
        writeln!(&mut script, "    exit 0").unwrap();
        writeln!(&mut script, "  fi").unwrap();
        writeln!(&mut script, "done").unwrap();

        let mut file = std::fs::File::create(&script_path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).unwrap();
        script_path
    }
}
