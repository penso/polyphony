use std::{process::Stdio, time::Duration};

use {
    async_trait::async_trait,
    chrono::Utc,
    polyphony_agent_common::{
        discover_models_from_command, emit, fetch_budget_for_agent, forward_reader_lines,
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
    tracing::{debug, info},
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

struct CodexAppServerSession {
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_forward: Option<JoinHandle<()>>,
    thread_id: String,
    next_request_id: u64,
    emitted_session_started: bool,
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
            &mut self.stdin,
            &mut self.lines,
            request_id,
        )
        .await?;
        let turn_id = turn_response["result"]["turn"]["id"]
            .as_str()
            .or_else(|| turn_response["result"]["id"].as_str())
            .ok_or_else(|| CoreError::Adapter("turn id missing".into()))?
            .to_string();
        let session_id = format!("{}-{}", self.thread_id, turn_id);
        if !self.emitted_session_started {
            emit(
                &self.event_tx,
                &self.spec,
                AgentEventKind::SessionStarted,
                Some("codex app-server session started".into()),
                Some(session_id.clone()),
                None,
                None,
                Some(turn_response.clone()),
            );
            self.emitted_session_started = true;
        }
        emit(
            &self.event_tx,
            &self.spec,
            AgentEventKind::TurnStarted,
            Some("turn started".into()),
            Some(session_id.clone()),
            None,
            None,
            Some(turn_response),
        );
        self.consume_turn_stream(session_id).await
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
    async fn consume_turn_stream(
        &mut self,
        session_id: String,
    ) -> Result<AgentRunResult, CoreError> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(self.spec.agent.turn_timeout_ms);
        loop {
            let next_line = tokio::time::timeout_at(deadline, self.lines.next_line())
                .await
                .map_err(|_| CoreError::Adapter("turn_timeout".into()))?;
            let Some(line) = next_line.map_err(|error| CoreError::Adapter(error.to_string()))?
            else {
                return Err(CoreError::Adapter("app-server closed stdout".into()));
            };
            let value = match serde_json::from_str::<Value>(&line) {
                Ok(value) => value,
                Err(error) => {
                    emit(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::OtherMessage,
                        Some(format!("malformed stdout JSON: {error}")),
                        Some(session_id.clone()),
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
                emit(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::UsageUpdated,
                    Some("usage updated".into()),
                    Some(session_id.clone()),
                    Some(usage),
                    extract_rate_limits(&value),
                    Some(value.clone()),
                );
            } else if let Some(message) = extract_message(&value) {
                emit(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(message),
                    Some(session_id.clone()),
                    None,
                    extract_rate_limits(&value),
                    Some(value.clone()),
                );
            }
            match value["method"].as_str() {
                Some("turn/completed") => {
                    emit(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::TurnCompleted,
                        Some("turn completed".into()),
                        Some(session_id.clone()),
                        None,
                        extract_rate_limits(&value),
                        Some(value),
                    );
                    debug!(
                        issue_identifier = %self.spec.issue.identifier,
                        session_id = %session_id,
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
                    emit(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::TurnFailed,
                        Some("turn failed".into()),
                        Some(session_id.clone()),
                        None,
                        extract_rate_limits(&value),
                        Some(value.clone()),
                    );
                    info!(
                        issue_identifier = %self.spec.issue.identifier,
                        session_id = %session_id,
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
                    emit(
                        &self.event_tx,
                        &self.spec,
                        AgentEventKind::TurnCancelled,
                        Some("turn cancelled".into()),
                        Some(session_id.clone()),
                        None,
                        extract_rate_limits(&value),
                        Some(value.clone()),
                    );
                    info!(
                        issue_identifier = %self.spec.issue.identifier,
                        session_id = %session_id,
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
                    return Err(CoreError::Adapter("turn_input_required".into()));
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
    .map_err(|error| CoreError::Adapter(error.to_string()))?;
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
    let stderr_forward = tokio::spawn(forward_reader_lines(
        BufReader::new(stderr),
        event_tx.clone(),
        spec.clone(),
        String::new(),
        "stderr",
    ));

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
        wait_for_response(&spec, &event_tx, &mut stdin, &mut lines, 1).await?;
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
        let thread_response =
            wait_for_response(&spec, &event_tx, &mut stdin, &mut lines, 2).await?;
        let thread_id = thread_response["result"]["thread"]["id"]
            .as_str()
            .or_else(|| thread_response["result"]["id"].as_str())
            .ok_or_else(|| CoreError::Adapter("thread id missing".into()))?
            .to_string();
        Ok::<String, CoreError>(thread_id)
    }
    .await;

    let thread_id = match handshake {
        Ok(thread_id) => thread_id,
        Err(error) => {
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
        next_request_id: 3,
        emitted_session_started: false,
    })
}

async fn wait_for_response(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value, CoreError> {
    loop {
        let next_line = tokio::time::timeout(
            Duration::from_millis(spec.agent.read_timeout_ms),
            lines.next_line(),
        )
        .await
        .map_err(|_| CoreError::Adapter("response_timeout".into()))?;
        let Some(line) = next_line.map_err(|error| CoreError::Adapter(error.to_string()))? else {
            return Err(CoreError::Adapter("app-server closed stdout".into()));
        };
        let value: Value =
            serde_json::from_str(&line).map_err(|error| CoreError::Adapter(error.to_string()))?;
        if maybe_auto_respond(stdin, &value).await? {
            continue;
        }
        if value["id"].as_u64() == Some(id) {
            return Ok(value);
        }
        if let Some(message) = extract_message(&value) {
            emit(
                event_tx,
                spec,
                AgentEventKind::Notification,
                Some(message),
                None,
                extract_usage(&value),
                extract_rate_limits(&value),
                Some(value),
            );
        }
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
    let bytes = serde_json::to_vec(value).map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .write_all(&bytes)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
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
    let usage = value
        .pointer("/params/usage")
        .or_else(|| value.pointer("/result/usage"))?;
    Some(TokenUsage {
        input_tokens: usage
            .get("input_tokens")
            .or_else(|| usage.get("inputTokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: usage
            .get("output_tokens")
            .or_else(|| usage.get("outputTokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        total_tokens: usage
            .get("total_tokens")
            .or_else(|| usage.get("totalTokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
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
        super::CodexRuntime,
        polyphony_core::{
            AgentDefinition, AgentProviderRuntime, AgentRunSpec, AgentTransport, Issue,
        },
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

        let (tx, _rx) = mpsc::unbounded_channel();
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
    }
}
