use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use factoryrs_core::{AgentRuntime, IssueTracker, StateStore, WorkspaceProvisioner};
use factoryrs_orchestrator::{RuntimeCommand, RuntimeService, spawn_workflow_watcher};
use factoryrs_workflow::load_workflow;
use thiserror::Error;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "factoryrs")]
struct Cli {
    #[arg(value_name = "WORKFLOW", default_value = "WORKFLOW.md")]
    workflow_path: PathBuf,
    #[arg(long)]
    no_tui: bool,
    #[arg(long)]
    log_json: bool,
    #[arg(long, env = "FACTORYRS_SQLITE_URL")]
    sqlite_url: Option<String>,
}

#[derive(Debug, Error)]
enum Error {
    #[error("core error: {0}")]
    Core(#[from] factoryrs_core::Error),
    #[error("workflow error: {0}")]
    Workflow(#[from] factoryrs_workflow::Error),
    #[error("runtime error: {0}")]
    Runtime(#[from] factoryrs_orchestrator::Error),
    #[error("tui error: {0}")]
    Tui(#[from] factoryrs_tui::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Config(String),
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = Cli::parse();
    init_tracing(cli.log_json);
    let workflow = load_workflow(&cli.workflow_path)?;
    let (tracker, agent) = build_runtime_components(&workflow)?;
    let provisioner: Arc<dyn WorkspaceProvisioner> =
        Arc::new(factoryrs_git::GitWorkspaceProvisioner);
    let store = build_store(cli.sqlite_url.as_deref()).await?;
    let (workflow_tx, workflow_rx) = tokio::sync::watch::channel(workflow.clone());
    let (service, handle) = RuntimeService::new(tracker, agent, provisioner, store, workflow_rx);
    let _watcher = spawn_workflow_watcher(
        cli.workflow_path.clone(),
        workflow_tx,
        handle.command_tx.clone(),
    )?;
    let service_task = tokio::spawn(service.run());

    if cli.no_tui {
        tokio::signal::ctrl_c().await?;
        let _ = handle.command_tx.send(RuntimeCommand::Shutdown);
    } else {
        factoryrs_tui::run(handle.snapshot_rx.clone(), handle.command_tx.clone()).await?;
        let _ = handle.command_tx.send(RuntimeCommand::Shutdown);
    }

    service_task
        .await
        .map_err(|error| Error::Config(error.to_string()))??;
    Ok(())
}

fn init_tracing(log_json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if log_json {
        builder.json().init();
    } else {
        builder.compact().init();
    }
}

#[allow(unused_variables)]
fn build_runtime_components(
    workflow: &factoryrs_workflow::LoadedWorkflow,
) -> Result<(Arc<dyn IssueTracker>, Arc<dyn AgentRuntime>), Error> {
    #[cfg(feature = "mock")]
    if workflow.config.tracker.kind == "mock" && workflow.config.provider.kind == "mock" {
        let tracker = factoryrs_issue_mock::MockTracker::seeded_demo();
        let agent = factoryrs_issue_mock::MockAgentRuntime::new(tracker.clone());
        return Ok((Arc::new(tracker), Arc::new(agent)));
    }

    let tracker: Arc<dyn IssueTracker> = match workflow.config.tracker.kind.as_str() {
        #[cfg(feature = "mock")]
        "mock" => Arc::new(factoryrs_issue_mock::MockTracker::seeded_demo()),
        #[cfg(feature = "linear")]
        "linear" => {
            let api_key = workflow
                .config
                .tracker
                .api_key
                .clone()
                .ok_or_else(|| Error::Config("tracker.api_key is required".into()))?;
            Arc::new(factoryrs_linear::LinearTracker::new(
                workflow.config.tracker.endpoint.clone(),
                api_key,
            ))
        }
        #[cfg(feature = "github")]
        "github" => Arc::new(factoryrs_github::GithubIssueTracker::new(
            workflow
                .config
                .tracker
                .repository
                .clone()
                .ok_or_else(|| Error::Config("tracker.repository is required".into()))?,
            workflow.config.tracker.api_key.clone(),
        )?),
        other => {
            return Err(Error::Config(format!(
                "unsupported tracker.kind `{other}` for this build"
            )));
        }
    };

    match workflow.config.provider.kind.as_str() {
        #[cfg(feature = "mock")]
        "mock" => Err(Error::Config(
            "provider.kind `mock` only supports tracker.kind `mock`".into(),
        )),
        "codex" | "copilot" | "claude" | "generic" => Err(Error::Config(format!(
            "provider.kind `{}` is declared but no real app-server runtime is wired yet",
            workflow.config.provider.kind
        ))),
        other => Err(Error::Config(format!(
            "unsupported provider.kind `{other}` for this build"
        ))),
    }
}

async fn build_store(sqlite_url: Option<&str>) -> Result<Option<Arc<dyn StateStore>>, Error> {
    #[cfg(feature = "sqlite")]
    if let Some(url) = sqlite_url {
        let store = factoryrs_sqlite::SqliteStateStore::connect(url)
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

    Ok(None)
}
