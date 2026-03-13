use std::{env, path::PathBuf, sync::Arc};

use {
    clap::Parser,
    opentelemetry::{KeyValue, global, trace::TracerProvider as _},
    opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::SdkTracerProvider},
    polyphony_core::{
        IssueTracker, PullRequestCommenter, PullRequestManager, StateStore, WorkspaceCommitter,
        WorkspaceProvisioner,
    },
    polyphony_orchestrator::{
        RuntimeCommand, RuntimeComponentFactory, RuntimeComponents, RuntimeService,
        spawn_workflow_watcher,
    },
    polyphony_workflow::load_workflow,
    thiserror::Error,
    tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt},
};

#[derive(Debug, Parser)]
#[command(name = "polyphony")]
struct Cli {
    #[arg(value_name = "WORKFLOW", default_value = "WORKFLOW.md")]
    workflow_path: PathBuf,
    #[arg(long)]
    no_tui: bool,
    #[arg(long)]
    log_json: bool,
    #[arg(long, env = "POLYPHONY_SQLITE_URL")]
    sqlite_url: Option<String>,
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
    Tui(#[from] polyphony_tui::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Config(String),
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = Cli::parse();
    let _telemetry = init_tracing(cli.log_json)?;
    tracing::info!(
        workflow_path = %cli.workflow_path.display(),
        no_tui = cli.no_tui,
        sqlite_enabled = cli.sqlite_url.is_some(),
        "starting polyphony"
    );
    let workflow = load_workflow(&cli.workflow_path)?;
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(|workflow| {
        build_runtime_components(workflow)
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))
    });
    let components = build_runtime_components(&workflow)?;
    let provisioner: Arc<dyn WorkspaceProvisioner> =
        Arc::new(polyphony_git::GitWorkspaceProvisioner);
    let store = build_store(cli.sqlite_url.as_deref()).await?;
    let (workflow_tx, workflow_rx) = tokio::sync::watch::channel(workflow.clone());
    let (service, handle) = RuntimeService::new(
        components.tracker,
        components.agent,
        provisioner,
        components.committer,
        components.pull_request_manager,
        components.pull_request_commenter,
        components.feedback,
        store,
        workflow_rx,
    );
    let service = service.with_workflow_reload(
        cli.workflow_path.clone(),
        workflow_tx.clone(),
        component_factory,
    );
    let _watcher = spawn_workflow_watcher(cli.workflow_path.clone(), handle.command_tx.clone())?;
    let service_task = tokio::spawn(service.run());

    if cli.no_tui {
        tokio::signal::ctrl_c().await?;
        let _ = handle.command_tx.send(RuntimeCommand::Shutdown);
    } else {
        polyphony_tui::run(handle.snapshot_rx.clone(), handle.command_tx.clone()).await?;
        let _ = handle.command_tx.send(RuntimeCommand::Shutdown);
    }

    service_task
        .await
        .map_err(|error| Error::Config(error.to_string()))??;
    Ok(())
}

struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(tracer_provider) = self.tracer_provider.take() {
            let _ = tracer_provider.shutdown();
        }
    }
}

fn init_tracing(log_json: bool) -> Result<TelemetryGuard, Error> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let tracer_provider = build_tracer_provider()?;
    if log_json {
        if let Some(provider) = tracer_provider.clone() {
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().json())
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony")))
                .try_init()
                .map_err(|error| Error::Config(error.to_string()))?;
        } else {
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().json())
                .try_init()
                .map_err(|error| Error::Config(error.to_string()))?;
        }
    } else if let Some(provider) = tracer_provider.clone() {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().compact())
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony")))
            .try_init()
            .map_err(|error| Error::Config(error.to_string()))?;
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().compact())
            .try_init()
            .map_err(|error| Error::Config(error.to_string()))?;
    }
    tracing::info!(
        otel_enabled = tracer_provider.is_some(),
        log_json,
        "tracing initialized"
    );
    Ok(TelemetryGuard { tracer_provider })
}

fn build_tracer_provider() -> Result<Option<SdkTracerProvider>, Error> {
    if !otel_configured() {
        return Ok(None);
    }

    global::set_text_map_propagator(TraceContextPropagator::new());
    let service_name = env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "polyphony".into());
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .map_err(|error| Error::Config(format!("building OTLP exporter failed: {error}")))?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_service_name(service_name.clone())
                .with_attributes([KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))])
                .build(),
        )
        .build();
    let tracer = tracer_provider.tracer("polyphony");
    drop(tracer);
    global::set_tracer_provider(tracer_provider.clone());
    Ok(Some(tracer_provider))
}

fn otel_configured() -> bool {
    env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
        || env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some()
}

#[allow(unused_variables)]
fn build_runtime_components(
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<RuntimeComponents, Error> {
    #[cfg(feature = "mock")]
    if is_mock_workflow(workflow) {
        let tracker = polyphony_issue_mock::MockTracker::seeded_demo();
        let agent = polyphony_issue_mock::MockAgentRuntime::new(tracker.clone());
        return Ok(RuntimeComponents {
            tracker: Arc::new(tracker),
            agent: Arc::new(agent),
            committer: None,
            pull_request_manager: None,
            pull_request_commenter: None,
            feedback: None,
        });
    }

    let tracker: Arc<dyn IssueTracker> = match workflow.config.tracker.kind.as_str() {
        #[cfg(feature = "mock")]
        "mock" => Arc::new(polyphony_issue_mock::MockTracker::seeded_demo()),
        #[cfg(feature = "linear")]
        "linear" => {
            let api_key = workflow
                .config
                .tracker
                .api_key
                .clone()
                .ok_or_else(|| Error::Config("tracker.api_key is required".into()))?;
            Arc::new(polyphony_linear::LinearTracker::new(
                workflow.config.tracker.endpoint.clone(),
                api_key,
            ))
        },
        #[cfg(feature = "github")]
        "github" => Arc::new(polyphony_github::GithubIssueTracker::new(
            workflow
                .config
                .tracker
                .repository
                .clone()
                .ok_or_else(|| Error::Config("tracker.repository is required".into()))?,
            workflow.config.tracker.api_key.clone(),
            workflow.config.tracker.project_owner.clone(),
            workflow.config.tracker.project_number,
            workflow.config.tracker.project_status_field.clone(),
        )?),
        other => {
            return Err(Error::Config(format!(
                "unsupported tracker.kind `{other}` for this build"
            )));
        },
    };

    let feedback = {
        let registry = polyphony_feedback::FeedbackRegistry::from_config(&workflow.config.feedback);
        (!registry.is_empty()).then_some(Arc::new(registry))
    };
    let committer: Option<Arc<dyn WorkspaceCommitter>> =
        workflow.config.automation.enabled.then_some(
            Arc::new(polyphony_git::GitWorkspaceCommitter) as Arc<dyn WorkspaceCommitter>,
        );
    #[cfg(feature = "github")]
    let (pull_request_manager, pull_request_commenter) =
        if workflow.config.automation.enabled && workflow.config.tracker.kind == "github" {
            let repository = workflow
                .config
                .tracker
                .repository
                .clone()
                .ok_or_else(|| Error::Config("tracker.repository is required".into()))?;
            let token = workflow
                .config
                .tracker
                .api_key
                .clone()
                .ok_or_else(|| Error::Config("tracker.api_key is required".into()))?;
            (
                Some(Arc::new(polyphony_github::GithubPullRequestManager::new(
                    repository.clone(),
                    token.clone(),
                )?) as Arc<dyn PullRequestManager>),
                Some(
                    Arc::new(polyphony_github::GithubPullRequestCommenter::new(token))
                        as Arc<dyn PullRequestCommenter>,
                ),
            )
        } else {
            (None, None)
        };
    #[cfg(not(feature = "github"))]
    let (pull_request_manager, pull_request_commenter): (
        Option<Arc<dyn PullRequestManager>>,
        Option<Arc<dyn PullRequestCommenter>>,
    ) = (None, None);

    Ok(RuntimeComponents {
        tracker,
        agent: Arc::new(polyphony_agents::AgentRegistryRuntime::new()),
        committer,
        pull_request_manager,
        pull_request_commenter,
        feedback,
    })
}

async fn build_store(sqlite_url: Option<&str>) -> Result<Option<Arc<dyn StateStore>>, Error> {
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

    Ok(None)
}

#[cfg(feature = "mock")]
fn is_mock_workflow(workflow: &polyphony_workflow::LoadedWorkflow) -> bool {
    workflow.config.tracker.kind == "mock"
        && workflow
            .config
            .agents
            .default
            .as_ref()
            .and_then(|name| workflow.config.agents.profiles.get(name))
            .map(|profile| {
                profile.kind == "mock"
                    || matches!(profile.transport.as_deref(), Some("mock"))
                    || profile.transport.is_none() && profile.kind.is_empty()
            })
            .unwrap_or(false)
}
