use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write as _},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use {
    agent_client_protocol::{self as acp, Agent as _},
    async_trait::async_trait,
    polyphony_agent_common::{
        discover_models_from_command, emit_with_metadata, extract_text_rate_limit_signal,
        fetch_budget_for_agent, prepare_context_file, prepare_prompt_file, selected_model_hint,
        shell_command,
    },
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentModelCatalog, AgentProviderRuntime,
        AgentRunResult, AgentRunSpec, AgentSession, AgentTransport, AttemptStatus, BudgetSnapshot,
        Error as CoreError,
    },
    portable_pty::{ChildKiller, CommandBuilder, native_pty_system},
    serde_json::{Value, json},
    tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::ChildStderr,
        sync::{mpsc, oneshot},
    },
    tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt},
    tracing::{info, warn},
};

#[derive(Debug, Default, Clone)]
pub struct AcpRuntime;

#[async_trait]
impl AgentProviderRuntime for AcpRuntime {
    fn runtime_key(&self) -> String {
        "agent:acp".into()
    }

    fn supports(&self, agent: &AgentDefinition) -> bool {
        matches!(agent.transport, AgentTransport::Acp) || agent.kind.eq_ignore_ascii_case("acp")
    }

    async fn start_session(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, CoreError> {
        Ok(Some(Box::new(launch_acp_session(spec, event_tx).await?)))
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        let prompt = spec.prompt.clone();
        let mut session = launch_acp_session(spec, event_tx).await?;
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

struct AcpSession {
    command_tx: mpsc::UnboundedSender<SessionCommand>,
    worker: Option<JoinHandle<()>>,
}

enum SessionCommand {
    RunTurn {
        prompt: String,
        response_tx: oneshot::Sender<Result<AgentRunResult, CoreError>>,
    },
    Stop {
        response_tx: oneshot::Sender<Result<(), CoreError>>,
    },
}

#[derive(Default)]
struct ClientState {
    rate_limit_signal: Option<polyphony_core::RateLimitSignal>,
    session_id: Option<String>,
    next_terminal_id: u64,
    terminals: BTreeMap<String, Arc<AcpTerminal>>,
}

#[derive(Clone)]
struct AcpClient {
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    state: Arc<Mutex<ClientState>>,
    log_path: PathBuf,
}

impl AcpClient {
    fn new(
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
        log_path: PathBuf,
    ) -> Self {
        Self {
            spec,
            event_tx,
            state: Arc::new(Mutex::new(ClientState::default())),
            log_path,
        }
    }

    fn clear_turn_state(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.rate_limit_signal = None;
        }
    }

    fn set_session_id(&self, session_id: String) {
        if let Ok(mut state) = self.state.lock() {
            state.session_id = Some(session_id);
        }
    }

    fn take_rate_limit_signal(&self) -> Option<polyphony_core::RateLimitSignal> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.rate_limit_signal.take())
    }

    fn record_rate_limit_if_present(&self, text: &str) {
        let Some(signal) = extract_text_rate_limit_signal(&self.spec, text) else {
            return;
        };
        if let Ok(mut state) = self.state.lock() {
            state.rate_limit_signal = Some(signal);
        }
    }

    fn append_log_line(&self, line: &str) {
        if let Some(parent) = self.log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn validate_session_id(&self, session_id: &acp::SessionId) -> acp::Result<()> {
        let expected = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.session_id.clone());
        match expected {
            Some(expected) if expected == session_id.to_string() => Ok(()),
            Some(expected) => Err(acp::Error::invalid_params().data(format!(
                "unknown session_id {}, expected {expected}",
                session_id
            ))),
            None => Err(acp::Error::internal_error().data("ACP session not initialized")),
        }
    }

    fn register_terminal(&self, terminal: Arc<AcpTerminal>) -> Result<String, CoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CoreError::Adapter("ACP client state lock poisoned".into()))?;
        state.next_terminal_id = state.next_terminal_id.saturating_add(1);
        let terminal_id = format!("term-{}", state.next_terminal_id);
        state.terminals.insert(terminal_id.clone(), terminal);
        Ok(terminal_id)
    }

    fn terminal(&self, terminal_id: &acp::TerminalId) -> acp::Result<Arc<AcpTerminal>> {
        let state = self
            .state
            .lock()
            .map_err(|_| acp::Error::internal_error().data("ACP client state lock poisoned"))?;
        state
            .terminals
            .get(&terminal_id.to_string())
            .cloned()
            .ok_or_else(|| {
                acp::Error::invalid_params().data(format!("unknown terminal_id {}", terminal_id))
            })
    }

    fn remove_terminal(&self, terminal_id: &acp::TerminalId) -> acp::Result<Arc<AcpTerminal>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| acp::Error::internal_error().data("ACP client state lock poisoned"))?;
        state
            .terminals
            .remove(&terminal_id.to_string())
            .ok_or_else(|| {
                acp::Error::invalid_params().data(format!("unknown terminal_id {}", terminal_id))
            })
    }

    fn drain_terminals(&self) -> Result<Vec<Arc<AcpTerminal>>, CoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CoreError::Adapter("ACP client state lock poisoned".into()))?;
        Ok(std::mem::take(&mut state.terminals).into_values().collect())
    }

    async fn cleanup_terminals(&self) {
        let terminals = match self.drain_terminals() {
            Ok(terminals) => terminals,
            Err(error) => {
                warn!("failed to drain ACP terminals during cleanup: {error}");
                return;
            },
        };
        for terminal in terminals {
            if let Err(error) = terminal.release().await {
                warn!("failed to release ACP terminal during cleanup: {error}");
            }
        }
    }
}

struct AcpTerminal {
    capture_state: Arc<Mutex<AcpTerminalCaptureState>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
}

struct AcpTerminalCaptureState {
    transcript: String,
    truncated: bool,
    output_byte_limit: Option<usize>,
    exit_status: Option<acp::TerminalExitStatus>,
}

#[async_trait(?Send)]
impl acp::Client for AcpClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let selected = args
            .options
            .iter()
            .find(|option| matches!(option.kind, acp::PermissionOptionKind::AllowAlways))
            .or_else(|| {
                args.options
                    .iter()
                    .find(|option| matches!(option.kind, acp::PermissionOptionKind::AllowOnce))
            })
            .or_else(|| args.options.first());
        let Some(option) = selected else {
            return Ok(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Cancelled,
            ));
        };

        let title = args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "permission requested".into());
        let message = format!("auto-approved permission: {title} ({})", option.name);
        self.append_log_line(&message);
        emit_acp_event(
            &self.event_tx,
            &self.spec,
            AgentEventKind::Notification,
            Some(message),
            Some(args.session_id.to_string()),
            Some(serde_json::to_value(&args).unwrap_or(Value::Null)),
        );

        Ok(acp::RequestPermissionResponse::new(
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                option.option_id.clone(),
            )),
        ))
    }

    async fn write_text_file(
        &self,
        args: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        tokio::fs::write(&args.path, args.content)
            .await
            .map_err(acp_error_from_io)?;
        Ok(acp::WriteTextFileResponse::new())
    }

    async fn read_text_file(
        &self,
        args: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        let content = tokio::fs::read_to_string(&args.path)
            .await
            .map_err(acp_error_from_io)?;
        Ok(acp::ReadTextFileResponse::new(slice_lines(
            &content, args.line, args.limit,
        )))
    }

    async fn create_terminal(
        &self,
        args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        self.validate_session_id(&args.session_id)?;
        let terminal = spawn_acp_terminal(&args)
            .await
            .map_err(acp_error_from_core)?;
        let terminal_id = self
            .register_terminal(Arc::new(terminal))
            .map_err(acp_error_from_core)?;
        self.append_log_line(&format!(
            "terminal created: {} {}",
            terminal_id, args.command
        ));
        Ok(acp::CreateTerminalResponse::new(terminal_id))
    }

    async fn terminal_output(
        &self,
        args: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        self.validate_session_id(&args.session_id)?;
        let terminal = self.terminal(&args.terminal_id)?;
        let output = terminal.output().await.map_err(acp_error_from_core)?;
        Ok(output)
    }

    async fn release_terminal(
        &self,
        args: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        self.validate_session_id(&args.session_id)?;
        let terminal = self.remove_terminal(&args.terminal_id)?;
        terminal.release().await.map_err(acp_error_from_core)?;
        self.append_log_line(&format!("terminal released: {}", args.terminal_id));
        Ok(acp::ReleaseTerminalResponse::new())
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        self.validate_session_id(&args.session_id)?;
        let terminal = self.terminal(&args.terminal_id)?;
        let exit_status = terminal
            .wait_for_exit()
            .await
            .map_err(acp_error_from_core)?;
        Ok(acp::WaitForTerminalExitResponse::new(exit_status))
    }

    async fn kill_terminal(
        &self,
        args: acp::KillTerminalRequest,
    ) -> acp::Result<acp::KillTerminalResponse> {
        self.validate_session_id(&args.session_id)?;
        let terminal = self.terminal(&args.terminal_id)?;
        terminal.kill().await.map_err(acp_error_from_core)?;
        self.append_log_line(&format!("terminal killed: {}", args.terminal_id));
        Ok(acp::KillTerminalResponse::new())
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        let session_id = args.session_id.to_string();
        let raw = serde_json::to_value(&args).unwrap_or(Value::Null);
        match &args.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => {
                let message = content_chunk_summary(chunk);
                self.record_rate_limit_if_present(&message);
                self.append_log_line(&format!("agent: {message}"));
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
            acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                let message = format!("thought: {}", content_chunk_summary(chunk));
                self.append_log_line(&message);
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::OtherMessage,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
            acp::SessionUpdate::ToolCall(tool_call) => {
                let message = format!("tool call: {}", tool_call.title);
                self.append_log_line(&message);
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
            acp::SessionUpdate::ToolCallUpdate(update) => {
                let message = tool_call_update_summary(update);
                self.append_log_line(&message);
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
            acp::SessionUpdate::CurrentModeUpdate(update) => {
                let message = format!("mode: {}", update.current_mode_id);
                self.append_log_line(&message);
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::Notification,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
            acp::SessionUpdate::SessionInfoUpdate(update) => {
                let message = session_info_summary(update);
                self.append_log_line(&message);
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::OtherMessage,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
            other => {
                let message = format!("acp update: {}", session_update_kind(other));
                self.append_log_line(&message);
                emit_acp_event(
                    &self.event_tx,
                    &self.spec,
                    AgentEventKind::OtherMessage,
                    Some(message),
                    Some(session_id),
                    Some(raw),
                );
            },
        }
        Ok(())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Ok(())
    }
}

impl AcpTerminal {
    async fn output(&self) -> Result<acp::TerminalOutputResponse, CoreError> {
        let exit_status = self.poll_exit_status().await?;
        let state = self
            .capture_state
            .lock()
            .map_err(|_| CoreError::Adapter("ACP terminal capture lock poisoned".into()))?;
        Ok(
            acp::TerminalOutputResponse::new(state.transcript.clone(), state.truncated)
                .exit_status(exit_status.or_else(|| state.exit_status.clone())),
        )
    }

    async fn wait_for_exit(&self) -> Result<acp::TerminalExitStatus, CoreError> {
        if let Some(exit_status) = self.cached_exit_status()? {
            return Ok(exit_status);
        }
        let child = self.child.clone();
        let status = tokio::task::spawn_blocking(move || {
            let mut guard = child
                .lock()
                .map_err(|_| CoreError::Adapter("ACP terminal child lock poisoned".into()))?;
            guard
                .wait()
                .map_err(|error| CoreError::Adapter(error.to_string()))
        })
        .await
        .map_err(join_error)??;
        let exit_status = terminal_exit_status_from_portable(&status);
        self.set_exit_status(exit_status.clone())?;
        Ok(exit_status)
    }

    async fn kill(&self) -> Result<(), CoreError> {
        if self.cached_exit_status()?.is_some() {
            return Ok(());
        }
        let killer = self.killer.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = killer
                .lock()
                .map_err(|_| CoreError::Adapter("ACP terminal killer lock poisoned".into()))?;
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

    async fn release(&self) -> Result<(), CoreError> {
        self.kill().await?;
        let _ = self.wait_for_exit().await;
        Ok(())
    }

    async fn poll_exit_status(&self) -> Result<Option<acp::TerminalExitStatus>, CoreError> {
        if let Some(exit_status) = self.cached_exit_status()? {
            return Ok(Some(exit_status));
        }
        let child = self.child.clone();
        let status = tokio::task::spawn_blocking(move || {
            let mut guard = child
                .lock()
                .map_err(|_| CoreError::Adapter("ACP terminal child lock poisoned".into()))?;
            guard
                .try_wait()
                .map_err(|error| CoreError::Adapter(error.to_string()))
        })
        .await
        .map_err(join_error)??;
        let Some(status) = status else {
            return Ok(None);
        };
        let exit_status = terminal_exit_status_from_portable(&status);
        self.set_exit_status(exit_status.clone())?;
        Ok(Some(exit_status))
    }

    fn cached_exit_status(&self) -> Result<Option<acp::TerminalExitStatus>, CoreError> {
        let state = self
            .capture_state
            .lock()
            .map_err(|_| CoreError::Adapter("ACP terminal capture lock poisoned".into()))?;
        Ok(state.exit_status.clone())
    }

    fn set_exit_status(&self, exit_status: acp::TerminalExitStatus) -> Result<(), CoreError> {
        let mut state = self
            .capture_state
            .lock()
            .map_err(|_| CoreError::Adapter("ACP terminal capture lock poisoned".into()))?;
        state.exit_status = Some(exit_status);
        Ok(())
    }
}

#[async_trait]
impl AgentSession for AcpSession {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, CoreError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(SessionCommand::RunTurn {
                prompt,
                response_tx,
            })
            .map_err(|_| CoreError::Adapter("ACP session controller stopped".into()))?;
        response_rx
            .await
            .map_err(|_| CoreError::Adapter("ACP session controller stopped".into()))?
    }

    async fn stop(&mut self) -> Result<(), CoreError> {
        if self.worker.is_none() {
            return Ok(());
        }
        let (response_tx, response_rx) = oneshot::channel();
        let _ = self.command_tx.send(SessionCommand::Stop { response_tx });
        let stop_result = response_rx
            .await
            .map_err(|_| CoreError::Adapter("ACP session controller stopped".into()))?;
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || worker.join())
                .await
                .map_err(|error| CoreError::Adapter(error.to_string()))?
                .map_err(|_| CoreError::Adapter("ACP worker thread panicked".into()))?;
        }
        stop_result
    }
}

async fn launch_acp_session(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<AcpSession, CoreError> {
    let command = spec
        .agent
        .command
        .clone()
        .ok_or_else(|| CoreError::Adapter("ACP agents require a command".into()))?;
    let prompt_file = prepare_prompt_file(&spec).await?;
    let context_file = prepare_context_file(&spec).await?;
    let model = selected_model_hint(&spec.agent);
    let log_path = spec
        .workspace_path
        .join(".polyphony")
        .join(format!("{}-acp.log", spec.agent.name));
    if let Some(parent) = log_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
    }
    tokio::fs::write(&log_path, b"")
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (startup_tx, startup_rx) = oneshot::channel();
    let thread_name = format!("polyphony-acp-{}", spec.issue.identifier);
    let worker = std::thread::Builder::new()
        .name(thread_name)
        .spawn({
            let spec = spec.clone();
            let event_tx = event_tx.clone();
            move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = startup_tx.send(Err(CoreError::Adapter(error.to_string())));
                        return;
                    },
                };
                let local_set = tokio::task::LocalSet::new();
                runtime.block_on(local_set.run_until(async move {
                    let _ = run_acp_controller(
                        spec,
                        event_tx,
                        command,
                        prompt_file,
                        context_file,
                        model,
                        log_path,
                        command_rx,
                        startup_tx,
                    )
                    .await;
                }));
            }
        })
        .map_err(|error| CoreError::Adapter(error.to_string()))?;

    let session_id = startup_rx
        .await
        .map_err(|_| CoreError::Adapter("ACP worker failed during startup".into()))??;
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        session_id = %session_id,
        "started ACP agent session"
    );

    Ok(AcpSession {
        command_tx,
        worker: Some(worker),
    })
}

async fn run_acp_controller(
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    command: String,
    prompt_file: PathBuf,
    context_file: Option<PathBuf>,
    model: Option<String>,
    log_path: PathBuf,
    mut command_rx: mpsc::UnboundedReceiver<SessionCommand>,
    startup_tx: oneshot::Sender<Result<String, CoreError>>,
) -> Result<(), CoreError> {
    let mut child = shell_command(
        &command,
        &spec.workspace_path,
        &spec.agent.env,
        &spec,
        &prompt_file,
        context_file.as_deref(),
        model.as_deref(),
    );
    child
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child
        .spawn()
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Adapter("ACP child missing stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Adapter("ACP child missing stdout".into()))?;
    let client = Arc::new(AcpClient::new(spec.clone(), event_tx.clone(), log_path));
    if let Some(stderr) = child.stderr.take() {
        tokio::task::spawn_local(forward_stderr(stderr, client.clone()));
    }

    let (conn, io_task) = acp::ClientSideConnection::new(
        client.clone(),
        stdin.compat_write(),
        stdout.compat(),
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );
    tokio::task::spawn_local(async move {
        if let Err(error) = io_task.await {
            warn!("ACP I/O task exited: {error}");
        }
    });

    let initialize = acp::InitializeRequest::new(acp::ProtocolVersion::V1)
        .client_capabilities(
            acp::ClientCapabilities::new()
                .fs(acp::FileSystemCapabilities::new()
                    .read_text_file(true)
                    .write_text_file(true))
                .terminal(true),
        )
        .client_info(
            acp::Implementation::new("polyphony", env!("CARGO_PKG_VERSION")).title("Polyphony"),
        );
    conn.initialize(initialize)
        .await
        .map_err(acp_error_to_core)?;
    let session = conn
        .new_session(acp::NewSessionRequest::new(spec.workspace_path.clone()))
        .await
        .map_err(acp_error_to_core)?;
    let session_id = session.session_id.to_string();
    client.set_session_id(session_id.clone());
    emit_acp_event(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some("acp session started".into()),
        Some(session_id.clone()),
        Some(serde_json::to_value(&session).unwrap_or(Value::Null)),
    );
    let _ = startup_tx.send(Ok(session_id.clone()));

    while let Some(command) = command_rx.recv().await {
        match command {
            SessionCommand::RunTurn {
                prompt,
                response_tx,
            } => {
                client.clear_turn_state();
                emit_acp_event(
                    &event_tx,
                    &spec,
                    AgentEventKind::TurnStarted,
                    Some("turn started".into()),
                    Some(session_id.clone()),
                    None,
                );
                let result = conn
                    .prompt(acp::PromptRequest::new(session_id.clone(), vec![
                        acp::ContentBlock::from(prompt),
                    ]))
                    .await
                    .map_err(acp_error_to_core)
                    .and_then(|response| {
                        if let Some(signal) = client.take_rate_limit_signal() {
                            return Err(CoreError::RateLimited(Box::new(signal)));
                        }
                        Ok(prompt_response_to_result(
                            &spec,
                            &event_tx,
                            &session_id,
                            response,
                        ))
                    });
                let _ = response_tx.send(result);
            },
            SessionCommand::Stop { response_tx } => {
                let cancel_result = conn
                    .cancel(acp::CancelNotification::new(session_id.clone()))
                    .await
                    .map_err(acp_error_to_core);
                client.cleanup_terminals().await;
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = response_tx.send(cancel_result);
                return Ok(());
            },
        }
    }

    client.cleanup_terminals().await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(())
}

async fn forward_stderr(stderr: ChildStderr, client: Arc<AcpClient>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        client.append_log_line(&format!("stderr: {line}"));
        emit_acp_event(
            &client.event_tx,
            &client.spec,
            AgentEventKind::OtherMessage,
            Some(format!("stderr: {line}")),
            None,
            None,
        );
    }
}

fn emit_acp_event(
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    spec: &AgentRunSpec,
    kind: AgentEventKind,
    message: Option<String>,
    session_id: Option<String>,
    raw: Option<Value>,
) {
    emit_with_metadata(
        event_tx, spec, kind, message, session_id, None, None, None, None, None, raw,
    );
}

fn prompt_response_to_result(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    session_id: &str,
    response: acp::PromptResponse,
) -> AgentRunResult {
    let raw = serde_json::to_value(&response).unwrap_or(Value::Null);
    match response.stop_reason {
        acp::StopReason::EndTurn => {
            emit_acp_event(
                event_tx,
                spec,
                AgentEventKind::TurnCompleted,
                Some("turn completed".into()),
                Some(session_id.to_string()),
                Some(raw),
            );
            AgentRunResult::succeeded(1)
        },
        acp::StopReason::Cancelled => {
            let message = "turn cancelled".to_string();
            emit_acp_event(
                event_tx,
                spec,
                AgentEventKind::TurnCancelled,
                Some(message.clone()),
                Some(session_id.to_string()),
                Some(raw),
            );
            AgentRunResult::cancelled(message)
        },
        reason => {
            let error = format!("acp turn ended with {:?}", reason);
            emit_acp_event(
                event_tx,
                spec,
                AgentEventKind::TurnFailed,
                Some(error.clone()),
                Some(session_id.to_string()),
                Some(raw),
            );
            AgentRunResult {
                status: AttemptStatus::Failed,
                turns_completed: 0,
                error: Some(error),
                final_issue_state: None,
            }
        },
    }
}

fn content_chunk_summary(chunk: &acp::ContentChunk) -> String {
    content_block_summary(&chunk.content)
}

fn content_block_summary(block: &acp::ContentBlock) -> String {
    match block {
        acp::ContentBlock::Text(content) => content.text.clone(),
        acp::ContentBlock::Image(_) => "<image>".into(),
        acp::ContentBlock::Audio(_) => "<audio>".into(),
        acp::ContentBlock::ResourceLink(resource) => resource.uri.to_string(),
        acp::ContentBlock::Resource(_) => "<resource>".into(),
        _ => "<content>".into(),
    }
}

fn tool_call_update_summary(update: &acp::ToolCallUpdate) -> String {
    let title = update
        .fields
        .title
        .clone()
        .unwrap_or_else(|| format!("tool {}", update.tool_call_id));
    if let Some(status) = update.fields.status {
        format!("tool update: {title} ({status:?})")
    } else {
        format!("tool update: {title}")
    }
}

fn session_info_summary(update: &acp::SessionInfoUpdate) -> String {
    let title = maybe_undefined_to_string(&update.title).map(|value| format!("title={value}"));
    let updated_at =
        maybe_undefined_to_string(&update.updated_at).map(|value| format!("updated_at={value}"));
    let parts = [title, updated_at]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "session info updated".into()
    } else {
        format!("session info: {}", parts.join(", "))
    }
}

fn maybe_undefined_to_string<T: ToString>(value: &acp::MaybeUndefined<T>) -> Option<String> {
    match value {
        acp::MaybeUndefined::Value(value) => Some(value.to_string()),
        acp::MaybeUndefined::Null | acp::MaybeUndefined::Undefined => None,
    }
}

fn session_update_kind(update: &acp::SessionUpdate) -> &'static str {
    match update {
        acp::SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
        acp::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
        acp::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
        acp::SessionUpdate::ToolCall(_) => "tool_call",
        acp::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
        acp::SessionUpdate::Plan(_) => "plan",
        acp::SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
        acp::SessionUpdate::CurrentModeUpdate(_) => "current_mode_update",
        acp::SessionUpdate::ConfigOptionUpdate(_) => "config_option_update",
        acp::SessionUpdate::SessionInfoUpdate(_) => "session_info_update",
        _ => "unknown",
    }
}

fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let limit = limit.unwrap_or(u32::MAX) as usize;
    content
        .lines()
        .skip(start)
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n")
}

fn acp_error_to_core(error: acp::Error) -> CoreError {
    CoreError::Adapter(error.to_string())
}

fn acp_error_from_core(error: CoreError) -> acp::Error {
    acp::Error::internal_error().data(error.to_string())
}

fn acp_error_from_io(error: std::io::Error) -> acp::Error {
    acp::Error::internal_error().data(json!(error.to_string()))
}

async fn spawn_acp_terminal(args: &acp::CreateTerminalRequest) -> Result<AcpTerminal, CoreError> {
    if args.cwd.as_ref().is_some_and(|cwd| !cwd.is_absolute()) {
        return Err(CoreError::Adapter(
            "ACP terminal cwd must be absolute".into(),
        ));
    }

    let command_builder = build_terminal_command(args);
    let capture_state = Arc::new(Mutex::new(AcpTerminalCaptureState {
        transcript: String::new(),
        truncated: false,
        output_byte_limit: args
            .output_byte_limit
            .and_then(|limit| usize::try_from(limit).ok()),
        exit_status: None,
    }));
    let pty_system = native_pty_system();
    let pair = tokio::task::spawn_blocking(move || {
        pty_system
            .openpty(portable_pty::PtySize {
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
    let child = tokio::task::spawn_blocking(move || {
        pair.slave
            .spawn_command(command_builder)
            .map_err(|error| CoreError::Adapter(error.to_string()))
    })
    .await
    .map_err(join_error)??;
    let child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>> = Arc::new(Mutex::new(child));
    let killer = {
        let guard = child
            .lock()
            .map_err(|_| CoreError::Adapter("ACP terminal child lock poisoned".into()))?;
        Arc::new(Mutex::new(guard.clone_killer()))
    };
    spawn_terminal_reader(reader, capture_state.clone())?;

    Ok(AcpTerminal {
        capture_state,
        child,
        killer,
    })
}

fn build_terminal_command(args: &acp::CreateTerminalRequest) -> CommandBuilder {
    let mut builder = CommandBuilder::new(&args.command);
    builder.args(&args.args);
    if let Some(cwd) = &args.cwd {
        builder.cwd(cwd);
    }
    for env_var in &args.env {
        builder.env(&env_var.name, &env_var.value);
    }
    builder
}

fn spawn_terminal_reader(
    mut reader: Box<dyn Read + Send>,
    capture_state: Arc<Mutex<AcpTerminalCaptureState>>,
) -> Result<(), CoreError> {
    std::thread::Builder::new()
        .name("polyphony-acp-terminal-reader".into())
        .spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let text = String::from_utf8_lossy(&buffer[..read]);
                        if let Ok(mut state) = capture_state.lock() {
                            state.transcript.push_str(&text);
                            apply_output_byte_limit_to_state(&mut state);
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

fn apply_output_byte_limit(output: &mut String, truncated: &mut bool, limit: Option<usize>) {
    let Some(limit) = limit else {
        return;
    };
    if output.len() <= limit {
        return;
    }
    let start = output
        .char_indices()
        .find_map(|(idx, _)| (output.len().saturating_sub(idx) <= limit).then_some(idx))
        .unwrap_or(output.len());
    *output = output[start..].to_string();
    *truncated = true;
}

fn apply_output_byte_limit_to_state(state: &mut AcpTerminalCaptureState) {
    apply_output_byte_limit(
        &mut state.transcript,
        &mut state.truncated,
        state.output_byte_limit,
    );
}

fn terminal_exit_status_from_portable(
    status: &portable_pty::ExitStatus,
) -> acp::TerminalExitStatus {
    let exit_code = Some(status.exit_code());
    let signal = status.signal().map(ToOwned::to_owned);
    acp::TerminalExitStatus::new()
        .exit_code(exit_code)
        .signal(signal)
}

fn join_error(error: tokio::task::JoinError) -> CoreError {
    CoreError::Adapter(error.to_string())
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
        super::AcpRuntime,
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
    async fn acp_runtime_streams_agent_notifications() {
        let dir = tempdir().unwrap();
        let agent_script = write_fake_acp_agent(
            dir.path(),
            r#"        notify(session_id, {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "hello from acp"}
        })
        respond(msg["id"], {"stopReason": "end_turn"})"#,
        );
        let runtime = AcpRuntime;
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
                        name: "claude_acp".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acp,
                        command: Some(format!("python3 {}", agent_script.display())),
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
                .is_some_and(|message| message.contains("hello from acp"))
        }));
    }

    #[tokio::test]
    async fn acp_runtime_detects_rate_limit_messages() {
        let dir = tempdir().unwrap();
        let agent_script = write_fake_acp_agent(
            dir.path(),
            r#"        notify(session_id, {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "You've hit your limit · resets 2am (Europe/Lisbon)"}
        })
        respond(msg["id"], {"stopReason": "end_turn"})"#,
        );
        let runtime = AcpRuntime;
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
                        name: "opus_acp".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acp,
                        command: Some(format!("python3 {}", agent_script.display())),
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
    async fn acp_runtime_supports_terminal_lifecycle() {
        let dir = tempdir().unwrap();
        let agent_script = write_fake_acp_agent(
            dir.path(),
            r#"        terminal = request("terminal/create", {
            "sessionId": session_id,
            "command": "/bin/sh",
            "args": ["-lc", "printf 'alpha\nomega'"]
        })
        exit_status = request("terminal/wait_for_exit", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        output = request("terminal/output", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        notify(session_id, {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": output["output"] + " exit=" + str(exit_status.get("exitCode"))}
        })
        request("terminal/release", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        respond(msg["id"], {"stopReason": "end_turn"})"#,
        );
        let runtime = AcpRuntime;
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
                        name: "claude_acp".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acp,
                        command: Some(format!("python3 {}", agent_script.display())),
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
            event.message.as_deref().is_some_and(|message| {
                message.contains("alpha") && message.contains("omega") && message.contains("exit=0")
            })
        }));
    }

    #[tokio::test]
    async fn acp_runtime_terminal_output_truncates_on_char_boundary() {
        let dir = tempdir().unwrap();
        let agent_script = write_fake_acp_agent(
            dir.path(),
            r#"        terminal = request("terminal/create", {
            "sessionId": session_id,
            "command": "python3",
            "args": ["-c", "import sys; sys.stdout.write('abc·def')"],
            "outputByteLimit": 5
        })
        request("terminal/wait_for_exit", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        output = request("terminal/output", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        notify(session_id, {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": output["output"] + " truncated=" + str(output["truncated"])}
        })
        request("terminal/release", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        respond(msg["id"], {"stopReason": "end_turn"})"#,
        );
        let runtime = AcpRuntime;
        let (tx, mut rx) = mpsc::unbounded_channel();
        runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: None,
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "claude_acp".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acp,
                        command: Some(format!("python3 {}", agent_script.display())),
                        turn_timeout_ms: 5_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await
            .unwrap();

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| {
            event.message.as_deref().is_some_and(|message| {
                message.contains("·def") && message.contains("truncated=True")
            })
        }));
    }

    #[tokio::test]
    async fn acp_runtime_can_kill_terminal() {
        let dir = tempdir().unwrap();
        let agent_script = write_fake_acp_agent(
            dir.path(),
            r#"        terminal = request("terminal/create", {
            "sessionId": session_id,
            "command": "/bin/sh",
            "args": ["-lc", "sleep 30"]
        })
        request("terminal/kill", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        exit_status = request("terminal/wait_for_exit", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        notify(session_id, {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "killed signal=" + str(exit_status.get("signal")) + " exit=" + str(exit_status.get("exitCode"))}
        })
        request("terminal/release", {
            "sessionId": session_id,
            "terminalId": terminal["terminalId"]
        })
        respond(msg["id"], {"stopReason": "end_turn"})"#,
        );
        let runtime = AcpRuntime;
        let (tx, mut rx) = mpsc::unbounded_channel();
        runtime
            .run(
                AgentRunSpec {
                    issue: test_issue(),
                    attempt: None,
                    workspace_path: dir.path().to_path_buf(),
                    prompt: "hello".into(),
                    max_turns: 1,
                    prior_context: None,
                    agent: AgentDefinition {
                        name: "claude_acp".into(),
                        kind: "claude".into(),
                        transport: AgentTransport::Acp,
                        command: Some(format!("python3 {}", agent_script.display())),
                        turn_timeout_ms: 5_000,
                        ..AgentDefinition::default()
                    },
                },
                tx,
            )
            .await
            .unwrap();

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| {
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("killed"))
        }));
    }

    fn write_fake_acp_agent(dir: &Path, prompt_handler: &str) -> PathBuf {
        let script_path = dir.join("fake_acp_agent.py");
        let mut script = String::new();
        writeln!(&mut script, "#!/usr/bin/env python3").unwrap();
        writeln!(&mut script, "import json").unwrap();
        writeln!(&mut script, "import sys").unwrap();
        writeln!(&mut script).unwrap();
        writeln!(&mut script, "session_id = 'sess-1'").unwrap();
        writeln!(&mut script, "next_id = 100").unwrap();
        writeln!(&mut script).unwrap();
        writeln!(&mut script, "def send(payload):").unwrap();
        writeln!(
            &mut script,
            "    sys.stdout.write(json.dumps(payload) + '\\n')"
        )
        .unwrap();
        writeln!(&mut script, "    sys.stdout.flush()").unwrap();
        writeln!(&mut script).unwrap();
        writeln!(&mut script, "def respond(req_id, result):").unwrap();
        writeln!(
            &mut script,
            "    send({{\"jsonrpc\": \"2.0\", \"id\": req_id, \"result\": result}})"
        )
        .unwrap();
        writeln!(&mut script).unwrap();
        writeln!(&mut script, "def request(method, params):").unwrap();
        writeln!(&mut script, "    global next_id").unwrap();
        writeln!(&mut script, "    req_id = next_id").unwrap();
        writeln!(&mut script, "    next_id += 1").unwrap();
        writeln!(
            &mut script,
            "    send({{\"jsonrpc\": \"2.0\", \"id\": req_id, \"method\": method, \"params\": params}})"
        )
        .unwrap();
        writeln!(&mut script, "    while True:").unwrap();
        writeln!(&mut script, "        line = sys.stdin.readline()").unwrap();
        writeln!(&mut script, "        if not line:").unwrap();
        writeln!(
            &mut script,
            "            raise RuntimeError('client closed while waiting for response')"
        )
        .unwrap();
        writeln!(&mut script, "        message = json.loads(line)").unwrap();
        writeln!(&mut script, "        if message.get('id') == req_id:").unwrap();
        writeln!(&mut script, "            if 'error' in message:").unwrap();
        writeln!(
            &mut script,
            "                raise RuntimeError(message['error'])"
        )
        .unwrap();
        writeln!(&mut script, "            return message['result']").unwrap();
        writeln!(&mut script).unwrap();
        writeln!(&mut script, "def notify(session_id, update):").unwrap();
        writeln!(
            &mut script,
            "    send({{\"jsonrpc\": \"2.0\", \"method\": \"session/update\", \"params\": {{\"sessionId\": session_id, \"update\": update}}}})"
        )
        .unwrap();
        writeln!(&mut script).unwrap();
        writeln!(&mut script, "for line in sys.stdin:").unwrap();
        writeln!(&mut script, "    msg = json.loads(line)").unwrap();
        writeln!(&mut script, "    method = msg.get('method')").unwrap();
        writeln!(&mut script, "    if method == 'initialize':").unwrap();
        writeln!(
            &mut script,
            "        respond(msg['id'], {{'protocolVersion': 'v1', 'agentCapabilities': {{}}, 'authMethods': [], 'agentInfo': {{'name': 'fake-agent', 'version': '0.1.0'}}}})"
        )
        .unwrap();
        writeln!(&mut script, "    elif method == 'session/new':").unwrap();
        writeln!(
            &mut script,
            "        respond(msg['id'], {{'sessionId': session_id}})"
        )
        .unwrap();
        writeln!(&mut script, "    elif method == 'session/prompt':").unwrap();
        writeln!(&mut script, "{prompt_handler}").unwrap();

        let mut file = std::fs::File::create(&script_path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).unwrap();
        script_path
    }
}
