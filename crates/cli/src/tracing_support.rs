use crate::{prelude::*, ui_support::LogBuffer, *};

#[cfg(feature = "tracing")]
pub(crate) struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

#[cfg(not(feature = "tracing"))]
pub(crate) struct TelemetryGuard;

#[derive(Clone)]
#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
pub(crate) struct TracingOutput {
    mode: Arc<Mutex<TracingOutputMode>>,
    file_sink: Option<FileLogSink>,
}

#[derive(Clone)]
enum TracingOutputMode {
    Stderr,
    Tui(LogBuffer),
}

#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
impl TracingOutput {
    pub(crate) fn stderr(file_sink: Option<FileLogSink>) -> Self {
        Self {
            mode: Arc::new(Mutex::new(TracingOutputMode::Stderr)),
            file_sink,
        }
    }

    pub(crate) fn tui(log_buffer: LogBuffer, file_sink: Option<FileLogSink>) -> Self {
        Self {
            mode: Arc::new(Mutex::new(TracingOutputMode::Tui(log_buffer))),
            file_sink,
        }
    }

    pub(crate) fn log_path(&self) -> Option<PathBuf> {
        self.file_sink.as_ref().map(FileLogSink::path)
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
        if let Some(file_sink) = &self.file_sink {
            file_sink.record_line(line);
        }
        match lock_or_recover(&self.mode).clone() {
            TracingOutputMode::Stderr => {
                let _ = writeln!(io::stderr().lock(), "{line}");
            },
            TracingOutputMode::Tui(log_buffer) => log_buffer.push_line(line.to_string()),
        }
    }
}

#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
pub(crate) struct TracingOutputWriter {
    pub(crate) output: TracingOutput,
    pub(crate) buffer: Vec<u8>,
}

#[derive(Clone)]
#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
pub(crate) struct FileLogSink {
    path: PathBuf,
    state: Arc<Mutex<FileLogSinkState>>,
}

#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
struct FileLogSinkState {
    file: File,
    write_failed: bool,
}

#[cfg_attr(not(feature = "tracing"), allow(dead_code))]
impl FileLogSink {
    fn new(path: PathBuf) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(Error::Io)?;
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(FileLogSinkState {
                file,
                write_failed: false,
            })),
        })
    }

    fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn record_line(&self, line: &str) {
        let mut state = lock_or_recover(&self.state);
        if state.write_failed {
            return;
        }
        if let Err(error) = writeln!(state.file, "{line}") {
            state.write_failed = true;
            let _ = writeln!(
                io::stderr().lock(),
                "polyphony: failed to append to log file {}: {error}",
                self.path.display()
            );
        }
    }
}

impl io::Write for TracingOutputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            self.output.record_bytes(&self.buffer);
            self.buffer.clear();
        }
        Ok(())
    }
}

impl Drop for TracingOutputWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(feature = "tracing")]
impl<'a> MakeWriter<'a> for TracingOutput {
    type Writer = TracingOutputWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TracingOutputWriter {
            output: self.clone(),
            buffer: Vec::new(),
        }
    }
}

#[cfg(feature = "tracing")]
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(tracer_provider) = self.tracer_provider.take() {
            let _ = tracer_provider.shutdown();
        }
    }
}

#[cfg(feature = "tracing")]
pub(crate) fn init_tracing(
    log_json: bool,
    tui_mode: bool,
    tracing_output: TracingOutput,
) -> TelemetryGuard {
    let default_filter = if tui_mode {
        // Show network activity in the TUI logs tab
        "info,polyphony_github=debug,polyphony_linear=debug,polyphony_orchestrator=debug"
    } else {
        "info"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
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
        tracing_output.clone(),
    ) {
        eprintln!("polyphony: tracing subscriber setup failed: {error}");
    }
    if let Some(log_path) = tracing_output.log_path() {
        tracing::info!(
            log_path = %log_path.display(),
            otel_enabled = tracer_provider.is_some(),
            log_json,
            "tracing initialized"
        );
    } else {
        tracing::info!(
            otel_enabled = tracer_provider.is_some(),
            log_json,
            "tracing initialized"
        );
    }
    TelemetryGuard { tracer_provider }
}

#[cfg(not(feature = "tracing"))]
pub(crate) fn init_tracing(
    _log_json: bool,
    _tui_mode: bool,
    _tracing_output: TracingOutput,
) -> TelemetryGuard {
    TelemetryGuard
}

pub(crate) fn init_run_log_sink(workflow_path: &Path) -> Result<FileLogSink, Error> {
    let log_dir = workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join("logs");
    fs::create_dir_all(&log_dir).map_err(Error::Io)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let file_name = format!(
        "polyphony-{}-{:09}-pid{}.log",
        now.as_secs(),
        now.subsec_nanos(),
        std::process::id()
    );
    FileLogSink::new(log_dir.join(file_name))
}

pub(crate) fn load_historical_log_lines(workflow_path: &Path) -> Result<Vec<String>, Error> {
    let log_dir = workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join("logs");
    if !log_dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = fs::read_dir(&log_dir)
        .map_err(Error::Io)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
        .collect::<Vec<_>>();
    entries.sort();

    let mut lines = Vec::new();
    for path in entries {
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                warn!(path = %path.display(), %error, "failed to load historical log file");
                continue;
            },
        };
        lines.extend(
            content
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_owned),
        );
    }
    Ok(lines)
}

#[cfg(feature = "tracing")]
fn install_tracing_subscriber(
    filter: EnvFilter,
    tracer_provider: Option<SdkTracerProvider>,
    log_json: bool,
    tui_mode: bool,
    tracing_output: TracingOutput,
) -> Result<(), Error> {
    if log_json || tui_mode {
        let otel_layer = tracer_provider.map(|provider| {
            tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony"))
        });
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(tracing_output)
                    .with_ansi(false),
            )
            .with(otel_layer)
            .try_init()
            .map_err(|error| Error::Config(error.to_string()))?;
    } else {
        let otel_layer = tracer_provider.map(|provider| {
            tracing_opentelemetry::layer().with_tracer(provider.tracer("polyphony"))
        });
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_writer(tracing_output)
                    .with_ansi(false),
            )
            .with(otel_layer)
            .try_init()
            .map_err(|error| Error::Config(error.to_string()))?;
    }
    Ok(())
}

#[cfg(feature = "tracing")]
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
    global::set_tracer_provider(tracer_provider.clone());
    Ok(Some(tracer_provider))
}

#[cfg(feature = "tracing")]
fn otel_configured() -> bool {
    env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
        || env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some()
}

pub(crate) async fn run_operator_surface<F>(
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
    ) -> crate::ui_support::TuiRunFuture,
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
