use std::{
    env,
    future::Future,
    io::{self, Write as _},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

use {
    clap::Parser,
    opentelemetry::{KeyValue, global, trace::TracerProvider as _},
    opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::SdkTracerProvider},
    polyphony_core::{
        IssueTracker, PullRequestCommenter, PullRequestManager, RuntimeSnapshot, StateStore,
        WorkspaceCommitter, WorkspaceProvisioner,
    },
    polyphony_orchestrator::{
        RuntimeCommand, RuntimeComponentFactory, RuntimeComponents, RuntimeService,
        spawn_workflow_watcher,
    },
    polyphony_tui::LogBuffer,
    polyphony_workflow::load_workflow,
    thiserror::Error,
    tokio::sync::{mpsc, watch},
    tracing::warn,
    tracing_subscriber::{
        EnvFilter, fmt::writer::MakeWriter, layer::SubscriberExt, util::SubscriberInitExt,
    },
};

type TuiRunFuture = Pin<Box<dyn Future<Output = Result<(), polyphony_tui::Error>> + Send>>;
type ShutdownFuture = Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send>>;

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
    let tui_logs = LogBuffer::default();
    let tracing_output = if cli.no_tui {
        TracingOutput::stderr()
    } else {
        TracingOutput::tui(tui_logs.clone())
    };
    let _telemetry = init_tracing(cli.log_json, !cli.no_tui, tracing_output.clone());
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

    run_operator_surface(
        cli.no_tui,
        handle.snapshot_rx.clone(),
        handle.command_tx.clone(),
        tui_logs,
        tracing_output,
        |snapshot_rx, command_tx, tui_logs| {
            Box::pin(polyphony_tui::run(snapshot_rx, command_tx, tui_logs))
        },
        Box::pin(tokio::signal::ctrl_c()),
    )
    .await?;

    service_task
        .await
        .map_err(|error| Error::Config(error.to_string()))??;
    Ok(())
}

struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

#[derive(Clone)]
struct TracingOutput {
    mode: Arc<Mutex<TracingOutputMode>>,
}

#[derive(Clone)]
enum TracingOutputMode {
    Stderr,
    Tui(LogBuffer),
}

impl TracingOutput {
    fn stderr() -> Self {
        Self {
            mode: Arc::new(Mutex::new(TracingOutputMode::Stderr)),
        }
    }

    fn tui(log_buffer: LogBuffer) -> Self {
        Self {
            mode: Arc::new(Mutex::new(TracingOutputMode::Tui(log_buffer))),
        }
    }

    fn switch_to_stderr(&self) {
        let buffered = {
            let mut mode = lock_or_recover(&self.mode);
            let TracingOutputMode::Tui(log_buffer) = &*mode else {
                return;
            };
            let log_buffer = log_buffer.clone();
            *mode = TracingOutputMode::Stderr;
            log_buffer.drain_oldest_first()
        };

        for line in buffered {
            let _ = writeln!(io::stderr().lock(), "{line}");
        }
    }

    fn record_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        for line in String::from_utf8_lossy(bytes).lines() {
            self.record_line(line);
        }
    }

    fn record_line(&self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        match lock_or_recover(&self.mode).clone() {
            TracingOutputMode::Stderr => {
                let _ = writeln!(io::stderr().lock(), "{line}");
            },
            TracingOutputMode::Tui(log_buffer) => log_buffer.push_line(line.to_string()),
        }
    }
}

struct TracingOutputWriter {
    output: TracingOutput,
    buffer: Vec<u8>,
}

impl io::Write for TracingOutputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TracingOutputWriter {
    fn drop(&mut self) {
        self.output.record_bytes(&self.buffer);
    }
}

impl<'a> MakeWriter<'a> for TracingOutput {
    type Writer = TracingOutputWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TracingOutputWriter {
            output: self.clone(),
            buffer: Vec::new(),
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(tracer_provider) = self.tracer_provider.take() {
            let _ = tracer_provider.shutdown();
        }
    }
}

fn init_tracing(log_json: bool, tui_mode: bool, tracing_output: TracingOutput) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let tracer_provider = match build_tracer_provider() {
        Ok(provider) => provider,
        Err(error) => {
            eprintln!("polyphony: tracing exporter setup failed: {error}");
            None
        },
    };
    if let Err(error) = install_tracing_subscriber(
        filter,
        tracer_provider.clone(),
        log_json,
        tui_mode,
        tracing_output,
    ) {
        eprintln!("polyphony: tracing subscriber setup failed: {error}");
    }
    tracing::info!(
        otel_enabled = tracer_provider.is_some(),
        log_json,
        "tracing initialized"
    );
    TelemetryGuard { tracer_provider }
}

fn install_tracing_subscriber(
    filter: EnvFilter,
    tracer_provider: Option<SdkTracerProvider>,
    log_json: bool,
    tui_mode: bool,
    tracing_output: TracingOutput,
) -> Result<(), Error> {
    if log_json {
        if tui_mode {
            if let Some(provider) = tracer_provider {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .with_writer(tracing_output)
                            .with_ansi(false),
                    )
                    .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony")))
                    .try_init()
                    .map_err(|error| Error::Config(error.to_string()))?;
            } else {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .with_writer(tracing_output)
                            .with_ansi(false),
                    )
                    .try_init()
                    .map_err(|error| Error::Config(error.to_string()))?;
            }
        } else if let Some(provider) = tracer_provider {
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
    } else if let Some(provider) = tracer_provider {
        if tui_mode {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .compact()
                        .with_writer(tracing_output)
                        .with_ansi(false),
                )
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony")))
                .try_init()
                .map_err(|error| Error::Config(error.to_string()))?;
        } else {
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().compact())
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony")))
                .try_init()
                .map_err(|error| Error::Config(error.to_string()))?;
        }
    } else if tui_mode {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_writer(tracing_output)
                    .with_ansi(false),
            )
            .try_init()
            .map_err(|error| Error::Config(error.to_string()))?;
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().compact())
            .try_init()
            .map_err(|error| Error::Config(error.to_string()))?;
    }
    Ok(())
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

async fn run_operator_surface<F>(
    no_tui: bool,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    tui_logs: LogBuffer,
    tracing_output: TracingOutput,
    run_tui: F,
    shutdown_signal: ShutdownFuture,
) -> Result<(), Error>
where
    F: FnOnce(
        watch::Receiver<RuntimeSnapshot>,
        mpsc::UnboundedSender<RuntimeCommand>,
        LogBuffer,
    ) -> TuiRunFuture,
{
    if no_tui {
        shutdown_signal.await?;
        let _ = command_tx.send(RuntimeCommand::Shutdown);
        return Ok(());
    }

    match run_tui(snapshot_rx, command_tx.clone(), tui_logs).await {
        Ok(()) => {
            let _ = command_tx.send(RuntimeCommand::Shutdown);
            Ok(())
        },
        Err(error) => {
            tracing_output.switch_to_stderr();
            warn!(%error, "tui failed; continuing headless");
            eprintln!(
                "polyphony: TUI failed: {error}. Continuing headless mode. Press Ctrl-C to stop."
            );
            shutdown_signal.await?;
            let _ = command_tx.send(RuntimeCommand::Shutdown);
            Ok(())
        },
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
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
            )?)
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

#[cfg(all(test, feature = "mock"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{
        fs, io,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use {
        polyphony_orchestrator::{RuntimeCommand, RuntimeService},
        polyphony_workflow::load_workflow,
        tokio::sync::{mpsc, watch},
    };

    use crate::{Error, LogBuffer, TracingOutput, run_operator_surface};

    fn snapshot_rx() -> watch::Receiver<polyphony_core::RuntimeSnapshot> {
        let workflow_path = std::env::temp_dir().join(format!(
            "polyphony-cli-test-{}.md",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
            Arc::new(agent),
            Arc::new(polyphony_git::GitWorkspaceProvisioner),
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
    async fn operator_surface_falls_back_to_headless_when_tui_fails() {
        let snapshot_rx = snapshot_rx();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        run_operator_surface(
            false,
            snapshot_rx,
            command_tx,
            LogBuffer::default(),
            TracingOutput::stderr(),
            |_snapshot_rx, _command_tx, _tui_logs| {
                Box::pin(async { Err(polyphony_tui::Error::Io(io::Error::other("boom"))) })
            },
            Box::pin(async { Ok(()) }),
        )
        .await
        .unwrap();

        assert!(matches!(
            command_rx.recv().await,
            Some(RuntimeCommand::Shutdown)
        ));
    }

    #[tokio::test]
    async fn operator_surface_waits_for_shutdown_in_headless_mode() {
        let snapshot_rx = snapshot_rx();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        run_operator_surface(
            true,
            snapshot_rx,
            command_tx,
            LogBuffer::default(),
            TracingOutput::stderr(),
            |_snapshot_rx, _command_tx, _tui_logs| Box::pin(async { Ok(()) }),
            Box::pin(async { Ok(()) }),
        )
        .await
        .unwrap();

        assert!(matches!(
            command_rx.recv().await,
            Some(RuntimeCommand::Shutdown)
        ));
    }

    #[tokio::test]
    async fn operator_surface_propagates_shutdown_wait_errors() {
        let snapshot_rx = snapshot_rx();
        let (command_tx, _command_rx) = mpsc::unbounded_channel();

        let error = run_operator_surface(
            true,
            snapshot_rx,
            command_tx,
            LogBuffer::default(),
            TracingOutput::stderr(),
            |_snapshot_rx, _command_tx, _tui_logs| Box::pin(async { Ok(()) }),
            Box::pin(async { Err(io::Error::other("ctrl-c failed")) }),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Io(_)));
    }
}
