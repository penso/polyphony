use std::{path::PathBuf, process::Stdio, time::Duration};

use {
    async_trait::async_trait,
    polyphony_agent_common::{
        discover_models_from_command, emit, extract_text_rate_limit_signal, fetch_budget_for_agent,
        sanitize_session_fragment, shell_escape,
    },
    polyphony_core::{
        AgentDefinition, AgentEventKind, AgentModelCatalog, AgentProviderRuntime, AgentRunResult,
        AgentRunSpec, AgentSession, AgentTransport, AttemptStatus, BudgetSnapshot,
        Error as CoreError,
    },
    serde_json::Value,
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::{Child, Command},
        sync::mpsc,
        time::Instant,
    },
    tracing::{debug, info, warn},
};

#[derive(Debug, Default, Clone)]
pub struct AcpxRuntime;

struct AcpxSession {
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    base_command: String,
    agent_alias: String,
    session_name: String,
    workspace_path: PathBuf,
}

enum AcpxPromptEvent {
    Output(String),
    Thought(String),
    Tool(String),
    Status(String),
    Done,
    Error(String),
}

#[async_trait]
impl AgentProviderRuntime for AcpxRuntime {
    fn runtime_key(&self) -> String {
        "agent:acpx".into()
    }

    fn supports(&self, agent: &AgentDefinition) -> bool {
        matches!(agent.transport, AgentTransport::Acpx)
    }

    async fn start_session(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, CoreError> {
        Ok(Some(Box::new(launch_acpx_session(spec, event_tx).await?)))
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        let prompt = spec.prompt.clone();
        let mut session = launch_acpx_session(spec, event_tx).await?;
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
        discover_models_from_command(agent).await
    }
}

async fn launch_acpx_session(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
) -> Result<AcpxSession, CoreError> {
    let base_command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("acpx command is required".into()))?;
    let agent_alias = resolve_acpx_agent_alias(&spec.agent);
    let session_name = format!(
        "{}-{}-{}",
        spec.agent.name,
        sanitize_session_fragment(&spec.issue.identifier),
        spec.attempt.unwrap_or(0)
    );

    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_name = %session_name,
        agent_alias = %agent_alias,
        "ensuring acpx session"
    );
    run_acpx_control_command(
        &spec,
        &base_command,
        &agent_alias,
        &spec.workspace_path,
        &["sessions", "ensure", "--name", &session_name],
        None,
    )
    .await?;

    emit(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some("acpx session started".into()),
        Some(session_name.clone()),
        None,
        None,
        None,
    );

    Ok(AcpxSession {
        workspace_path: spec.workspace_path.clone(),
        spec,
        event_tx,
        base_command,
        agent_alias,
        session_name,
    })
}

#[async_trait]
impl AgentSession for AcpxSession {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, CoreError> {
        emit(
            &self.event_tx,
            &self.spec,
            AgentEventKind::TurnStarted,
            Some("turn started".into()),
            Some(self.session_name.clone()),
            None,
            None,
            None,
        );
        info!(
            issue_identifier = %self.spec.issue.identifier,
            agent_name = %self.spec.agent.name,
            session_name = %self.session_name,
            agent_alias = %self.agent_alias,
            "starting acpx turn"
        );

        let mut child = spawn_acpx_child(
            &self.spec,
            &self.base_command,
            &self.agent_alias,
            &self.workspace_path,
            &["prompt", "--session", &self.session_name, "--file", "-"],
        )?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            stdin
                .shutdown()
                .await
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Adapter("acpx stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CoreError::Adapter("acpx stderr unavailable".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        let stderr_task = tokio::spawn(async move {
            let mut stderr_lines = BufReader::new(stderr).lines();
            let mut buffer = String::new();
            loop {
                match stderr_lines.next_line().await {
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
        });
        let mut transcript = String::new();
        let deadline = Instant::now() + Duration::from_millis(self.spec.agent.turn_timeout_ms);
        let mut saw_done = false;
        let mut error_message: Option<String> = None;

        loop {
            let next_line = tokio::time::timeout_at(deadline, lines.next_line())
                .await
                .map_err(|_| CoreError::Adapter("turn_timeout".into()))?;
            let next_line = next_line.map_err(|error| CoreError::Adapter(error.to_string()))?;
            let Some(line) = next_line else {
                break;
            };
            transcript.push_str(&line);
            transcript.push('\n');
            let Some(event) = parse_acpx_prompt_event_line(&line) else {
                continue;
            };
            match event {
                AcpxPromptEvent::Output(text) => emit(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(text),
                    Some(self.session_name.clone()),
                    None,
                    None,
                    None,
                ),
                AcpxPromptEvent::Thought(text) | AcpxPromptEvent::Status(text) => emit(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::OtherMessage,
                    Some(text),
                    Some(self.session_name.clone()),
                    None,
                    None,
                    None,
                ),
                AcpxPromptEvent::Tool(text) => emit(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(text),
                    Some(self.session_name.clone()),
                    None,
                    None,
                    None,
                ),
                AcpxPromptEvent::Done => {
                    saw_done = true;
                },
                AcpxPromptEvent::Error(message) => {
                    error_message = Some(message);
                },
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let stderr = stderr_task
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if let Some(signal) = extract_text_rate_limit_signal(&self.spec, &transcript) {
            warn!(
                issue_identifier = %self.spec.issue.identifier,
                agent_name = %self.spec.agent.name,
                reason = %signal.reason,
                "acpx turn hit a local rate limit"
            );
            return Err(CoreError::RateLimited(Box::new(signal)));
        }

        if let Some(message) = error_message {
            emit(
                &self.event_tx,
                &self.spec,
                AgentEventKind::TurnFailed,
                Some(message.clone()),
                Some(self.session_name.clone()),
                None,
                None,
                None,
            );
            return Ok(AgentRunResult {
                status: AttemptStatus::Failed,
                turns_completed: 0,
                error: Some(message),
                final_issue_state: None,
            });
        }

        if !status.success() {
            let error = if status.code() == Some(127) {
                "acpx_not_found".to_string()
            } else if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                format!("acpx exited with status {}", status.code().unwrap_or(-1))
            };
            emit(
                &self.event_tx,
                &self.spec,
                AgentEventKind::TurnFailed,
                Some(error.clone()),
                Some(self.session_name.clone()),
                None,
                None,
                None,
            );
            return Err(CoreError::Adapter(error));
        }

        if !saw_done {
            debug!(
                issue_identifier = %self.spec.issue.identifier,
                agent_name = %self.spec.agent.name,
                session_name = %self.session_name,
                "acpx turn completed without explicit done event"
            );
        }
        emit(
            &self.event_tx,
            &self.spec,
            AgentEventKind::TurnCompleted,
            Some("turn completed".into()),
            Some(self.session_name.clone()),
            None,
            None,
            None,
        );
        Ok(AgentRunResult::succeeded(1))
    }

    async fn stop(&mut self) -> Result<(), CoreError> {
        let _ = run_acpx_control_command(
            &self.spec,
            &self.base_command,
            &self.agent_alias,
            &self.workspace_path,
            &["cancel", "--session", &self.session_name],
            Some("cancel"),
        )
        .await;
        let _ = run_acpx_control_command(
            &self.spec,
            &self.base_command,
            &self.agent_alias,
            &self.workspace_path,
            &["sessions", "close", &self.session_name],
            Some("close"),
        )
        .await;
        Ok(())
    }
}

fn resolve_acpx_agent_alias(agent: &AgentDefinition) -> String {
    agent.kind.trim().to_lowercase()
}

fn build_acpx_shell_command(
    base_command: &str,
    agent_alias: &str,
    workspace_path: &std::path::Path,
    args: &[&str],
) -> String {
    let mut command = String::new();
    command.push_str(base_command);
    command.push_str(" --format json --json-strict --cwd ");
    command.push_str(&shell_escape(workspace_path.to_string_lossy().as_ref()));
    command.push(' ');
    command.push_str(&shell_escape(agent_alias));
    for arg in args {
        command.push(' ');
        command.push_str(&shell_escape(arg));
    }
    command
}

fn spawn_acpx_child(
    spec: &AgentRunSpec,
    base_command: &str,
    agent_alias: &str,
    workspace_path: &std::path::Path,
    args: &[&str],
) -> Result<Child, CoreError> {
    let command = build_acpx_shell_command(base_command, agent_alias, workspace_path, args);
    let mut child = Command::new("bash");
    child
        .arg("-lc")
        .arg(command)
        .current_dir(workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CLAUDECODE");
    for (key, value) in &spec.agent.env {
        child.env(key, value);
    }
    child
        .spawn()
        .map_err(|error| CoreError::Adapter(error.to_string()))
}

async fn run_acpx_control_command(
    spec: &AgentRunSpec,
    base_command: &str,
    agent_alias: &str,
    workspace_path: &std::path::Path,
    args: &[&str],
    action: Option<&str>,
) -> Result<Vec<Value>, CoreError> {
    let mut child = spawn_acpx_child(spec, base_command, agent_alias, workspace_path, args)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .shutdown()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let parsed = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    if output.status.success() {
        return Ok(parsed);
    }
    let label = action.unwrap_or("command");
    let error = if output.status.code() == Some(127) {
        "acpx_not_found".to_string()
    } else if let Some(message) = parsed.iter().find_map(parse_error_message) {
        message
    } else if !stderr.is_empty() {
        stderr
    } else {
        format!(
            "acpx {label} failed with status {}",
            output.status.code().unwrap_or(-1)
        )
    };
    Err(CoreError::Adapter(error))
}

fn parse_error_message(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    object
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn parse_acpx_prompt_event_line(line: &str) -> Option<AcpxPromptEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = match serde_json::from_str::<Value>(trimmed) {
        Ok(parsed) => parsed,
        Err(_) => return Some(AcpxPromptEvent::Status(trimmed.to_string())),
    };
    let object = parsed.as_object()?;
    let payload = if object.get("method").and_then(Value::as_str) == Some("session/update") {
        object.get("params")?.get("update")?.as_object()?.clone()
    } else {
        object.clone()
    };
    let tag = payload
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .or_else(|| payload.get("type").and_then(Value::as_str))
        .unwrap_or_default();

    match tag {
        "text" | "agent_message_chunk" => extract_text(&payload).map(AcpxPromptEvent::Output),
        "thought" | "agent_thought_chunk" => extract_text(&payload).map(AcpxPromptEvent::Thought),
        "tool_call" | "tool_call_update" => Some(AcpxPromptEvent::Tool(tool_summary(&payload))),
        "usage_update" => Some(AcpxPromptEvent::Status("usage updated".into())),
        "available_commands_update" => {
            Some(AcpxPromptEvent::Status("available commands updated".into()))
        },
        "current_mode_update" => payload
            .get("currentModeId")
            .and_then(Value::as_str)
            .map(|mode| AcpxPromptEvent::Status(format!("mode updated: {mode}"))),
        "config_option_update" => Some(AcpxPromptEvent::Status("config updated".into())),
        "session_info_update" | "plan" | "client_operation" | "update" => extract_text(&payload)
            .map(AcpxPromptEvent::Status)
            .or_else(|| Some(AcpxPromptEvent::Status(tag.replace('_', " ")))),
        "done" => Some(AcpxPromptEvent::Done),
        "error" => Some(AcpxPromptEvent::Error(
            payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("acpx runtime error")
                .to_string(),
        )),
        _ => None,
    }
}

fn extract_text(payload: &serde_json::Map<String, Value>) -> Option<String> {
    payload
        .get("content")
        .and_then(|content| {
            if let Some(text) = content.as_str() {
                return Some(text.to_string());
            }
            content
                .as_object()
                .and_then(|content| content.get("text").and_then(Value::as_str))
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            payload
                .get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            payload
                .get("summary")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            payload
                .get("update")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn tool_summary(payload: &serde_json::Map<String, Value>) -> String {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("tool call");
    let status = payload.get("status").and_then(Value::as_str);
    match status {
        Some(status) if !status.is_empty() => format!("{title} ({status})"),
        _ => title.to_string(),
    }
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
        super::AcpxRuntime,
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

    #[tokio::test]
    async fn acpx_runtime_runs_turn_and_streams_output() {
        let dir = tempdir().unwrap();
        let script = write_fake_acpx(dir.path(), "hello from acpx", 0);
        let runtime = AcpxRuntime;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut session = runtime
            .start_session(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: Some(0),
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "ignored".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "opus".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acpx,
                        command: Some(script.display().to_string()),
                        turn_timeout_ms: 5_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await
            .unwrap()
            .unwrap();

        let result = session.run_turn("hello".into()).await.unwrap();
        let _ = session.stop().await;

        assert!(matches!(
            result.status,
            polyphony_core::AttemptStatus::Succeeded
        ));
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| {
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("hello from acpx"))
        }));
    }

    #[tokio::test]
    async fn acpx_runtime_detects_rate_limit_messages() {
        let dir = tempdir().unwrap();
        let script = write_fake_acpx(
            dir.path(),
            "You've hit your limit · resets 2am (Europe/Lisbon)",
            0,
        );
        let runtime = AcpxRuntime;
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
                        name: "opus".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acpx,
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

    fn write_fake_acpx(dir: &Path, message: &str, exit_code: i32) -> PathBuf {
        let script_path = dir.join("fake-acpx.sh");
        let prompt_event = serde_json::json!({
            "type": "agent_message_chunk",
            "content": {
                "type": "text",
                "text": message,
            }
        })
        .to_string();
        let mut script = String::new();
        writeln!(&mut script, "#!/usr/bin/env bash").unwrap();
        writeln!(&mut script, "set -euo pipefail").unwrap();
        writeln!(&mut script, "cwd=''").unwrap();
        writeln!(&mut script, "while [[ $# -gt 0 ]]; do").unwrap();
        writeln!(&mut script, "  case \"$1\" in").unwrap();
        writeln!(&mut script, "    --format) shift 2 ;;").unwrap();
        writeln!(&mut script, "    --json-strict) shift ;;").unwrap();
        writeln!(&mut script, "    --cwd) cwd=\"$2\"; shift 2 ;;").unwrap();
        writeln!(
            &mut script,
            "    --approve-all|--approve-reads|--deny-all) shift ;;"
        )
        .unwrap();
        writeln!(
            &mut script,
            "    --non-interactive-permissions|--timeout|--ttl) shift 2 ;;"
        )
        .unwrap();
        writeln!(&mut script, "    *) agent=\"$1\"; shift; break ;;").unwrap();
        writeln!(&mut script, "  esac").unwrap();
        writeln!(&mut script, "done").unwrap();
        writeln!(&mut script, "verb=\"$1\"").unwrap();
        writeln!(&mut script, "shift").unwrap();
        writeln!(&mut script, "case \"$verb\" in").unwrap();
        writeln!(
            &mut script,
            "  sessions) if [[ \"$1\" == \"ensure\" ]]; then echo '{{\"agentSessionId\":\"sess-1\",\"acpxSessionId\":\"acpx-1\"}}'; exit 0; fi; exit 0 ;;"
        )
        .unwrap();
        writeln!(&mut script, "  prompt) cat >/dev/null").unwrap();
        writeln!(&mut script, "    cat <<'EOF'").unwrap();
        writeln!(&mut script, "{prompt_event}").unwrap();
        writeln!(&mut script, "EOF").unwrap();
        writeln!(&mut script, "    echo '{{\"type\":\"done\"}}'").unwrap();
        writeln!(&mut script, "    exit {exit_code} ;;").unwrap();
        writeln!(&mut script, "  cancel) exit 0 ;;").unwrap();
        writeln!(&mut script, "  *) exit 0 ;;").unwrap();
        writeln!(&mut script, "esac").unwrap();

        let mut file = std::fs::File::create(&script_path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).unwrap();
        script_path
    }

    #[tokio::test]
    async fn acpx_runtime_maps_stderr_failures() {
        let dir = tempdir().unwrap();
        let script = write_fake_acpx_failure(dir.path(), "permission denied by harness");
        let runtime = AcpxRuntime;
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
                        name: "opus".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acpx,
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
            CoreError::Adapter(message) => assert!(message.contains("permission denied")),
            other => panic!("expected adapter error, got {other:?}"),
        }
    }

    fn write_fake_acpx_failure(dir: &Path, stderr_message: &str) -> PathBuf {
        let script_path = dir.join("fake-acpx-failure.sh");
        let script = format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' \"{stderr_message}\" >&2\nexit 9\n"
        );
        let mut file = std::fs::File::create(&script_path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).unwrap();
        script_path
    }
}
