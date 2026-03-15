use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use {
    async_trait::async_trait,
    polyphony_agent_common::{
        base_agent_env, discover_models_from_command, emit, extract_text_rate_limit_signal,
        fetch_budget_for_agent, prepare_context_file, prepare_prompt_file,
        sanitize_session_fragment, selected_model_hint, shell_escape, status_to_result,
    },
    polyphony_core::{
        AgentEventKind, AgentInteractionMode, AgentPromptMode, AgentProviderRuntime,
        AgentRunResult, AgentRunSpec, BudgetSnapshot, Error as CoreError, RateLimitSignal,
    },
    portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system},
    tokio::{fs, process::Command, sync::mpsc, time::Instant},
    tracing::{debug, info, warn},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptDelivery {
    None,
    RedirectStdinAtLaunch,
    WriteAfterLaunch,
}

#[async_trait]
trait TerminalSession: Send {
    fn session_id(&self) -> Option<&str>;

    async fn send_prompt(&mut self, prompt_file: &Path) -> Result<(), CoreError>;

    #[allow(dead_code)]
    async fn resize(&mut self, rows: u16, cols: u16) -> Result<(), CoreError>;

    async fn snapshot(&mut self) -> Result<String, CoreError>;

    async fn transcript(&mut self) -> Result<String, CoreError>;

    async fn try_wait(&mut self) -> Result<Option<Option<i32>>, CoreError>;

    async fn terminate(&mut self) -> Result<(), CoreError>;
}

struct SpawnedSession {
    session: Box<dyn TerminalSession>,
    prompt_delivery: PromptDelivery,
    startup_message: &'static str,
}

struct TmuxSession {
    session_name: String,
    exit_path: PathBuf,
    output_path: PathBuf,
}

struct PtySession {
    capture_state: Arc<Mutex<PtyCaptureState>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    #[allow(dead_code)]
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

struct PtyCaptureState {
    parser: vt100::Parser,
    transcript: String,
}

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
        transport = if spec.agent.use_tmux { "tmux" } else { "pty" },
        "starting local agent run"
    );

    let command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("agent command is required".into()))?;
    let prompt_file = absolute_path(&prepare_prompt_file(&spec).await?)?;
    let context_file = match prepare_context_file(&spec).await? {
        Some(path) => Some(absolute_path(&path)?),
        None => None,
    };
    let workspace_path = absolute_path(&spec.workspace_path)?;

    let spawned = if spec.agent.use_tmux {
        spawn_tmux_session(
            &spec,
            &command,
            &prompt_file,
            context_file.as_deref(),
            &workspace_path,
        )
        .await?
    } else {
        spawn_pty_session(
            &spec,
            &command,
            &prompt_file,
            context_file.as_deref(),
            &workspace_path,
        )
        .await?
    };

    run_terminal_session(spec, event_tx, prompt_file, spawned).await
}

async fn run_terminal_session(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    prompt_file: PathBuf,
    mut spawned: SpawnedSession,
) -> Result<AgentRunResult, CoreError> {
    let session_id = spawned.session.session_id().map(ToOwned::to_owned);
    emit(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some(spawned.startup_message.into()),
        session_id.clone(),
        None,
        None,
        None,
    );
    emit(
        &event_tx,
        &spec,
        AgentEventKind::TurnStarted,
        Some("turn started".into()),
        session_id.clone(),
        None,
        None,
        None,
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    if matches!(spawned.prompt_delivery, PromptDelivery::WriteAfterLaunch) {
        spawned.session.send_prompt(&prompt_file).await?;
    }

    let deadline = Instant::now() + Duration::from_millis(spec.agent.turn_timeout_ms);
    let idle_limit = Duration::from_millis(spec.agent.idle_timeout_ms.max(500));
    let mut last_snapshot = String::new();
    let mut last_change = Instant::now();
    loop {
        if Instant::now() > deadline {
            spawned.session.terminate().await?;
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                timeout_ms = spec.agent.turn_timeout_ms,
                "local terminal session timed out"
            );
            return Err(CoreError::Adapter("turn_timeout".into()));
        }

        if let Some(code) = spawned.session.try_wait().await? {
            let snapshot = spawned.session.snapshot().await.unwrap_or_default();
            let transcript = spawned.session.transcript().await.unwrap_or_default();
            emit_terminal_snapshot_delta(
                &event_tx,
                &spec,
                session_id.as_deref(),
                &mut last_snapshot,
                &mut last_change,
                snapshot,
            );
            if let Some(signal) = extract_local_rate_limit_signal(
                &spec,
                pick_rate_limit_source(&last_snapshot, &transcript),
            ) {
                spawned.session.terminate().await?;
                warn!(
                    issue_identifier = %spec.issue.identifier,
                    agent_name = %spec.agent.name,
                    session_id = ?session_id,
                    reason = %signal.reason,
                    retry_after_ms = ?signal.retry_after_ms,
                    reset_at = ?signal.reset_at,
                    "local terminal session hit a local rate limit"
                );
                return Err(CoreError::RateLimited(Box::new(signal)));
            }
            spawned.session.terminate().await?;
            debug!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = ?session_id,
                exit_code = ?code,
                "local terminal session completed"
            );
            return Ok(status_to_result(&spec, &event_tx, session_id, code));
        }

        let snapshot = spawned.session.snapshot().await?;
        let transcript = spawned.session.transcript().await.unwrap_or_default();
        emit_terminal_snapshot_delta(
            &event_tx,
            &spec,
            session_id.as_deref(),
            &mut last_snapshot,
            &mut last_change,
            snapshot.clone(),
        );

        if spec
            .agent
            .completion_sentinel
            .as_ref()
            .is_some_and(|sentinel| snapshot.contains(sentinel))
        {
            spawned.session.terminate().await?;
            debug!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = ?session_id,
                "terminal completion sentinel observed"
            );
            return Ok(status_to_result(&spec, &event_tx, session_id, Some(0)));
        }

        if let Some(signal) =
            extract_local_rate_limit_signal(&spec, pick_rate_limit_source(&snapshot, &transcript))
        {
            spawned.session.terminate().await?;
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = ?session_id,
                reason = %signal.reason,
                retry_after_ms = ?signal.retry_after_ms,
                reset_at = ?signal.reset_at,
                "local terminal session hit a local rate limit"
            );
            return Err(CoreError::RateLimited(Box::new(signal)));
        }

        if matches!(
            spec.agent.interaction_mode,
            AgentInteractionMode::Interactive
        ) && Instant::now().duration_since(last_change) >= idle_limit
        {
            spawned.session.terminate().await?;
            debug!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                session_id = ?session_id,
                idle_timeout_ms = spec.agent.idle_timeout_ms,
                "interactive terminal session stopped after idle timeout"
            );
            return Ok(status_to_result(&spec, &event_tx, session_id, Some(0)));
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn spawn_tmux_session(
    spec: &AgentRunSpec,
    command: &str,
    prompt_file: &Path,
    context_file: Option<&Path>,
    workspace_path: &Path,
) -> Result<SpawnedSession, CoreError> {
    let run_dir = workspace_path.join(".polyphony");
    fs::create_dir_all(&run_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let exit_path = run_dir.join(format!("{}-exit.txt", spec.agent.name));
    let output_path = run_dir.join(format!("{}-tmux.log", spec.agent.name));
    clear_tmux_artifacts(&exit_path, &output_path).await?;
    let prompt_delivery = tmux_prompt_delivery(&spec.agent);
    let launch_command = tmux_launch_command(command, prompt_file, prompt_delivery);
    let session_name = format!(
        "{}-{}-{}",
        spec.agent
            .tmux_session_prefix
            .clone()
            .unwrap_or_else(|| spec.agent.name.clone()),
        sanitize_session_fragment(&spec.issue.identifier),
        spec.attempt.unwrap_or(0)
    );
    let wrapped = tmux_wrapped_command(workspace_path, &launch_command, &output_path, &exit_path);
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_name = %session_name,
        timeout_ms = spec.agent.turn_timeout_ms,
        idle_timeout_ms = spec.agent.idle_timeout_ms,
        "starting tmux-backed agent turn"
    );
    debug!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_name = %session_name,
        prompt_delivery = ?prompt_delivery,
        prompt_file = %prompt_file.display(),
        output_path = %output_path.display(),
        exit_path = %exit_path.display(),
        "tmux-backed agent command prepared"
    );

    let model = selected_model_hint(&spec.agent);
    let mut tmux = Command::new("tmux");
    tmux.env_remove("CLAUDECODE");
    tmux.arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&session_name)
        .arg("bash")
        .arg("-lc")
        .arg(wrapped);
    for (key, value) in base_agent_env(spec, prompt_file, context_file, model.as_deref()) {
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

    Ok(SpawnedSession {
        session: Box::new(TmuxSession {
            session_name,
            exit_path,
            output_path,
        }),
        prompt_delivery,
        startup_message: "tmux session started",
    })
}

async fn spawn_pty_session(
    spec: &AgentRunSpec,
    command: &str,
    prompt_file: &Path,
    context_file: Option<&Path>,
    workspace_path: &Path,
) -> Result<SpawnedSession, CoreError> {
    let run_dir = workspace_path.join(".polyphony");
    fs::create_dir_all(&run_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let output_path = run_dir.join(format!("{}-pty.log", spec.agent.name));
    remove_file_if_exists(&output_path).await?;

    let model = selected_model_hint(&spec.agent);
    let command_builder = build_pty_command(
        command,
        workspace_path,
        &spec.agent.env,
        spec,
        prompt_file,
        context_file,
        model.as_deref(),
    );
    let capture_state = Arc::new(Mutex::new(PtyCaptureState {
        parser: vt100::Parser::new(24, 80, 2_000),
        transcript: String::new(),
    }));
    let pty_system = native_pty_system();
    let pair = tokio::task::spawn_blocking(move || {
        pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| CoreError::Adapter(error.to_string()))
    })
    .await
    .map_err(join_error)??;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let slave = pair.slave;
    let child = tokio::task::spawn_blocking(move || {
        slave
            .spawn_command(command_builder)
            .map_err(|error| CoreError::Adapter(error.to_string()))
    })
    .await
    .map_err(join_error)??;
    let child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>> = Arc::new(Mutex::new(child));
    let killer = {
        let guard = child
            .lock()
            .map_err(|_| CoreError::Adapter("pty child lock poisoned".into()))?;
        Arc::new(Mutex::new(guard.clone_killer()))
    };
    spawn_pty_reader(reader, capture_state.clone(), output_path)?;

    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        timeout_ms = spec.agent.turn_timeout_ms,
        idle_timeout_ms = spec.agent.idle_timeout_ms,
        "starting pty-backed agent turn"
    );

    let prompt_delivery = pty_prompt_delivery(&spec.agent);
    let writer = if matches!(prompt_delivery, PromptDelivery::None) {
        None
    } else {
        Some(writer)
    };

    Ok(SpawnedSession {
        session: Box::new(PtySession {
            capture_state,
            child,
            killer,
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(pair.master)),
        }),
        prompt_delivery,
        startup_message: "pty session started",
    })
}

#[async_trait]
impl TerminalSession for TmuxSession {
    fn session_id(&self) -> Option<&str> {
        Some(&self.session_name)
    }

    async fn send_prompt(&mut self, prompt_file: &Path) -> Result<(), CoreError> {
        send_prompt_to_tmux(&self.session_name, prompt_file).await
    }

    async fn resize(&mut self, rows: u16, cols: u16) -> Result<(), CoreError> {
        let status = Command::new("tmux")
            .arg("resize-window")
            .arg("-t")
            .arg(&self.session_name)
            .arg("-x")
            .arg(cols.to_string())
            .arg("-y")
            .arg(rows.to_string())
            .status()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(CoreError::Adapter("failed to resize tmux window".into()))
        }
    }

    async fn snapshot(&mut self) -> Result<String, CoreError> {
        capture_tmux_pane(&self.session_name).await
    }

    async fn transcript(&mut self) -> Result<String, CoreError> {
        Ok(read_tmux_output(&self.output_path)
            .await
            .unwrap_or_default())
    }

    async fn try_wait(&mut self) -> Result<Option<Option<i32>>, CoreError> {
        Ok(read_tmux_exit_code(&self.exit_path).await)
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        kill_tmux_session(&self.session_name).await;
        Ok(())
    }
}

#[async_trait]
impl TerminalSession for PtySession {
    fn session_id(&self) -> Option<&str> {
        None
    }

    async fn send_prompt(&mut self, prompt_file: &Path) -> Result<(), CoreError> {
        let prompt = fs::read_to_string(prompt_file)
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = writer
                .lock()
                .map_err(|_| CoreError::Adapter("pty writer lock poisoned".into()))?;
            let Some(mut writer) = guard.take() else {
                return Ok(());
            };
            writer
                .write_all(prompt.as_bytes())
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            writer
                .write_all(b"\n")
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            writer
                .flush()
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            Ok(())
        })
        .await
        .map_err(join_error)?
    }

    async fn resize(&mut self, rows: u16, cols: u16) -> Result<(), CoreError> {
        let master = self.master.clone();
        tokio::task::spawn_blocking(move || {
            let guard = master
                .lock()
                .map_err(|_| CoreError::Adapter("pty master lock poisoned".into()))?;
            guard
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| CoreError::Adapter(error.to_string()))
        })
        .await
        .map_err(join_error)?
    }

    async fn snapshot(&mut self) -> Result<String, CoreError> {
        let guard = self
            .capture_state
            .lock()
            .map_err(|_| CoreError::Adapter("pty capture lock poisoned".into()))?;
        Ok(guard.parser.screen().contents())
    }

    async fn transcript(&mut self) -> Result<String, CoreError> {
        let guard = self
            .capture_state
            .lock()
            .map_err(|_| CoreError::Adapter("pty capture lock poisoned".into()))?;
        Ok(guard.transcript.clone())
    }

    async fn try_wait(&mut self) -> Result<Option<Option<i32>>, CoreError> {
        let child = self.child.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = child
                .lock()
                .map_err(|_| CoreError::Adapter("pty child lock poisoned".into()))?;
            let status = guard
                .try_wait()
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            Ok(status.map(|status| portable_exit_code(&status)))
        })
        .await
        .map_err(join_error)?
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        let killer = self.killer.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = killer
                .lock()
                .map_err(|_| CoreError::Adapter("pty killer lock poisoned".into()))?;
            match guard.kill() {
                Ok(()) => Ok(()),
                Err(error)
                    if error.kind() == std::io::ErrorKind::InvalidInput
                        || error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(3) =>
                {
                    Ok(())
                },
                Err(error) => Err(CoreError::Adapter(error.to_string())),
            }
        })
        .await
        .map_err(join_error)?
    }
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            return Err(CoreError::Adapter("failed to capture tmux pane".into()));
        }
        return Err(CoreError::Adapter(format!(
            "failed to capture tmux pane: {detail}"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn tmux_prompt_delivery(agent: &polyphony_core::AgentDefinition) -> PromptDelivery {
    match agent.prompt_mode {
        AgentPromptMode::Stdin => PromptDelivery::RedirectStdinAtLaunch,
        AgentPromptMode::TmuxPaste => {
            if command_requires_launch_stdin(agent) {
                PromptDelivery::RedirectStdinAtLaunch
            } else {
                PromptDelivery::WriteAfterLaunch
            }
        },
        AgentPromptMode::Env => {
            if matches!(agent.interaction_mode, AgentInteractionMode::Interactive) {
                PromptDelivery::WriteAfterLaunch
            } else {
                PromptDelivery::None
            }
        },
    }
}

fn pty_prompt_delivery(agent: &polyphony_core::AgentDefinition) -> PromptDelivery {
    match agent.prompt_mode {
        AgentPromptMode::Env => {
            if matches!(agent.interaction_mode, AgentInteractionMode::Interactive) {
                PromptDelivery::WriteAfterLaunch
            } else {
                PromptDelivery::None
            }
        },
        AgentPromptMode::Stdin | AgentPromptMode::TmuxPaste => PromptDelivery::WriteAfterLaunch,
    }
}

fn command_requires_launch_stdin(agent: &polyphony_core::AgentDefinition) -> bool {
    agent.kind.eq_ignore_ascii_case("claude")
        && agent
            .command
            .as_deref()
            .is_some_and(claude_command_uses_print_mode)
}

fn claude_command_uses_print_mode(command: &str) -> bool {
    command
        .split_whitespace()
        .any(|token| token == "-p" || token == "--print")
}

fn build_pty_command(
    command: &str,
    cwd: &Path,
    extra_env: &std::collections::BTreeMap<String, String>,
    spec: &AgentRunSpec,
    prompt_file: &Path,
    context_file: Option<&Path>,
    model: Option<&str>,
) -> CommandBuilder {
    let mut builder = CommandBuilder::new("bash");
    builder.arg("-lc");
    builder.arg(command);
    builder.cwd(cwd.as_os_str());
    builder.env_remove("CLAUDECODE");
    for (key, value) in base_agent_env(spec, prompt_file, context_file, model) {
        builder.env(key, value);
    }
    for (key, value) in extra_env {
        builder.env(key, value);
    }
    builder
}

fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    capture_state: Arc<Mutex<PtyCaptureState>>,
    output_path: PathBuf,
) -> Result<(), CoreError> {
    let file = std::fs::File::create(output_path)
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    std::thread::Builder::new()
        .name("polyphony-pty-reader".into())
        .spawn(move || {
            let mut file = file;
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let chunk = &buffer[..read];
                        let _ = file.write_all(chunk);
                        let text = String::from_utf8_lossy(chunk);
                        if let Ok(mut guard) = capture_state.lock() {
                            guard.parser.process(chunk);
                            guard.transcript.push_str(&text);
                        } else {
                            break;
                        }
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map(|_| ())
        .map_err(|error| CoreError::Adapter(error.to_string()))
}

fn portable_exit_code(status: &portable_pty::ExitStatus) -> Option<i32> {
    i32::try_from(status.exit_code()).ok()
}

fn pick_rate_limit_source<'a>(snapshot: &'a str, transcript: &'a str) -> &'a str {
    if transcript.trim().is_empty() {
        snapshot
    } else {
        transcript
    }
}

fn join_error(error: tokio::task::JoinError) -> CoreError {
    CoreError::Adapter(error.to_string())
}

fn tmux_launch_command(
    command: &str,
    prompt_file: &std::path::Path,
    prompt_delivery: PromptDelivery,
) -> String {
    match prompt_delivery {
        PromptDelivery::RedirectStdinAtLaunch => format!(
            "({command}) < {}",
            shell_escape(prompt_file.to_string_lossy().as_ref())
        ),
        PromptDelivery::None | PromptDelivery::WriteAfterLaunch => command.to_string(),
    }
}

fn absolute_path(path: &std::path::Path) -> Result<std::path::PathBuf, CoreError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current_dir =
        std::env::current_dir().map_err(|error| CoreError::Adapter(error.to_string()))?;
    Ok(current_dir.join(path))
}

fn tmux_wrapped_command(
    workspace_path: &std::path::Path,
    launch_command: &str,
    output_path: &std::path::Path,
    exit_path: &std::path::Path,
) -> String {
    format!(
        "cd {} || exit $?; set +e; {{ {} 2>&1 | tee {}; code=${{PIPESTATUS[0]}}; printf '%s' \"$code\" > {}; exit \"$code\"; }}",
        shell_escape(workspace_path.to_string_lossy().as_ref()),
        launch_command,
        shell_escape(output_path.to_string_lossy().as_ref()),
        shell_escape(exit_path.to_string_lossy().as_ref()),
    )
}

fn extract_local_rate_limit_signal(spec: &AgentRunSpec, text: &str) -> Option<RateLimitSignal> {
    extract_text_rate_limit_signal(spec, text)
}

fn emit_terminal_snapshot_delta(
    event_tx: &mpsc::UnboundedSender<polyphony_core::AgentEvent>,
    spec: &AgentRunSpec,
    session_id: Option<&str>,
    last_snapshot: &mut String,
    last_change: &mut Instant,
    snapshot: String,
) {
    let delta = diff_tail(last_snapshot, &snapshot);
    if !delta.trim().is_empty() {
        emit(
            event_tx,
            spec,
            AgentEventKind::Notification,
            Some(delta.trim().to_string()),
            session_id.map(ToOwned::to_owned),
            None,
            None,
            None,
        );
    }
    *last_change = Instant::now();
    *last_snapshot = snapshot;
}

async fn read_tmux_exit_code(exit_path: &std::path::Path) -> Option<Option<i32>> {
    fs::read_to_string(exit_path)
        .await
        .ok()
        .map(|exit_code| exit_code.trim().parse::<i32>().ok())
}

async fn read_tmux_output(output_path: &std::path::Path) -> Option<String> {
    fs::read_to_string(output_path).await.ok()
}

async fn clear_tmux_artifacts(
    exit_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), CoreError> {
    remove_file_if_exists(exit_path).await?;
    remove_file_if_exists(output_path).await?;
    Ok(())
}

async fn remove_file_if_exists(path: &std::path::Path) -> Result<(), CoreError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CoreError::Adapter(error.to_string())),
    }
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
            AgentRunSpec, AgentTransport, Error as CoreError, Issue,
        },
        std::os::unix::fs::PermissionsExt,
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
    async fn pty_runner_streams_visible_output() {
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

        assert!(
            matches!(result.status, polyphony_core::AttemptStatus::Succeeded),
            "{result:?}"
        );
        let mut saw_terminal_output = false;
        while let Ok(event) = rx.try_recv() {
            if event.message.as_deref().is_some_and(|message| {
                message.contains("first line") && message.contains("second line")
            }) {
                saw_terminal_output = true;
            }
        }
        assert!(saw_terminal_output);
    }

    #[tokio::test]
    async fn interactive_pty_writes_prompt_to_stdin() {
        let runtime = LocalCliRuntime::fallback_transport();
        let dir = tempdir().unwrap();
        let output = dir.path().join("prompt.txt");
        let (tx, mut rx) = mpsc::unbounded_channel();
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

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            matches!(result.status, polyphony_core::AttemptStatus::Succeeded),
            "result={result:?} events={events:?}"
        );
        let captured = tokio::fs::read_to_string(output).await.unwrap();
        assert!(captured.contains("hello from stdin"));
    }

    #[test]
    fn claude_print_tmux_uses_launch_stdin() {
        let agent = AgentDefinition {
            name: "opus".into(),
            kind: "claude".into(),
            command: Some("claude -p --verbose".into()),
            interaction_mode: AgentInteractionMode::Interactive,
            prompt_mode: AgentPromptMode::TmuxPaste,
            ..AgentDefinition::default()
        };

        assert_eq!(
            super::tmux_prompt_delivery(&agent),
            super::PromptDelivery::RedirectStdinAtLaunch
        );
        assert!(super::claude_command_uses_print_mode(
            agent.command.as_deref().unwrap()
        ));
    }

    #[test]
    fn tmux_wrapper_disables_errexit_and_records_exit_status() {
        let wrapped = super::tmux_wrapped_command(
            std::path::Path::new("/tmp/worktree"),
            "claude -p --verbose",
            std::path::Path::new("/tmp/worktree/.polyphony/opus-tmux.log"),
            std::path::Path::new("/tmp/worktree/.polyphony/opus-exit.txt"),
        );

        assert!(wrapped.contains("set +e;"));
        assert!(wrapped.contains("tee "));
        assert!(wrapped.contains("printf '%s' \"$code\" >"));
        assert!(wrapped.contains("exit \"$code\""));
    }

    #[test]
    fn claude_rate_limit_signal_defaults_to_five_hours() {
        let spec = AgentRunSpec {
            issue: test_issue(),
            attempt: Some(0),
            workspace_path: std::env::temp_dir(),
            prompt: "test".into(),
            max_turns: 1,
            prior_context: None,
            agent: AgentDefinition {
                name: "opus".into(),
                kind: "claude".into(),
                ..AgentDefinition::default()
            },
        };

        let signal = super::extract_local_rate_limit_signal(
            &spec,
            "Claude usage limit reached, please try again later.",
        )
        .unwrap();

        assert_eq!(signal.component, "agent:opus");
        assert_eq!(signal.status_code, Some(429));
        assert_eq!(signal.retry_after_ms, Some(5 * 60 * 60 * 1000));
    }

    #[test]
    fn claude_hit_your_limit_message_is_detected_and_parsed() {
        let spec = AgentRunSpec {
            issue: test_issue(),
            attempt: Some(0),
            workspace_path: std::env::temp_dir(),
            prompt: "test".into(),
            max_turns: 1,
            prior_context: None,
            agent: AgentDefinition {
                name: "opus".into(),
                kind: "claude".into(),
                ..AgentDefinition::default()
            },
        };

        let signal = super::extract_local_rate_limit_signal(
            &spec,
            "You've hit your limit · resets 2am (Europe/Lisbon)",
        )
        .unwrap();

        assert_eq!(signal.component, "agent:opus");
        assert_eq!(signal.status_code, Some(429));
        assert!(signal.retry_after_ms.is_some());
        assert!(signal.reset_at.is_some());
    }

    #[test]
    fn absolute_path_resolves_relative_paths_against_current_dir() {
        let current_dir = std::env::current_dir().unwrap();
        let resolved =
            super::absolute_path(std::path::Path::new(".polyphony/workspaces/xm7")).unwrap();

        assert_eq!(resolved, current_dir.join(".polyphony/workspaces/xm7"));
    }

    #[tokio::test]
    async fn clear_tmux_artifacts_removes_stale_exit_and_output_files() {
        let dir = tempdir().unwrap();
        let exit_path = dir.path().join("opus-exit.txt");
        let output_path = dir.path().join("opus-tmux.log");
        tokio::fs::write(&exit_path, "1").await.unwrap();
        tokio::fs::write(&output_path, "stale").await.unwrap();

        super::clear_tmux_artifacts(&exit_path, &output_path)
            .await
            .unwrap();

        assert!(!exit_path.exists());
        assert!(!output_path.exists());
    }

    #[tokio::test]
    async fn tmux_claude_print_mode_reads_prompt_before_launch() {
        let tmux_available = tokio::process::Command::new("tmux")
            .arg("-V")
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false);
        if !tmux_available {
            return;
        }

        let runtime = LocalCliRuntime::fallback_transport();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let output = dir.path().join("captured.txt");
        let claude = bin_dir.join("claude");
        let script = format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \" $* \" == *\" -p \"* || \" $* \" == *\" --print \"* ]]; then\n  line=$(cat)\n  if [[ -z \"$line\" ]]; then\n    echo \"Error: Input must be provided either through stdin or as a prompt argument when using --print\" >&2\n    exit 2\n  fi\n  printf '%s\\n' \"$line\" > {}\n  exit 0\nfi\nexit 3\n",
            output.display()
        );
        tokio::fs::write(&claude, script).await.unwrap();
        let mut perms = tokio::fs::metadata(&claude).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&claude, perms).await.unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: Some(0),
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello from tmux".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "opus".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::LocalCli,
                        command: Some(format!("{} -p --verbose", claude.display())),
                        interaction_mode: AgentInteractionMode::Interactive,
                        prompt_mode: AgentPromptMode::TmuxPaste,
                        use_tmux: true,
                        tmux_session_prefix: Some("test-opus".into()),
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

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            matches!(result.status, polyphony_core::AttemptStatus::Succeeded),
            "result={result:?} events={events:?}"
        );
        let captured = tokio::fs::read_to_string(output).await.unwrap();
        assert_eq!(captured.trim(), "hello from tmux");
    }

    #[tokio::test]
    async fn tmux_claude_rate_limit_returns_rate_limited_error() {
        let tmux_available = tokio::process::Command::new("tmux")
            .arg("-V")
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false);
        if !tmux_available {
            return;
        }

        let runtime = LocalCliRuntime::fallback_transport();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let claude = bin_dir.join("claude");
        let script = "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \" $* \" == *\" -p \"* || \" $* \" == *\" --print \"* ]]; then\n  cat >/dev/null\n  echo 'Claude usage limit reached, please try again later.'\n  exit 1\nfi\nexit 3\n";
        tokio::fs::write(&claude, script).await.unwrap();
        let mut perms = tokio::fs::metadata(&claude).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&claude, perms).await.unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let error = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: Some(0),
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello from tmux".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "opus".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::LocalCli,
                        command: Some(format!("{} -p --verbose", claude.display())),
                        interaction_mode: AgentInteractionMode::Interactive,
                        prompt_mode: AgentPromptMode::TmuxPaste,
                        use_tmux: true,
                        tmux_session_prefix: Some("test-opus-limit".into()),
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
            .unwrap_err();

        match error {
            CoreError::RateLimited(signal) => {
                assert_eq!(signal.component, "agent:opus");
                assert_eq!(signal.status_code, Some(429));
            },
            other => panic!("expected rate limit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tmux_claude_print_mode_preserves_nonzero_exit_code() {
        let tmux_available = tokio::process::Command::new("tmux")
            .arg("-V")
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false);
        if !tmux_available {
            return;
        }

        let runtime = LocalCliRuntime::fallback_transport();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let output = dir.path().join("captured-failure.txt");
        let claude = bin_dir.join("claude");
        let script = format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \" $* \" == *\" -p \"* || \" $* \" == *\" --print \"* ]]; then\n  line=$(cat)\n  printf '%s\\n' \"$line\" > {}\n  echo 'fatal model mismatch' >&2\n  exit 7\nfi\nexit 3\n",
            output.display()
        );
        tokio::fs::write(&claude, script).await.unwrap();
        let mut perms = tokio::fs::metadata(&claude).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&claude, perms).await.unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: Some(0),
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello from failing tmux".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "opus".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::LocalCli,
                        command: Some(format!("{} -p --verbose", claude.display())),
                        interaction_mode: AgentInteractionMode::Interactive,
                        prompt_mode: AgentPromptMode::TmuxPaste,
                        use_tmux: true,
                        tmux_session_prefix: Some("test-opus-fail".into()),
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

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let exit_file = dir.path().join(".polyphony").join("opus-exit.txt");
        let exit_code = tokio::fs::read_to_string(&exit_file).await.unwrap();

        assert!(
            matches!(result.status, polyphony_core::AttemptStatus::Failed),
            "result={result:?} events={events:?}"
        );
        assert_eq!(exit_code.trim(), "7");
        assert!(
            events.iter().all(|event| {
                event.message.as_deref()
                    != Some("run ended with error: failed to capture tmux pane")
            }),
            "events={events:?}"
        );
    }
}
