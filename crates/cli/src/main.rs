use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use polyphony_core::{NetworkCache, StateStore, WorkspaceProvisioner};
use polyphony_orchestrator::{RuntimeComponentFactory, RuntimeService, spawn_workflow_watcher};
use polyphony_workflow::{
    ensure_repo_agent_prompt_files, ensure_user_config_file, load_workflow_with_user_config,
    user_config_path,
};
use thiserror::Error;

type ShutdownFuture = Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send>>;
#[cfg(feature = "beads")]
const BEADS_SUPPLEMENTAL_PREFIX: &str = "beads:";
#[cfg(feature = "github")]
const GITHUB_SUPPLEMENTAL_PREFIX: &str = "github:";

#[derive(Debug, Parser)]
#[command(name = "polyphony")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the repository or directory to operate in.
    #[arg(short = 'C', long = "directory", value_name = "DIR", global = true)]
    directory: Option<PathBuf>,
    /// Path to the workflow file (default: WORKFLOW.md)
    #[arg(long = "workflow", value_name = "WORKFLOW", global = true)]
    workflow_path: Option<PathBuf>,
    #[arg(long, global = true)]
    no_tui: bool,
    #[arg(long, global = true)]
    log_json: bool,
    #[arg(long, env = "POLYPHONY_SQLITE_URL", global = true)]
    sqlite_url: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
enum Commands {
    /// Run and control a headless Polyphony daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Manage tracker issues
    Issue {
        #[command(subcommand)]
        action: IssueAction,
    },
    /// Read Polyphony runtime data as JSON
    Data {
        #[command(subcommand)]
        action: DataAction,
    },
    /// Show and validate the merged configuration
    Config {
        /// Output full config as JSON
        #[arg(long)]
        json: bool,
    },
    /// Check agent commands, models_command, and configuration health
    Doctor,
}

#[derive(Debug, Clone, Subcommand)]
enum DaemonAction {
    /// Run the daemon in the foreground
    Run,
    /// Start the daemon in the background
    Start,
    /// Stop the running daemon
    Stop,
    /// Show daemon status
    Status,
    /// Print daemon logs
    Logs {
        #[arg(long, default_value_t = 50)]
        lines: usize,
        #[arg(long)]
        follow: bool,
    },
    /// Request a tracker refresh
    Refresh,
    /// Change daemon dispatch mode
    Mode { mode: DispatchModeArg },
    /// Manually dispatch an issue
    Dispatch {
        issue_id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Approve a waiting issue trigger so automatic pickup can use it
    Approve {
        issue_id: String,
        #[arg(long)]
        source: Option<String>,
    },
    /// Manually dispatch a pull request trigger
    Trigger { trigger_id: String },
    /// Emit the live daemon snapshot
    Snapshot,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum DispatchModeArg {
    Manual,
    Automatic,
    Nightshift,
    Idle,
}

impl DispatchModeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
            Self::Nightshift => "nightshift",
            Self::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
enum IssueAction {
    /// Create a new issue
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        #[arg(long, value_delimiter = ',')]
        labels: Vec<String>,
        #[arg(long)]
        parent: Option<String>,
    },
    /// Update an existing issue
    Update {
        /// Issue identifier (e.g. GH-42, beads ID)
        identifier: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        #[arg(long, value_delimiter = ',')]
        labels: Option<Vec<String>>,
    },
    /// List issues from the tracker
    List {
        #[arg(long, value_delimiter = ',')]
        state: Option<Vec<String>>,
        #[arg(long)]
        all: bool,
    },
    /// Show issue details
    Show {
        /// Issue identifier
        identifier: String,
    },
    /// Post a comment to an issue
    Comment {
        /// Issue identifier
        identifier: String,
        #[arg(long)]
        body: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum DataAction {
    /// Emit the full runtime snapshot
    Snapshot,
    /// Emit snapshot counts
    Counts,
    /// Emit runtime cadence information
    Cadence,
    /// Emit visible issues
    Issues,
    /// Emit visible triggers
    Triggers,
    /// Emit running agents
    Running,
    /// Emit historical agent runs
    History,
    /// Emit retrying agents
    Retrying,
    /// Emit agent model catalogs
    Catalogs,
    /// Emit recent runtime events
    Events {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Emit running agents, history, retries, and model catalogs
    Agents,
    /// Emit pipeline tasks
    Tasks,
    /// Emit movements
    Movements,
    /// Emit workspace directories with runtime annotations
    Workspaces,
    /// Emit budget snapshots
    Budgets,
    /// Emit Codex aggregate totals
    CodexTotals,
    /// Emit provider rate limit state
    RateLimits,
    /// Emit active throttle windows
    Throttles,
    /// Emit saved contexts
    Contexts,
    /// Emit runtime loading state
    Loading,
    /// Emit tracker status and dispatch metadata
    Tracker,
    /// Emit configured agent profile names
    Profiles,
}

#[derive(Debug, Error)]
enum Error {
    #[error("core error: {0}")]
    Core(#[from] polyphony_core::Error),
    #[error("workflow error: {0}")]
    Workflow(#[from] polyphony_workflow::Error),
    #[error("runtime error: {0}")]
    Runtime(#[from] polyphony_orchestrator::Error),
    #[error("tui error: {0}")]
    Tui(#[from] ui_support::TuiError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Config(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowBootstrap {
    Ready,
    Canceled,
}

#[tokio::main]
async fn main() {
    if let Err(error) = try_main().await {
        eprintln!("{}", format_fatal_error(&error));
        std::process::exit(1);
    }
}

mod bootstrap_support;
mod commands;
mod daemon;
mod errors;
mod prelude;
mod tracing_support;
mod tracker_factory;
mod ui_support;

#[cfg(test)]
mod tests;

use crate::{
    bootstrap_support::{
        ensure_bootstrapped_workflow, maybe_seed_repo_config_with_github_detection,
        workflow_root_dir,
    },
    commands::{handle_config_command, handle_doctor_command, handle_issue_command},
    daemon::{DaemonRequest, send_control_request, start_daemon_process},
    errors::format_fatal_error,
    tracing_support::{
        TelemetryGuard, TracingOutput, init_run_log_sink, init_tracing, load_historical_log_lines,
        run_operator_surface,
    },
    tracker_factory::build_runtime_components,
    ui_support::{LogBuffer, prompt_workflow_initialization, run as run_tui, tui_available},
};

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn try_main() -> Result<(), Error> {
    install_rustls_crypto_provider();
    let cli = Cli::parse();
    let daemon_foreground = matches!(
        cli.command,
        Some(Commands::Daemon {
            action: DaemonAction::Run
        })
    );
    let tui_mode = !daemon_foreground && tui_available() && !cli.no_tui;
    if let Some(dir) = &cli.directory {
        std::env::set_current_dir(dir).map_err(|e| {
            Error::Config(format!("cannot change to directory {}: {e}", dir.display()))
        })?;
    }
    let workflow_path = cli
        .workflow_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("WORKFLOW.md"));

    // For issue/config subcommands, skip TUI/tracing setup — just load the workflow and dispatch.
    if let Some(Commands::Config { json }) = &cli.command {
        let user_config_path = user_config_path()?;
        let workflow = load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return handle_config_command(&workflow, &workflow_path, *json);
    }
    if let Some(Commands::Doctor) = &cli.command {
        let user_config_path = user_config_path()?;
        let workflow = load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return handle_doctor_command(&workflow);
    }
    if let Some(Commands::Data { action }) = &cli.command {
        let user_config_path = user_config_path()?;
        let workflow = load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return commands::handle_data_command(
            action.clone(),
            &workflow,
            &workflow_path,
            cli.sqlite_url.as_deref(),
        )
        .await;
    }
    if let Some(Commands::Issue { action }) = &cli.command {
        let user_config_path = user_config_path()?;
        let workflow = load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return handle_issue_command(action.clone(), &workflow).await;
    }
    if let Some(Commands::Daemon { action }) = &cli.command {
        return handle_daemon_command(action.clone(), &cli, &workflow_path).await;
    }

    let Some(runtime) = start_runtime(&cli, &workflow_path, tui_mode).await? else {
        return Ok(());
    };

    run_operator_surface(
        !tui_mode,
        runtime.handle.snapshot_rx.clone(),
        runtime.handle.command_tx.clone(),
        runtime.tui_logs,
        runtime.tracing_output,
        |snapshot_rx, command_tx, tui_logs| Box::pin(run_tui(snapshot_rx, command_tx, tui_logs)),
        Box::pin(tokio::signal::ctrl_c()),
    )
    .await?;

    // The TUI shows a "Leaving..." modal and waits up to 3 seconds for the
    // service to finish. By the time we get here, the service is either done
    // or we should just exit.
    tokio::select! {
        result = runtime.service_task => {
            result.map_err(|error| Error::Config(error.to_string()))??;
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
    }
    Ok(())
}

struct StartedRuntime {
    handle: polyphony_orchestrator::RuntimeHandle,
    service_task: tokio::task::JoinHandle<Result<(), polyphony_orchestrator::Error>>,
    tracing_output: TracingOutput,
    tui_logs: LogBuffer,
    _telemetry: TelemetryGuard,
}

async fn handle_daemon_command(
    action: DaemonAction,
    cli: &Cli,
    workflow_path: &Path,
) -> Result<(), Error> {
    match action {
        DaemonAction::Run => run_daemon(cli, workflow_path).await,
        DaemonAction::Start => {
            print_json(
                &start_daemon_process(workflow_path, cli.log_json, cli.sqlite_url.as_deref())
                    .await?,
            )?;
            Ok(())
        },
        DaemonAction::Stop => {
            print_json(&send_control_request(workflow_path, DaemonRequest::Shutdown).await?)?;
            Ok(())
        },
        DaemonAction::Status => {
            print_json(&daemon::request_status(workflow_path).await?)?;
            Ok(())
        },
        DaemonAction::Logs { lines, follow } => {
            daemon::print_daemon_logs(workflow_path, lines, follow).await
        },
        DaemonAction::Refresh => {
            print_json(&send_control_request(workflow_path, DaemonRequest::Refresh).await?)?;
            Ok(())
        },
        DaemonAction::Mode { mode } => {
            print_json(
                &send_control_request(workflow_path, DaemonRequest::SetMode {
                    mode: mode.as_str().into(),
                })
                .await?,
            )?;
            Ok(())
        },
        DaemonAction::Dispatch { issue_id, agent } => {
            print_json(
                &send_control_request(workflow_path, DaemonRequest::DispatchIssue {
                    issue_id,
                    agent_name: agent,
                    directives: None,
                })
                .await?,
            )?;
            Ok(())
        },
        DaemonAction::Approve { issue_id, source } => {
            let source = source.unwrap_or_else(|| infer_issue_source(&issue_id).to_string());
            print_json(
                &send_control_request(workflow_path, DaemonRequest::ApproveIssueTrigger {
                    issue_id,
                    source,
                })
                .await?,
            )?;
            Ok(())
        },
        DaemonAction::Trigger { trigger_id } => {
            print_json(
                &send_control_request(workflow_path, DaemonRequest::DispatchPullRequestTrigger {
                    trigger_id,
                })
                .await?,
            )?;
            Ok(())
        },
        DaemonAction::Snapshot => {
            print_json(&daemon::request_snapshot(workflow_path).await?)?;
            Ok(())
        },
    }
}

fn infer_issue_source(issue_id: &str) -> &'static str {
    match issue_id.split(':').next() {
        Some("github") => "github",
        Some("gitlab") => "gitlab",
        Some("beads") => "beads",
        Some("linear") => "linear",
        Some("mock") => "mock",
        _ => "github",
    }
}

async fn run_daemon(cli: &Cli, workflow_path: &Path) -> Result<(), Error> {
    let Some(runtime) = start_runtime(cli, workflow_path, false).await? else {
        return Ok(());
    };

    // Resolve daemon HTTP config before spawning servers so the http_address
    // is known to both Unix socket and HTTP status responses.
    let daemon_config = load_daemon_config(workflow_path);
    let auth_token = resolve_daemon_auth_token(&daemon_config);

    let http_listener = if daemon_config.listen_port > 0 {
        let listen_addr: std::net::SocketAddr = format!(
            "{}:{}",
            daemon_config.listen_address, daemon_config.listen_port
        )
        .parse()
        .map_err(|e: std::net::AddrParseError| Error::Config(e.to_string()))?;
        Some(daemon::bind_http_listener(listen_addr).await?)
    } else {
        None
    };
    let http_address = http_listener
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.to_string());

    let (control_server, _control_state) = daemon::spawn_control_server(
        workflow_path,
        runtime.handle.snapshot_rx.clone(),
        runtime.handle.command_tx.clone(),
        auth_token.clone(),
        http_address.clone(),
    )?;
    tracing::info!(
        socket_path = %daemon::control_socket_path(workflow_path)?.display(),
        "daemon control server ready"
    );

    let http_server = if let Some(listener) = http_listener {
        let bound_addr = listener
            .local_addr()
            .map_err(|e| Error::Config(format!("failed to get local address: {e}")))?;
        let handle = daemon::serve_http(
            listener,
            runtime.handle.snapshot_rx.clone(),
            runtime.handle.command_tx.clone(),
            auth_token,
            http_address,
            daemon::control_socket_path(workflow_path)?,
            daemon::daemon_pid_path(workflow_path)?,
            daemon::latest_log_path(workflow_path)?,
        );
        tracing::info!(
            http_address = %bound_addr,
            "daemon HTTP control server ready"
        );
        Some(handle)
    } else {
        None
    };

    let mut service_task = runtime.service_task;
    tokio::select! {
        result = &mut service_task => {
            control_server.abort();
            let _ = control_server.await;
            if let Some(http) = http_server { http.abort(); let _ = http.await; }
            result.map_err(|error| Error::Config(error.to_string()))??;
            Ok(())
        }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            let _ = runtime.handle.command_tx.send(polyphony_orchestrator::RuntimeCommand::Shutdown);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let _ = service_task.await;
            }).await;
            control_server.abort();
            let _ = control_server.await;
            if let Some(http) = http_server { http.abort(); let _ = http.await; }
            Ok(())
        }
    }
}

fn load_daemon_config(workflow_path: &Path) -> polyphony_workflow::DaemonConfig {
    let user_config_path = user_config_path().ok();
    match load_workflow_with_user_config(workflow_path, user_config_path.as_deref()) {
        Ok(workflow) => workflow.config.daemon,
        Err(_) => polyphony_workflow::DaemonConfig::default(),
    }
}

fn resolve_daemon_auth_token(config: &polyphony_workflow::DaemonConfig) -> Option<String> {
    // Environment variable takes precedence.
    if let Ok(token) = std::env::var("POLYPHONY_DAEMON_TOKEN")
        && !token.is_empty()
    {
        return Some(token);
    }
    // Fall back to config value.
    config.auth_token.clone().filter(|t| !t.is_empty())
}

async fn start_runtime(
    cli: &Cli,
    workflow_path: &Path,
    tui_mode: bool,
) -> Result<Option<StartedRuntime>, Error> {
    let historical_logs = if !tui_mode {
        Vec::new()
    } else {
        match load_historical_log_lines(workflow_path) {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("polyphony: failed to load historical logs: {error}");
                Vec::new()
            },
        }
    };
    let log_file_sink = match init_run_log_sink(workflow_path) {
        Ok(sink) => Some(sink),
        Err(error) => {
            eprintln!("polyphony: failed to initialize persistent log file: {error}");
            None
        },
    };
    let tui_logs = LogBuffer::from_lines(historical_logs);
    let tracing_output = if !tui_mode {
        TracingOutput::stderr(log_file_sink)
    } else {
        TracingOutput::tui(tui_logs.clone(), log_file_sink)
    };
    let telemetry = init_tracing(cli.log_json, tui_mode, tracing_output.clone());
    tracing::info!(
        workflow_path = %workflow_path.display(),
        no_tui = !tui_mode,
        tui_compiled = tui_available(),
        sqlite_enabled = cli.sqlite_url.is_some(),
        "starting polyphony"
    );
    if !tui_available() && !cli.no_tui {
        eprintln!("polyphony: tui support is disabled for this build, continuing headless.");
        tracing::warn!("tui support is disabled for this build; continuing headless");
    }
    let user_config_path = user_config_path()?;
    if ensure_user_config_file(&user_config_path)? {
        tracing::info!(
            config_path = %user_config_path.display(),
            "created default user config file"
        );
    }
    if ensure_bootstrapped_workflow(workflow_path, !tui_mode, |workflow_path| {
        Ok(prompt_workflow_initialization(workflow_path)?)
    })? == WorkflowBootstrap::Canceled
    {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            "workflow initialization canceled"
        );
        return Ok(None);
    }
    let created_agent_prompts = ensure_repo_agent_prompt_files(workflow_path)?;
    if !created_agent_prompts.is_empty() {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            created = created_agent_prompts.len(),
            "created default repo agent prompt files"
        );
    }
    let (repo_config_path, first_run_no_github) =
        maybe_seed_repo_config_with_github_detection(workflow_path, Some(&user_config_path))?;
    if first_run_no_github {
        eprintln!("Edit polyphony.toml to configure your tracker, then restart polyphony.");
        return Ok(None);
    }
    let workflow = load_workflow_with_user_config(workflow_path, Some(&user_config_path))?;
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(|workflow| {
        build_runtime_components(workflow)
            .map_err(|error: Error| polyphony_core::Error::Adapter(error.to_string()))
    });
    let components = build_runtime_components(&workflow)?;
    let provisioner: Arc<dyn WorkspaceProvisioner> =
        Arc::new(polyphony_git::GitWorkspaceProvisioner);
    let store = build_store(workflow_path, cli.sqlite_url.as_deref()).await?;
    let cache: Option<Arc<dyn NetworkCache>> = {
        let cache_path = workflow_root_dir(workflow_path)?
            .join(".polyphony")
            .join("cache.json");
        Some(Arc::new(polyphony_core::file_cache::FileNetworkCache::new(
            cache_path,
        )))
    };
    let (workflow_tx, workflow_rx) = tokio::sync::watch::channel(workflow.clone());
    let (service, handle) = RuntimeService::new(
        components.tracker,
        components.pull_request_trigger_source,
        components.agent,
        provisioner,
        components.committer,
        components.pull_request_manager,
        components.pull_request_commenter,
        components.feedback,
        store,
        cache,
        workflow_rx,
    );
    let service = service.with_workflow_reload(
        workflow_path.to_path_buf(),
        Some(user_config_path.clone()),
        workflow_tx.clone(),
        component_factory,
    );
    let _watcher = spawn_workflow_watcher(
        workflow_path.to_path_buf(),
        Some(user_config_path),
        repo_config_path,
        handle.command_tx.clone(),
    )?;
    let service_task = tokio::spawn(service.run());
    Ok(Some(StartedRuntime {
        handle,
        service_task,
        tracing_output,
        tui_logs,
        _telemetry: telemetry,
    }))
}

async fn build_store(
    workflow_path: &Path,
    sqlite_url: Option<&str>,
) -> Result<Option<Arc<dyn StateStore>>, Error> {
    #[cfg(feature = "sqlite")]
    if let Some(url) = sqlite_url {
        let store = polyphony_sqlite::SqliteStateStore::connect(url)
            .await
            .map_err(|error| Error::Config(error.to_string()))?;
        return Ok(Some(Arc::new(store)));
    }

    #[cfg(not(feature = "sqlite"))]
    if sqlite_url.is_some() {
        return Err(Error::Config(
            "sqlite support is disabled for this build".into(),
        ));
    }

    let state_path = workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join("state.json");
    Ok(Some(Arc::new(
        polyphony_core::file_store::JsonStateStore::new(state_path),
    )))
}

fn print_json(value: &impl serde::Serialize) -> Result<(), Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| Error::Config(error.to_string()))?
    );
    Ok(())
}
