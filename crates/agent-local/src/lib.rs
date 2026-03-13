use std::time::Duration;

use {
    async_trait::async_trait,
    polyphony_agent_common::{
        base_agent_env, discover_models_from_command, emit, fetch_budget_for_agent,
        forward_reader_lines, prepare_context_file, prepare_prompt_file, sanitize_session_fragment,
        selected_model_hint, shell_command, shell_escape, status_to_result,
    },
    polyphony_core::{
        AgentEventKind, AgentInteractionMode, AgentPromptMode, AgentProviderRuntime,
        AgentRunResult, AgentRunSpec, BudgetSnapshot, Error as CoreError,
    },
    tokio::{fs, io::AsyncWriteExt, process::Command, sync::mpsc, time::Instant},
    tracing::{debug, info, warn},
    uuid::Uuid,
};

#[derive(Debug, Clone)]
pub struct LocalCliRuntime {
    supported_kinds: Vec<String>,
    fallback_transport: bool,
}

impl LocalCliRuntime {
    pub fn new(supported_kinds: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            supported_kinds: supported_kinds.into_iter().map(Into::into).collect(),
            fallback_transport: false,
        }
    }

    pub fn fallback_transport() -> Self {
        Self {
            supported_kinds: Vec::new(),
            fallback_transport: true,
        }
    }

    fn supports_kind(&self, kind: &str) -> bool {
        self.supported_kinds
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(kind))
    }
}

#[async_trait]
impl AgentProviderRuntime for LocalCliRuntime {
    fn runtime_key(&self) -> String {
        if self.fallback_transport {
            "agent:local-cli".into()
        } else {
            format!("agent:local-cli:{}", self.supported_kinds.join(","))
        }
    }

    fn supports(&self, agent: &polyphony_core::AgentDefinition) -> bool {
        if self.supports_kind(&agent.kind) {
            return true;
        }
        self.fallback_transport
            && matches!(agent.transport, polyphony_core::AgentTransport::LocalCli)
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        run_local_cli(spec, event_tx).await
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
        discover_models_from_command(agent).await
    }
}

pub async fn run_local_cli(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
) -> Result<AgentRunResult, CoreError> {
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        provider_kind = %spec.agent.kind,
        transport = if spec.agent.use_tmux { "tmux" } else { "stdio" },
        "starting local agent run"
    );
    if spec.agent.use_tmux {
        run_tmux(spec, event_tx).await
    } else {
        run_stdio(spec, event_tx).await
    }
}

async fn run_stdio(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
) -> Result<AgentRunResult, CoreError> {
    let command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("agent command is required".into()))?;
    let session_id = format!("{}-{}", spec.agent.name, Uuid::new_v4());
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_id = %session_id,
        command = %command,
        timeout_ms = spec.agent.turn_timeout_ms,
        "starting local stdio turn"
    );
    emit(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some("local CLI session started".into()),
        Some(session_id.clone()),
        None,
        None,
        None,
    );
    emit(
        &event_tx,
        &spec,
        AgentEventKind::TurnStarted,
        Some("turn started".into()),
        Some(session_id.clone()),
        None,
        None,
        None,
    );

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
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .stdin(std::process::Stdio::piped())
    .spawn()
    .map_err(|error| CoreError::Adapter(error.to_string()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Adapter("local cli stdin unavailable".into()))?;
    maybe_write_prompt(&spec, &prompt_file, &mut stdin).await?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Adapter("local cli stdout unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::Adapter("local cli stderr unavailable".into()))?;

    let stdout_handle = tokio::spawn(forward_reader_lines(
        tokio::io::BufReader::new(stdout),
        event_tx.clone(),
        spec.clone(),
        session_id.clone(),
        "stdout".into(),
    ));
    let stderr_handle = tokio::spawn(forward_reader_lines(
        tokio::io::BufReader::new(stderr),
        event_tx.clone(),
        spec.clone(),
        session_id.clone(),
        "stderr".into(),
    ));

    let status = match tokio::time::timeout(
        Duration::from_millis(spec.agent.turn_timeout_ms),
        child.wait(),
    )
    .await
    {
        Ok(wait_result) => wait_result.map_err(|error| CoreError::Adapter(error.to_string()))?,
        Err(_) => {
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = %session_id,
                timeout_ms = spec.agent.turn_timeout_ms,
                "local stdio turn timed out"
            );
            return Err(CoreError::Adapter("turn_timeout".into()));
        },
    };

    let _ = stdout_handle.await;
    let _ = stderr_handle.await;
    debug!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_id = %session_id,
        exit_code = status.code(),
        "local stdio turn completed"
    );
    Ok(status_to_result(
        &spec,
        &event_tx,
        Some(session_id),
        status.code(),
    ))
}

async fn run_tmux(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
) -> Result<AgentRunResult, CoreError> {
    let command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("agent command is required".into()))?;
    let prompt_file = prepare_prompt_file(&spec).await?;
    let context_file = prepare_context_file(&spec).await?;
    let run_dir = spec.workspace_path.join(".polyphony");
    fs::create_dir_all(&run_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let exit_path = run_dir.join(format!("{}-exit.txt", spec.agent.name));
    let session_name = format!(
        "{}-{}-{}",
        spec.agent
            .tmux_session_prefix
            .clone()
            .unwrap_or_else(|| spec.agent.name.clone()),
        sanitize_session_fragment(&spec.issue.identifier),
        spec.attempt.unwrap_or(0)
    );
    let wrapped = format!(
        "cd {} && {} ; code=$?; printf '%s' \"$code\" > {}",
        shell_escape(spec.workspace_path.to_string_lossy().as_ref()),
        command,
        shell_escape(exit_path.to_string_lossy().as_ref()),
    );
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_name = %session_name,
        timeout_ms = spec.agent.turn_timeout_ms,
        idle_timeout_ms = spec.agent.idle_timeout_ms,
        "starting tmux-backed agent turn"
    );

    let model = selected_model_hint(&spec.agent);
    let mut tmux = Command::new("tmux");
    tmux.arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&session_name)
        .arg("bash")
        .arg("-lc")
        .arg(wrapped);
    for (key, value) in base_agent_env(
        &spec,
        &prompt_file,
        context_file.as_deref(),
        model.as_deref(),
    ) {
        tmux.env(key, value);
    }
    for (key, value) in &spec.agent.env {
        tmux.env(key, value);
    }
    let status = tmux
        .status()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !status.success() {
        return Err(CoreError::Adapter("failed to create tmux session".into()));
    }

    emit(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some("tmux session started".into()),
        Some(session_name.clone()),
        None,
        None,
        None,
    );
    emit(
        &event_tx,
        &spec,
        AgentEventKind::TurnStarted,
        Some("turn started".into()),
        Some(session_name.clone()),
        None,
        None,
        None,
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    if matches!(
        spec.agent.interaction_mode,
        AgentInteractionMode::Interactive
    ) || matches!(spec.agent.prompt_mode, AgentPromptMode::TmuxPaste)
    {
        send_prompt_to_tmux(&session_name, &prompt_file).await?;
    }

    let deadline = Instant::now() + Duration::from_millis(spec.agent.turn_timeout_ms);
    let idle_limit = Duration::from_millis(spec.agent.idle_timeout_ms.max(500));
    let mut last_pane = String::new();
    let mut last_change = Instant::now();
    loop {
        if Instant::now() > deadline {
            kill_tmux_session(&session_name).await;
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_name = %session_name,
                timeout_ms = spec.agent.turn_timeout_ms,
                "tmux-backed agent turn timed out"
            );
            return Err(CoreError::Adapter("turn_timeout".into()));
        }

        let pane = capture_tmux_pane(&session_name).await?;
        if pane != last_pane {
            let delta = diff_tail(&last_pane, &pane);
            if !delta.trim().is_empty() {
                emit(
                    &event_tx,
                    &spec,
                    AgentEventKind::Notification,
                    Some(delta.trim().to_string()),
                    Some(session_name.clone()),
                    None,
                    None,
                    None,
                );
            }
            if spec
                .agent
                .completion_sentinel
                .as_ref()
                .is_some_and(|sentinel| pane.contains(sentinel))
            {
                kill_tmux_session(&session_name).await;
                debug!(
                    issue_identifier = %spec.issue.identifier,
                    agent_name = %spec.agent.name,
                    session_name = %session_name,
                    "tmux completion sentinel observed"
                );
                return Ok(status_to_result(
                    &spec,
                    &event_tx,
                    Some(session_name.clone()),
                    Some(0),
                ));
            }
            last_change = Instant::now();
            last_pane = pane;
        }

        if let Ok(exit_code) = fs::read_to_string(&exit_path).await {
            let code = exit_code.trim().parse::<i32>().ok();
            kill_tmux_session(&session_name).await;
            debug!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_name = %session_name,
                exit_code = ?code,
                "tmux-backed agent turn completed"
            );
            return Ok(status_to_result(
                &spec,
                &event_tx,
                Some(session_name.clone()),
                code,
            ));
        }

        if matches!(
            spec.agent.interaction_mode,
            AgentInteractionMode::Interactive
        ) && Instant::now().duration_since(last_change) >= idle_limit
        {
            kill_tmux_session(&session_name).await;
            debug!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_name = %session_name,
                idle_timeout_ms = spec.agent.idle_timeout_ms,
                "tmux-backed interactive agent stopped after idle timeout"
            );
            return Ok(status_to_result(
                &spec,
                &event_tx,
                Some(session_name.clone()),
                Some(0),
            ));
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn maybe_write_prompt(
    spec: &AgentRunSpec,
    prompt_file: &std::path::Path,
    stdin: &mut tokio::process::ChildStdin,
) -> Result<(), CoreError> {
    if !matches!(spec.agent.prompt_mode, AgentPromptMode::Stdin)
        && !matches!(
            spec.agent.interaction_mode,
            AgentInteractionMode::Interactive
        )
    {
        return Ok(());
    }
    let prompt = fs::read_to_string(prompt_file)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    stdin
        .shutdown()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    Ok(())
}

async fn send_prompt_to_tmux(
    session_name: &str,
    prompt_file: &std::path::Path,
) -> Result<(), CoreError> {
    let load_status = Command::new("tmux")
        .arg("load-buffer")
        .arg(prompt_file)
        .status()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !load_status.success() {
        return Err(CoreError::Adapter("failed to load tmux buffer".into()));
    }
    let paste_status = Command::new("tmux")
        .arg("paste-buffer")
        .arg("-t")
        .arg(session_name)
        .status()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !paste_status.success() {
        return Err(CoreError::Adapter("failed to paste tmux buffer".into()));
    }
    let enter_status = Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(session_name)
        .arg("Enter")
        .status()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !enter_status.success() {
        return Err(CoreError::Adapter("failed to submit tmux prompt".into()));
    }
    Ok(())
}

async fn capture_tmux_pane(session_name: &str) -> Result<String, CoreError> {
    let output = Command::new("tmux")
        .arg("capture-pane")
        .arg("-p")
        .arg("-t")
        .arg(session_name)
        .arg("-S")
        .arg("-200")
        .output()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !output.status.success() {
        return Err(CoreError::Adapter("failed to capture tmux pane".into()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn kill_tmux_session(session_name: &str) {
    let _ = Command::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(session_name)
        .status()
        .await;
}

fn diff_tail(previous: &str, current: &str) -> String {
    if let Some(stripped) = current.strip_prefix(previous) {
        return stripped.to_string();
    }
    current.to_string()
}

#[cfg(test)]
mod tests {
    use {
        super::LocalCliRuntime,
        polyphony_core::{
            AgentDefinition, AgentInteractionMode, AgentPromptMode, AgentProviderRuntime,
            AgentRunSpec, AgentTransport, Issue,
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
    async fn stdio_runner_streams_output() {
        let runtime = LocalCliRuntime::fallback_transport();
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
                        name: "claude".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::LocalCli,
                        command: Some("printf 'first line\\nsecond line\\n'".into()),
                        turn_timeout_ms: 1_000,
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
        let mut saw_stdout = false;
        while let Ok(event) = rx.try_recv() {
            if event.message.as_deref() == Some("first line") {
                saw_stdout = true;
            }
        }
        assert!(saw_stdout);
    }

    #[tokio::test]
    async fn interactive_stdio_writes_prompt_to_stdin() {
        let runtime = LocalCliRuntime::fallback_transport();
        let dir = tempdir().unwrap();
        let output = dir.path().join("prompt.txt");
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: None,
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello from stdin".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "claude".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::LocalCli,
                        command: Some(format!("cat > {}", output.display())),
                        interaction_mode: AgentInteractionMode::Interactive,
                        prompt_mode: AgentPromptMode::Stdin,
                        turn_timeout_ms: 1_000,
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
        let captured = tokio::fs::read_to_string(output).await.unwrap();
        assert!(captured.contains("hello from stdin"));
    }
}
