use std::{
    ffi::OsStr,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use {
    axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    },
    polyphony_core::RuntimeSnapshot,
    polyphony_orchestrator::RuntimeCommand,
    serde::{Deserialize, Serialize},
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, UnixListener, UnixStream},
        sync::{mpsc, watch},
        task::JoinHandle,
    },
};

use crate::{Error, bootstrap_support::workflow_root_dir};

const CONTROL_SOCKET_NAME: &str = "daemon.sock";
const DAEMON_PID_NAME: &str = "daemon.pid";
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(500);
const STARTUP_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DaemonRequest {
    Status,
    Snapshot,
    Refresh,
    SetMode {
        mode: String,
    },
    ApproveIssueTrigger {
        issue_id: String,
        source: String,
    },
    DispatchIssue {
        issue_id: String,
        agent_name: Option<String>,
    },
    DispatchPullRequestTrigger {
        trigger_id: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DaemonResponse {
    Status { status: DaemonStatus },
    Snapshot { snapshot: Box<RuntimeSnapshot> },
    Accepted { message: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DaemonStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub log_path: Option<PathBuf>,
    pub http_address: Option<String>,
    pub snapshot: Option<serde_json::Value>,
}

struct PathCleanup {
    path: PathBuf,
}

impl Drop for PathCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn control_socket_path(workflow_path: &Path) -> Result<PathBuf, Error> {
    Ok(workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join(CONTROL_SOCKET_NAME))
}

pub(crate) fn daemon_pid_path(workflow_path: &Path) -> Result<PathBuf, Error> {
    Ok(workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join(DAEMON_PID_NAME))
}

pub(crate) fn latest_log_path(workflow_path: &Path) -> Result<Option<PathBuf>, Error> {
    let log_dir = workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join("logs");
    if !log_dir.exists() {
        return Ok(None);
    }
    let mut entries = std::fs::read_dir(log_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension == "jsonl" || extension == "log")
        })
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries.pop())
}

pub(crate) fn spawn_control_server(
    workflow_path: &Path,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    auth_token: Option<String>,
    http_address: Option<String>,
) -> Result<(JoinHandle<Result<(), Error>>, ControlState), Error> {
    let socket_path = control_socket_path(workflow_path)?;
    let pid_path = daemon_pid_path(workflow_path)?;
    let log_path = latest_log_path(workflow_path)?;
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        match std::os::unix::net::UnixStream::connect(&socket_path) {
            Ok(_) => {
                return Err(Error::Config(format!(
                    "daemon already running at {}",
                    socket_path.display()
                )));
            },
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::NotFound
                        | io::ErrorKind::AddrNotAvailable
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                let _ = std::fs::remove_file(&socket_path);
            },
            Err(error) => return Err(Error::Io(error)),
        }
    }
    let listener = UnixListener::bind(&socket_path)?;
    std::fs::write(&pid_path, format!("{}\n", std::process::id()))?;
    let socket_cleanup = PathCleanup {
        path: socket_path.clone(),
    };
    let pid_cleanup = PathCleanup {
        path: pid_path.clone(),
    };

    let state = ControlState {
        socket_path,
        pid_path,
        log_path,
        http_address,
        snapshot_rx,
        command_tx,
        auth_token,
    };
    let loop_state = state.clone();

    let handle = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let response = handle_unix_request(&mut stream, &loop_state).await;
            let response = match response {
                Ok(response) => response,
                Err(error) => DaemonResponse::Error {
                    message: error.to_string(),
                },
            };
            let body =
                serde_json::to_vec(&response).map_err(|error| Error::Config(error.to_string()))?;
            stream.write_all(&body).await?;
            stream.shutdown().await?;
            let _ = (&socket_cleanup, &pid_cleanup);
        }
    });

    Ok((handle, state))
}

/// Shared state for both Unix socket and HTTP control servers.
#[derive(Clone)]
pub(crate) struct ControlState {
    socket_path: PathBuf,
    pid_path: PathBuf,
    log_path: Option<PathBuf>,
    http_address: Option<String>,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    auth_token: Option<String>,
}

async fn handle_unix_request(
    stream: &mut UnixStream,
    state: &ControlState,
) -> Result<DaemonResponse, Error> {
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await?;
    let request = serde_json::from_slice::<DaemonRequest>(&body)
        .map_err(|error| Error::Config(error.to_string()))?;
    dispatch_request(request, state)
}

fn dispatch_request(request: DaemonRequest, state: &ControlState) -> Result<DaemonResponse, Error> {
    match request {
        DaemonRequest::Status => Ok(DaemonResponse::Status {
            status: running_status(
                &state.socket_path,
                &state.pid_path,
                state.log_path.clone(),
                state.http_address.clone(),
                state.snapshot_rx.borrow().clone(),
            ),
        }),
        DaemonRequest::Snapshot => Ok(DaemonResponse::Snapshot {
            snapshot: Box::new(state.snapshot_rx.borrow().clone()),
        }),
        DaemonRequest::Refresh => {
            send_command(&state.command_tx, RuntimeCommand::Refresh)?;
            Ok(DaemonResponse::Accepted {
                message: "refresh queued".into(),
            })
        },
        DaemonRequest::SetMode { mode } => {
            let mode = parse_dispatch_mode(&mode)?;
            send_command(&state.command_tx, RuntimeCommand::SetMode(mode))?;
            Ok(DaemonResponse::Accepted {
                message: format!("dispatch mode queued: {mode}"),
            })
        },
        DaemonRequest::ApproveIssueTrigger { issue_id, source } => {
            send_command(&state.command_tx, RuntimeCommand::ApproveIssueTrigger {
                issue_id: issue_id.clone(),
                source: source.clone(),
            })?;
            Ok(DaemonResponse::Accepted {
                message: format!("approval queued for {source} issue {issue_id}"),
            })
        },
        DaemonRequest::DispatchIssue {
            issue_id,
            agent_name,
        } => {
            send_command(&state.command_tx, RuntimeCommand::DispatchIssue {
                issue_id: issue_id.clone(),
                agent_name,
            })?;
            Ok(DaemonResponse::Accepted {
                message: format!("dispatch queued for {issue_id}"),
            })
        },
        DaemonRequest::DispatchPullRequestTrigger { trigger_id } => {
            send_command(
                &state.command_tx,
                RuntimeCommand::DispatchPullRequestTrigger {
                    trigger_id: trigger_id.clone(),
                },
            )?;
            Ok(DaemonResponse::Accepted {
                message: format!("pull request trigger queued for {trigger_id}"),
            })
        },
        DaemonRequest::Shutdown => {
            send_command(&state.command_tx, RuntimeCommand::Shutdown)?;
            Ok(DaemonResponse::Accepted {
                message: "shutdown queued".into(),
            })
        },
    }
}

fn send_command(
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
    command: RuntimeCommand,
) -> Result<(), Error> {
    command_tx
        .send(command)
        .map_err(|error| Error::Config(format!("failed to send daemon command: {error}")))
}

fn running_status(
    socket_path: &Path,
    pid_path: &Path,
    log_path: Option<PathBuf>,
    http_address: Option<String>,
    snapshot: RuntimeSnapshot,
) -> DaemonStatus {
    DaemonStatus {
        running: true,
        pid: Some(std::process::id()),
        socket_path: socket_path.to_path_buf(),
        pid_path: pid_path.to_path_buf(),
        log_path,
        http_address,
        snapshot: Some(snapshot_summary(&snapshot)),
    }
}

fn stopped_status(workflow_path: &Path, log_path: Option<PathBuf>) -> Result<DaemonStatus, Error> {
    Ok(DaemonStatus {
        running: false,
        pid: read_pid(workflow_path)?,
        socket_path: control_socket_path(workflow_path)?,
        pid_path: daemon_pid_path(workflow_path)?,
        log_path,
        http_address: None,
        snapshot: None,
    })
}

fn snapshot_summary(snapshot: &RuntimeSnapshot) -> serde_json::Value {
    serde_json::json!({
        "generated_at": snapshot.generated_at,
        "dispatch_mode": snapshot.dispatch_mode,
        "tracker_kind": snapshot.tracker_kind,
        "tracker_connection": snapshot.tracker_connection,
        "counts": snapshot.counts,
        "loading": snapshot.loading,
    })
}

fn parse_dispatch_mode(mode: &str) -> Result<polyphony_core::DispatchMode, Error> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "manual" => Ok(polyphony_core::DispatchMode::Manual),
        "automatic" => Ok(polyphony_core::DispatchMode::Automatic),
        "nightshift" => Ok(polyphony_core::DispatchMode::Nightshift),
        "idle" => Ok(polyphony_core::DispatchMode::Idle),
        other => Err(Error::Config(format!(
            "unknown dispatch mode `{other}`; expected manual, automatic, nightshift, or idle"
        ))),
    }
}

// ── HTTP control server (axum) ────────────────────────────────────────────

pub(crate) async fn bind_http_listener(listen_addr: SocketAddr) -> Result<TcpListener, Error> {
    TcpListener::bind(listen_addr)
        .await
        .map_err(|e| Error::Config(format!("failed to bind HTTP server: {e}")))
}

pub(crate) fn serve_http(
    listener: TcpListener,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    auth_token: Option<String>,
    http_address: Option<String>,
    socket_path: PathBuf,
    pid_path: PathBuf,
    log_path: Option<PathBuf>,
) -> JoinHandle<Result<(), Error>> {
    let bound_addr = listener.local_addr().ok();
    let state = Arc::new(ControlState {
        socket_path,
        pid_path,
        log_path,
        http_address,
        snapshot_rx,
        command_tx,
        auth_token,
    });

    let app = Router::new()
        .route("/api/status", get(http_status))
        .route("/api/snapshot", get(http_snapshot))
        .route("/api/command", post(http_command))
        .with_state(state);

    tracing::debug!(
        http_address = ?bound_addr,
        "starting HTTP control server"
    );

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .map_err(|e| Error::Config(format!("HTTP server error: {e}")))
    })
}

fn check_auth(state: &ControlState, headers: &HeaderMap) -> Result<(), &'static str> {
    let Some(expected) = &state.auth_token else {
        return Ok(());
    };
    let Some(auth_header) = headers.get("authorization") else {
        return Err("missing Authorization header");
    };
    let value = auth_header.to_str().unwrap_or_default();
    let token = value.strip_prefix("Bearer ").unwrap_or(value);
    if token != expected.as_str() {
        return Err("invalid auth token");
    }
    Ok(())
}

async fn http_status(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(message) = check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(DaemonResponse::Error {
                message: message.into(),
            }),
        );
    }
    match dispatch_request(DaemonRequest::Status, &state) {
        Ok(response) => (StatusCode::OK, Json(response)),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DaemonResponse::Error {
                message: error.to_string(),
            }),
        ),
    }
}

async fn http_snapshot(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(message) = check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(DaemonResponse::Error {
                message: message.into(),
            }),
        );
    }
    match dispatch_request(DaemonRequest::Snapshot, &state) {
        Ok(response) => (StatusCode::OK, Json(response)),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DaemonResponse::Error {
                message: error.to_string(),
            }),
        ),
    }
}

async fn http_command(
    State(state): State<Arc<ControlState>>,
    headers: HeaderMap,
    Json(request): Json<DaemonRequest>,
) -> impl IntoResponse {
    if let Err(message) = check_auth(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(DaemonResponse::Error {
                message: message.into(),
            }),
        );
    }
    match dispatch_request(request, &state) {
        Ok(response) => (StatusCode::OK, Json(response)),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DaemonResponse::Error {
                message: error.to_string(),
            }),
        ),
    }
}

pub(crate) async fn request_status(workflow_path: &Path) -> Result<DaemonStatus, Error> {
    let log_path = latest_log_path(workflow_path)?;
    match send_request(workflow_path, DaemonRequest::Status).await {
        Ok(DaemonResponse::Status { status }) => Ok(status),
        Ok(other) => Err(Error::Config(format!(
            "unexpected daemon response: {}",
            response_kind(&other)
        ))),
        Err(error) if daemon_not_running(&error) => stopped_status(workflow_path, log_path),
        Err(error) => Err(error),
    }
}

pub(crate) async fn request_snapshot(workflow_path: &Path) -> Result<RuntimeSnapshot, Error> {
    match send_request(workflow_path, DaemonRequest::Snapshot).await? {
        DaemonResponse::Snapshot { snapshot } => Ok(*snapshot),
        other => Err(Error::Config(format!(
            "unexpected daemon response: {}",
            response_kind(&other)
        ))),
    }
}

pub(crate) async fn send_control_request(
    workflow_path: &Path,
    request: DaemonRequest,
) -> Result<DaemonResponse, Error> {
    send_request(workflow_path, request).await
}

async fn send_request(
    workflow_path: &Path,
    request: DaemonRequest,
) -> Result<DaemonResponse, Error> {
    let socket_path = control_socket_path(workflow_path)?;
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .map_err(map_connect_error)?;
    let body = serde_json::to_vec(&request).map_err(|error| Error::Config(error.to_string()))?;
    stream.write_all(&body).await?;
    AsyncWriteExt::shutdown(&mut stream).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    serde_json::from_slice(&response).map_err(|error| Error::Config(error.to_string()))
}

fn map_connect_error(error: io::Error) -> Error {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::AddrNotAvailable
    ) {
        return Error::Config("daemon is not running".into());
    }
    Error::Io(error)
}

fn daemon_not_running(error: &Error) -> bool {
    matches!(error, Error::Config(message) if message == "daemon is not running")
}

fn response_kind(response: &DaemonResponse) -> &'static str {
    match response {
        DaemonResponse::Status { .. } => "status",
        DaemonResponse::Snapshot { .. } => "snapshot",
        DaemonResponse::Accepted { .. } => "accepted",
        DaemonResponse::Error { .. } => "error",
    }
}

fn read_pid(workflow_path: &Path) -> Result<Option<u32>, Error> {
    let path = daemon_pid_path(workflow_path)?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(path)?;
    let pid = contents.trim().parse::<u32>().ok();
    Ok(pid)
}

pub(crate) async fn start_daemon_process(
    workflow_path: &Path,
    log_json: bool,
    sqlite_url: Option<&str>,
) -> Result<DaemonStatus, Error> {
    let socket_path = control_socket_path(workflow_path)?;
    if let Ok(status) = request_status(workflow_path).await
        && status.running
    {
        return Err(Error::Config(format!(
            "daemon already running at {}",
            socket_path.display()
        )));
    }
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let current_dir = std::env::current_dir()?;
    let absolute_workflow_path = if workflow_path.is_absolute() {
        workflow_path.to_path_buf()
    } else {
        current_dir.join(workflow_path)
    };
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .current_dir(&current_dir)
        .arg("--directory")
        .arg(&current_dir)
        .arg("--workflow")
        .arg(&absolute_workflow_path)
        .arg("--no-tui");
    if log_json {
        command.arg("--log-json");
    }
    if let Some(sqlite_url) = sqlite_url {
        command.arg("--sqlite-url").arg(sqlite_url);
    }
    command
        .arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;

    let started = tokio::time::Instant::now();
    loop {
        match request_status(&absolute_workflow_path).await {
            Ok(status)
                if status.running && daemon_snapshot_is_ready(&absolute_workflow_path).await? =>
            {
                return Ok(status);
            },
            Ok(_) => {},
            Err(error) if daemon_not_running(&error) => {},
            Err(error) => return Err(error),
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::Config(format!(
                "daemon exited early with status {status}; inspect {}",
                latest_log_path(&absolute_workflow_path)?
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| ".polyphony/logs".into())
            )));
        }
        if started.elapsed() >= STARTUP_WAIT_TIMEOUT {
            return Err(Error::Config(format!(
                "timed out waiting for daemon startup at {}",
                socket_path.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn daemon_snapshot_is_ready(workflow_path: &Path) -> Result<bool, Error> {
    let snapshot = request_snapshot(workflow_path).await?;
    Ok(snapshot.cadence.last_tracker_poll_at.is_some()
        && !snapshot.loading.fetching_issues
        && !snapshot.loading.fetching_budgets
        && !snapshot.loading.fetching_models
        && !snapshot.loading.reconciling)
}

pub(crate) async fn print_daemon_logs(
    workflow_path: &Path,
    lines: usize,
    follow: bool,
) -> Result<(), Error> {
    let Some(mut log_path) = latest_log_path(workflow_path)? else {
        return Err(Error::Config("no daemon logs found".into()));
    };
    let mut seen = 0usize;
    loop {
        if !log_path.exists()
            && let Some(new_path) = latest_log_path(workflow_path)?
        {
            log_path = new_path;
            seen = 0;
        }
        let contents = tokio::fs::read_to_string(&log_path)
            .await
            .unwrap_or_default();
        let entries = contents.lines().collect::<Vec<_>>();
        if !follow {
            if entries.is_empty() {
                println!("no log lines found in {}", log_path.display());
                return Ok(());
            }
            let start = entries.len().saturating_sub(lines);
            for line in &entries[start..] {
                println!("{line}");
            }
            return Ok(());
        }
        for line in entries.iter().skip(seen) {
            println!("{line}");
        }
        seen = entries.len();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(LOG_POLL_INTERVAL) => {}
        }
    }
}

#[cfg(all(test, feature = "mock"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use {
        polyphony_orchestrator::RuntimeService,
        polyphony_workflow::load_workflow,
        tokio::sync::{mpsc, watch},
    };

    use crate::daemon::{
        DaemonRequest, DaemonResponse, control_socket_path, request_status, send_control_request,
        spawn_control_server,
    };

    fn unique_temp_path(name: &str, extension: &str) -> PathBuf {
        PathBuf::from("/tmp").join(format!(
            "pd-{name}-{}.{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            extension
        ))
    }

    fn snapshot_rx() -> watch::Receiver<polyphony_core::RuntimeSnapshot> {
        let workflow_path = unique_temp_path("workflow", "md");
        fs::write(
            &workflow_path,
            format!(
                "---\ntracker:\n  kind: mock\nworkspace:\n  root: {}\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nMock prompt\n",
                std::env::temp_dir().display()
            ),
        )
        .unwrap();
        let workflow = load_workflow(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);
        let tracker = polyphony_issue_mock::MockTracker::seeded_demo();
        let agent = polyphony_issue_mock::MockAgentRuntime::new(tracker.clone());
        let (_tx, workflow_rx) = watch::channel(workflow);
        let (_service, handle) = RuntimeService::new(
            Arc::new(tracker),
            None,
            Arc::new(agent),
            Arc::new(polyphony_git::GitWorkspaceProvisioner),
            None,
            None,
            None,
            None,
            None,
            None,
            workflow_rx,
        );
        handle.snapshot_rx
    }

    #[tokio::test]
    async fn control_server_reports_running_status() {
        let repo_root = unique_temp_path("status", "d");
        fs::create_dir_all(repo_root.join(".polyphony")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        fs::write(&workflow_path, "---\n# test\n").unwrap();
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let (server, _state) =
            spawn_control_server(&workflow_path, snapshot_rx(), command_tx, None, None).unwrap();

        let status = request_status(&workflow_path).await.unwrap();

        assert!(status.running);
        assert_eq!(
            status.socket_path,
            control_socket_path(&workflow_path).unwrap()
        );
        server.abort();
        let _ = server.await;
        let _ = fs::remove_dir_all(&repo_root);
    }

    #[tokio::test]
    async fn control_server_forwards_refresh_commands() {
        let repo_root = unique_temp_path("refresh", "d");
        fs::create_dir_all(repo_root.join(".polyphony")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        fs::write(&workflow_path, "---\n# test\n").unwrap();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (server, _state) =
            spawn_control_server(&workflow_path, snapshot_rx(), command_tx, None, None).unwrap();

        let response = send_control_request(&workflow_path, DaemonRequest::Refresh)
            .await
            .unwrap();

        assert!(matches!(response, DaemonResponse::Accepted { .. }));
        assert!(matches!(
            command_rx.recv().await,
            Some(polyphony_orchestrator::RuntimeCommand::Refresh)
        ));
        server.abort();
        let _ = server.await;
        let _ = fs::remove_dir_all(&repo_root);
    }

    #[tokio::test]
    async fn control_server_forwards_issue_approval_commands() {
        let repo_root = unique_temp_path("approve", "d");
        fs::create_dir_all(repo_root.join(".polyphony")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        fs::write(&workflow_path, "---\n# test\n").unwrap();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (server, _state) =
            spawn_control_server(&workflow_path, snapshot_rx(), command_tx, None, None).unwrap();

        let response = send_control_request(&workflow_path, DaemonRequest::ApproveIssueTrigger {
            issue_id: "7".into(),
            source: "github".into(),
        })
        .await
        .unwrap();

        assert!(matches!(response, DaemonResponse::Accepted { .. }));
        assert!(matches!(
            command_rx.recv().await,
            Some(polyphony_orchestrator::RuntimeCommand::ApproveIssueTrigger {
                issue_id,
                source,
            }) if issue_id == "7" && source == "github"
        ));
        server.abort();
        let _ = server.await;
        let _ = fs::remove_dir_all(&repo_root);
    }
}
