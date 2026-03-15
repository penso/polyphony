use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use {
    clap::{Parser, Subcommand},
    polyphony_core::{NetworkCache, StateStore, WorkspaceProvisioner},
    polyphony_orchestrator::{RuntimeComponentFactory, RuntimeService, spawn_workflow_watcher},
    polyphony_workflow::{
        ensure_repo_agent_prompt_files, ensure_user_config_file, load_workflow_with_user_config,
        user_config_path,
    },
    thiserror::Error,
};

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
    errors::format_fatal_error,
    tracing_support::{
        TracingOutput, init_run_log_sink, init_tracing, load_historical_log_lines,
        run_operator_surface,
    },
    tracker_factory::build_runtime_components,
    ui_support::{LogBuffer, prompt_workflow_initialization, run as run_tui, tui_available},
};

async fn try_main() -> Result<(), Error> {
    let cli = Cli::parse();
    let tui_mode = tui_available() && !cli.no_tui;
    if let Some(dir) = &cli.directory {
        std::env::set_current_dir(dir).map_err(|e| {
            Error::Config(format!("cannot change to directory {}: {e}", dir.display()))
        })?;
    }
    let workflow_path = cli
        .workflow_path
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

    let historical_logs = if !tui_mode {
        Vec::new()
    } else {
        match load_historical_log_lines(&workflow_path) {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("polyphony: failed to load historical logs: {error}");
                Vec::new()
            },
        }
    };
    let log_file_sink = match init_run_log_sink(&workflow_path) {
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
    let _telemetry = init_tracing(cli.log_json, tui_mode, tracing_output.clone());
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
    if ensure_bootstrapped_workflow(&workflow_path, !tui_mode, |workflow_path| {
        Ok(prompt_workflow_initialization(workflow_path)?)
    })? == WorkflowBootstrap::Canceled
    {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            "workflow initialization canceled"
        );
        return Ok(());
    }
    let created_agent_prompts = ensure_repo_agent_prompt_files(&workflow_path)?;
    if !created_agent_prompts.is_empty() {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            created = created_agent_prompts.len(),
            "created default repo agent prompt files"
        );
    }
    let (repo_config_path, first_run_no_github) =
        maybe_seed_repo_config_with_github_detection(&workflow_path, Some(&user_config_path))?;
    if first_run_no_github {
        eprintln!("Edit polyphony.toml to configure your tracker, then restart polyphony.");
        return Ok(());
    }
    let workflow = load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(|workflow| {
        build_runtime_components(workflow)
            .map_err(|error: Error| polyphony_core::Error::Adapter(error.to_string()))
    });
    let components = build_runtime_components(&workflow)?;
    let provisioner: Arc<dyn WorkspaceProvisioner> =
        Arc::new(polyphony_git::GitWorkspaceProvisioner);
    let store = build_store(&workflow_path, cli.sqlite_url.as_deref()).await?;
    let cache: Option<Arc<dyn NetworkCache>> = {
        let cache_path = workflow_root_dir(&workflow_path)?
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
        workflow_path.clone(),
        Some(user_config_path.clone()),
        workflow_tx.clone(),
        component_factory,
    );
    let _watcher = spawn_workflow_watcher(
        workflow_path.clone(),
        Some(user_config_path.clone()),
        repo_config_path,
        handle.command_tx.clone(),
    )?;
    let service_task = tokio::spawn(service.run());

    run_operator_surface(
        !tui_mode,
        handle.snapshot_rx.clone(),
        handle.command_tx.clone(),
        tui_logs,
        tracing_output,
        |snapshot_rx, command_tx, tui_logs| Box::pin(run_tui(snapshot_rx, command_tx, tui_logs)),
        Box::pin(tokio::signal::ctrl_c()),
    )
    .await?;

    // The TUI shows a "Leaving..." modal and waits up to 3 seconds for the
    // service to finish. By the time we get here, the service is either done
    // or we should just exit.
    tokio::select! {
        result = service_task => {
            result.map_err(|error| Error::Config(error.to_string()))??;
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
    }
    Ok(())
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
