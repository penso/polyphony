use std::{
    env,
    future::Future,
    io::{self, Write as _},
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{Arc, Mutex, MutexGuard},
};

use {
    async_trait::async_trait,
    clap::{Parser, Subcommand},
    opentelemetry::{KeyValue, global, trace::TracerProvider as _},
    opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::SdkTracerProvider},
    polyphony_core::{
        CheckoutKind, IssueTracker, NetworkCache, PullRequestCommenter, PullRequestManager,
        RuntimeSnapshot, StateStore, TrackerKind, WorkspaceCommitter, WorkspaceProvisioner,
    },
    polyphony_orchestrator::{
        RuntimeCommand, RuntimeComponentFactory, RuntimeComponents, RuntimeService,
        spawn_workflow_watcher,
    },
    polyphony_tui::{LogBuffer, prompt_workflow_initialization},
    polyphony_workflow::{
        ServiceConfig, ensure_repo_config_file, ensure_user_config_file, ensure_workflow_file,
        load_workflow_with_user_config, repo_config_path, seed_repo_config_with_github,
        user_config_path,
    },
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

#[derive(Debug, Subcommand)]
enum Commands {
    /// Manage tracker issues
    Issue {
        #[command(subcommand)]
        action: IssueAction,
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

#[derive(Debug, Subcommand)]
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

fn build_tracker(
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<Arc<dyn IssueTracker>, Error> {
    #[allow(unused_variables)]
    let tracker: Arc<dyn IssueTracker> = match workflow.config.tracker.kind {
        TrackerKind::None => Arc::new(EmptyTracker),
        #[cfg(feature = "linear")]
        TrackerKind::Linear => {
            let api_key = workflow.config.tracker.api_key.clone().ok_or_else(|| {
                Error::Config("tracker.api_key is required for linear".into())
            })?;
            Arc::new(polyphony_linear::LinearTracker::new(
                workflow.config.tracker.endpoint.clone(),
                api_key,
                workflow.config.tracker.team_id.clone(),
            )?)
        },
        #[cfg(feature = "github")]
        TrackerKind::Github => Arc::new(polyphony_github::GithubIssueTracker::new(
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
        #[cfg(feature = "beads")]
        TrackerKind::Beads => {
            let workflow_root = workflow_root_dir(&workflow.path)?;
            Arc::new(polyphony_beads::BeadsTracker::new(workflow_root)?)
        },
        other => {
            return Err(Error::Config(format!(
                "unsupported tracker.kind `{other}` for this build"
            )));
        },
    };
    Ok(tracker)
}

async fn handle_issue_command(
    action: IssueAction,
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<(), Error> {
    let tracker = build_tracker(workflow)?;
    match action {
        IssueAction::Create {
            title,
            description,
            priority,
            labels,
            parent,
        } => {
            let request = polyphony_core::CreateIssueRequest {
                title,
                description,
                priority,
                labels,
                parent_id: parent,
            };
            let issue = tracker.create_issue(&request).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&issue)
                    .map_err(|e| Error::Config(e.to_string()))?
            );
        },
        IssueAction::Update {
            identifier,
            title,
            description,
            state,
            priority,
            labels,
        } => {
            let request = polyphony_core::UpdateIssueRequest {
                id: identifier,
                title,
                description,
                state,
                priority,
                labels,
            };
            let issue = tracker.update_issue(&request).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&issue)
                    .map_err(|e| Error::Config(e.to_string()))?
            );
        },
        IssueAction::List { state, all } => {
            let states = if all {
                let mut s = workflow.config.tracker.active_states.clone();
                s.extend(workflow.config.tracker.terminal_states.clone());
                s
            } else {
                state.unwrap_or_else(|| workflow.config.tracker.active_states.clone())
            };
            let issues = tracker
                .fetch_issues_by_states(
                    workflow.config.tracker.project_slug.as_deref(),
                    &states,
                )
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&issues)
                    .map_err(|e| Error::Config(e.to_string()))?
            );
        },
        IssueAction::Show { identifier } => {
            let issues = tracker
                .fetch_issues_by_ids(std::slice::from_ref(&identifier))
                .await?;
            if let Some(issue) = issues.first() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(issue)
                        .map_err(|e| Error::Config(e.to_string()))?
                );
            } else {
                return Err(Error::Config(format!("issue not found: {identifier}")));
            }
        },
    }
    Ok(())
}

fn handle_config_command(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_path: &Path,
    json: bool,
) -> Result<(), Error> {
    if json {
        print_config_json(&workflow.config)?;
    } else {
        print_config_summary(workflow, workflow_path)?;
    }
    Ok(())
}

fn print_config_json(config: &ServiceConfig) -> Result<(), Error> {
    let mut value =
        serde_json::to_value(config).map_err(|e| Error::Config(e.to_string()))?;
    redact_api_keys(&mut value);
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| Error::Config(e.to_string()))?
    );
    Ok(())
}

fn redact_api_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if key.contains("api_key") || key.contains("bot_token") || key == "bearer_token" {
                    if let serde_json::Value::String(s) = val {
                        // Keep env var references and empty strings visible
                        if !s.is_empty() && !s.starts_with('$') {
                            *val = serde_json::Value::String("<redacted>".into());
                        }
                    }
                } else {
                    redact_api_keys(val);
                }
            }
        },
        serde_json::Value::Array(arr) => {
            for item in arr {
                redact_api_keys(item);
            }
        },
        _ => {},
    }
}

fn print_config_summary(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_path: &Path,
) -> Result<(), Error> {
    let config = &workflow.config;
    let user_config = user_config_path()?;
    let repo_config = repo_config_path(workflow_path)?;

    // Config sources
    println!("Config sources:");
    print_source_line(&user_config);
    print_source_line(workflow_path);
    print_source_line(&repo_config);
    println!();

    // Tracker
    println!("Tracker:");
    println!("  kind: {}", config.tracker.kind);
    if !config.tracker.active_states.is_empty() {
        println!("  active states: {}", config.tracker.active_states.join(", "));
    }
    if !config.tracker.terminal_states.is_empty() {
        println!(
            "  terminal states: {}",
            config.tracker.terminal_states.join(", ")
        );
    }
    println!();

    // Workspace
    println!("Workspace:");
    println!("  checkout: {:?}", config.workspace.checkout_kind);
    println!("  root: {}", config.workspace.root.display());
    if let Some(src) = &config.workspace.source_repo_path {
        println!("  source: {}", src.display());
    }
    println!();

    // Agents
    println!("Agents:");
    if let Some(default) = &config.agents.default {
        println!("  default: {default}");
    }
    if !config.agents.profiles.is_empty() {
        println!("  profiles:");
        for (name, profile) in &config.agents.profiles {
            let transport = profile
                .transport
                .as_deref()
                .unwrap_or(&profile.kind);
            let mut extra = Vec::new();
            if let Some(model) = &profile.model {
                extra.push(format!("model: {model}"));
            }
            if !profile.fallbacks.is_empty() {
                extra.push(format!("fallbacks: [{}]", profile.fallbacks.join(", ")));
            }
            let extra_str = if extra.is_empty() {
                String::new()
            } else {
                format!("  {}", extra.join("  "))
            };
            println!(
                "    {name:<10} {:<8} {transport:<12}{extra_str}",
                profile.kind,
            );
        }
    }
    if !config.agents.by_label.is_empty() {
        println!("  routing:");
        let pairs: Vec<String> = config
            .agents
            .by_label
            .iter()
            .map(|(label, agent)| format!("{label}\u{2192}{agent}"))
            .collect();
        println!("    by_label: {}", pairs.join("  "));
    }
    if !config.agents.by_state.is_empty() {
        if config.agents.by_label.is_empty() {
            println!("  routing:");
        }
        let pairs: Vec<String> = config
            .agents
            .by_state
            .iter()
            .map(|(state, agent)| format!("{state}\u{2192}{agent}"))
            .collect();
        println!("    by_state: {}", pairs.join("  "));
    }
    println!();

    // Orchestrator
    println!("Orchestrator:");
    println!("  max concurrent: {}", config.agent.max_concurrent_agents);
    println!("  max turns: {}", config.agent.max_turns);
    println!("  poll interval: {}s", config.polling.interval_ms / 1000);
    println!();

    // Validation
    match config.validate() {
        Ok(()) => println!("Validation: \u{2713} passed"),
        Err(e) => println!("Validation: \u{2717} {e}"),
    }

    Ok(())
}

fn print_source_line(path: &Path) {
    if path.exists() {
        println!("  \u{2713} {}", path.display());
    } else {
        println!("  - {} (not found)", path.display());
    }
}

fn handle_doctor_command(
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<(), Error> {
    let config = &workflow.config;
    let mut failures = 0u32;

    // Validate config first
    print!("Config validation ... ");
    match config.validate() {
        Ok(()) => println!("\u{2713} passed"),
        Err(e) => {
            println!("\u{2717} {e}");
            failures += 1;
        },
    }

    // Check each agent profile
    for (name, profile) in &config.agents.profiles {
        println!();
        println!("Agent: {name} (kind: {})", profile.kind);

        // Check the main command binary exists on PATH
        if let Some(cmd_str) = &profile.command {
            let binary = cmd_str.split_whitespace().next().unwrap_or(cmd_str);
            print!("  command `{cmd_str}` ... ");
            match which_binary(binary) {
                Some(path) => println!("\u{2713} {}", path.display()),
                None => {
                    println!("\u{2717} `{binary}` not found in PATH");
                    failures += 1;
                },
            }
        }

        // Run models_command and validate output
        if let Some(models_cmd) = &profile.models_command {
            print!("  models_command `{models_cmd}` ... ");
            match run_shell_command(models_cmd) {
                Ok(output) if !output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("\u{2717} exit code {}", output.status);
                    for line in stderr.lines().take(5) {
                        println!("    {line}");
                    }
                    failures += 1;
                },
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
                        Ok(value) => {
                            let count = match &value {
                                serde_json::Value::Array(arr) => arr.len(),
                                _ => 1,
                            };
                            println!("\u{2713} ok ({count} entries)");
                        },
                        Err(e) => {
                            println!("\u{2717} output is not valid JSON: {e}");
                            for line in stdout.lines().take(3) {
                                println!("    {line}");
                            }
                            failures += 1;
                        },
                    }
                },
                Err(e) => {
                    println!("\u{2717} failed to run: {e}");
                    failures += 1;
                },
            }
        }

        // Run credits_command if present
        if let Some(credits_cmd) = &profile.credits_command {
            print!("  credits_command `{credits_cmd}` ... ");
            match run_shell_command(credits_cmd) {
                Ok(output) if output.status.success() => println!("\u{2713} ok"),
                Ok(output) => {
                    println!("\u{2717} exit code {}", output.status);
                    failures += 1;
                },
                Err(e) => {
                    println!("\u{2717} failed to run: {e}");
                    failures += 1;
                },
            }
        }

        // Run spending_command if present
        if let Some(spending_cmd) = &profile.spending_command {
            print!("  spending_command `{spending_cmd}` ... ");
            match run_shell_command(spending_cmd) {
                Ok(output) if output.status.success() => println!("\u{2713} ok"),
                Ok(output) => {
                    println!("\u{2717} exit code {}", output.status);
                    failures += 1;
                },
                Err(e) => {
                    println!("\u{2717} failed to run: {e}");
                    failures += 1;
                },
            }
        }

        // Check fallbacks reference valid profiles
        for fallback in &profile.fallbacks {
            if !config.agents.profiles.contains_key(fallback) {
                println!(
                    "  fallback `{fallback}` ... \u{2717} profile not defined"
                );
                failures += 1;
            }
        }
    }

    // Check routing references
    for (label, agent) in &config.agents.by_label {
        if !config.agents.profiles.contains_key(agent) {
            println!(
                "\nRouting by_label `{label}` \u{2192} `{agent}` ... \u{2717} profile not defined"
            );
            failures += 1;
        }
    }
    for (state, agent) in &config.agents.by_state {
        if !config.agents.profiles.contains_key(agent) {
            println!(
                "\nRouting by_state `{state}` \u{2192} `{agent}` ... \u{2717} profile not defined"
            );
            failures += 1;
        }
    }

    println!();
    if failures == 0 {
        println!("All checks passed.");
    } else {
        println!("{failures} check(s) failed.");
        std::process::exit(1);
    }
    Ok(())
}

fn which_binary(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(name);
            if full.is_file() { Some(full) } else { None }
        })
    })
}

fn run_shell_command(cmd: &str) -> Result<std::process::Output, std::io::Error> {
    Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .output()
}

async fn try_main() -> Result<(), Error> {
    let cli = Cli::parse();
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
        let workflow =
            load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return handle_config_command(&workflow, &workflow_path, *json);
    }
    if let Some(Commands::Doctor) = &cli.command {
        let user_config_path = user_config_path()?;
        let workflow =
            load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return handle_doctor_command(&workflow);
    }
    if let Some(Commands::Issue { action }) = cli.command {
        let user_config_path = user_config_path()?;
        let workflow =
            load_workflow_with_user_config(&workflow_path, Some(&user_config_path))?;
        return handle_issue_command(action, &workflow).await;
    }

    let tui_logs = LogBuffer::default();
    let tracing_output = if cli.no_tui {
        TracingOutput::stderr()
    } else {
        TracingOutput::tui(tui_logs.clone())
    };
    let _telemetry = init_tracing(cli.log_json, !cli.no_tui, tracing_output.clone());
    tracing::info!(
        workflow_path = %workflow_path.display(),
        no_tui = cli.no_tui,
        sqlite_enabled = cli.sqlite_url.is_some(),
        "starting polyphony"
    );
    let user_config_path = user_config_path()?;
    if ensure_user_config_file(&user_config_path)? {
        tracing::info!(
            config_path = %user_config_path.display(),
            "created default user config file"
        );
    }
    if ensure_bootstrapped_workflow(&workflow_path, cli.no_tui, |workflow_path| {
        Ok(prompt_workflow_initialization(workflow_path)?)
    })? == WorkflowBootstrap::Canceled
    {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            "workflow initialization canceled"
        );
        return Ok(());
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
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))
    });
    let components = build_runtime_components(&workflow)?;
    let provisioner: Arc<dyn WorkspaceProvisioner> =
        Arc::new(polyphony_git::GitWorkspaceProvisioner);
    let store = build_store(cli.sqlite_url.as_deref()).await?;
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
        Some(user_config_path),
        workflow_tx.clone(),
        component_factory,
    );
    let _watcher = spawn_workflow_watcher(
        workflow_path.clone(),
        repo_config_path,
        handle.command_tx.clone(),
    )?;
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

fn format_fatal_error(error: &Error) -> String {
    match error {
        Error::Workflow(polyphony_workflow::Error::InvalidConfig(message)) => {
            format_invalid_config_error(message)
        },
        Error::Workflow(polyphony_workflow::Error::MissingWorkflowFile(path)) => format!(
            "Workflow file not found: {}\nRun `polyphony` from the repository root, or pass an explicit workflow path.",
            path.display()
        ),
        Error::Workflow(polyphony_workflow::Error::WorkflowParse(message)) => {
            format!("Could not parse WORKFLOW.md front matter.\n{message}")
        },
        Error::Workflow(polyphony_workflow::Error::FrontMatterNotMap) => {
            "WORKFLOW.md front matter must be a YAML mapping.".into()
        },
        Error::Workflow(polyphony_workflow::Error::TemplateParse(message)) => {
            format!("Could not parse the WORKFLOW.md prompt template.\n{message}")
        },
        Error::Workflow(polyphony_workflow::Error::TemplateRender(message)) => {
            format!("Could not render the WORKFLOW.md prompt template.\n{message}")
        },
        Error::Workflow(polyphony_workflow::Error::Config(message)) | Error::Config(message) => {
            format_config_error(message)
        },
        Error::Core(error) => format!("Polyphony failed.\n{error}"),
        Error::Runtime(error) => format!("Polyphony runtime failed.\n{error}"),
        Error::Tui(error) => format!("Polyphony TUI failed.\n{error}"),
        Error::Io(error) => format!("Polyphony failed to read or write a local file.\n{error}"),
    }
}

fn format_invalid_config_error(message: &str) -> String {
    match message {
        "tracker.repository is required for github" => "Invalid workflow configuration: the GitHub tracker is selected, but tracker.repository is missing.\nAdd `repository = \"owner/repo\"` to `polyphony.toml` or `WORKFLOW.md`.".into(),
        "tracker.project_slug is required for linear" => "Invalid workflow configuration: the Linear tracker is selected, but tracker.project_slug is missing.\nAdd `project_slug = \"ENG\"` to `polyphony.toml` or `WORKFLOW.md`.".into(),
        "tracker.api_key is required for linear" => "Invalid workflow configuration: the Linear tracker is selected, but tracker.api_key is missing.\nSet `api_key = \"$LINEAR_API_KEY\"` in config and export `LINEAR_API_KEY`.".into(),
        "agents.default is required" => "Invalid workflow configuration: agent profiles are defined, but agents.default is missing.".into(),
        message if message.starts_with("tracker.profile `") && message.ends_with("` is not defined") => {
            format!("Invalid workflow configuration: {message}.\nDefine the named profile under `trackers.profiles.<name>` in `~/.config/polyphony/config.toml`, or remove `tracker.profile` from repo-local config.")
        },
        _ => format!("Invalid workflow configuration.\n{message}"),
    }
}

fn format_config_error(message: &str) -> String {
    match message {
        "tracker.api_key is required for linear" => {
            format_invalid_config_error(message)
        },
        "tracker.repository is required for github" => {
            format_invalid_config_error(message)
        },
        "tracker.api_key is required for github automation" => "Invalid workflow configuration: GitHub automation is enabled, but tracker.api_key is missing.\nSet `api_key = \"$GITHUB_TOKEN\"` in `polyphony.toml` or `WORKFLOW.md`.".into(),
        "tracker.repository is required for github automation" => "Invalid workflow configuration: GitHub automation is enabled, but tracker.repository is missing.".into(),
        _ => message.to_string(),
    }
}

fn ensure_bootstrapped_workflow<F>(
    workflow_path: &Path,
    no_tui: bool,
    prompt_create_workflow: F,
) -> Result<WorkflowBootstrap, Error>
where
    F: FnOnce(&Path) -> Result<bool, Error>,
{
    if workflow_path.exists() {
        if workflow_path.is_file() {
            return Ok(WorkflowBootstrap::Ready);
        }
        return Err(Error::Config(format!(
            "workflow path `{}` exists but is not a file",
            workflow_path.display()
        )));
    }

    let should_create = if no_tui {
        true
    } else {
        prompt_create_workflow(workflow_path)?
    };
    if !should_create {
        return Ok(WorkflowBootstrap::Canceled);
    }

    if ensure_workflow_file(workflow_path)? {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            "created default workflow file"
        );
    }
    Ok(WorkflowBootstrap::Ready)
}

#[cfg(test)]
fn maybe_seed_repo_config_file(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<Option<PathBuf>, Error> {
    let repo_config_path = repo_config_path(workflow_path)?;
    if repo_config_path.exists() {
        if repo_config_path.is_file() {
            return Ok(Some(repo_config_path));
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            repo_config_path.display()
        )));
    }

    let workflow = load_workflow_with_user_config(workflow_path, user_config_path)?;
    let workflow_root = workflow_root_dir(workflow_path)?;
    if !should_seed_repo_config(&workflow.config, &workflow_root) {
        return Ok(None);
    }

    let source_repo_path = workflow_root
        .canonicalize()
        .unwrap_or_else(|_| workflow_root.clone());
    if ensure_repo_config_file(&repo_config_path, &source_repo_path)? {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            repo_config_path = %repo_config_path.display(),
            "created default repo-local config file"
        );
    }
    Ok(Some(repo_config_path))
}

/// Seed the repo config file, auto-detecting GitHub remotes.
///
/// Returns `(repo_config_path, first_run_no_github)`. When `first_run_no_github`
/// is `true`, no GitHub remote was found and a default config with `kind = "none"`
/// was written — the caller should exit with instructions.
fn maybe_seed_repo_config_with_github_detection(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<(Option<PathBuf>, bool), Error> {
    let rcp = repo_config_path(workflow_path)?;
    if rcp.exists() {
        if rcp.is_file() {
            return Ok((Some(rcp), false));
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            rcp.display()
        )));
    }

    let workflow = load_workflow_with_user_config(workflow_path, user_config_path)?;
    let workflow_root = workflow_root_dir(workflow_path)?;
    if !should_seed_repo_config(&workflow.config, &workflow_root) {
        return Ok((None, false));
    }

    let source_repo_path = workflow_root
        .canonicalize()
        .unwrap_or_else(|_| workflow_root.clone());

    // Try beads first (local tracker, highest priority).
    if workflow_root.join(".beads").is_dir() {
        if polyphony_workflow::seed_repo_config_with_beads(&rcp, &source_repo_path)? {
            eprintln!("Detected beads issue tracker — tracker configured automatically.");
            tracing::info!(
                workflow_path = %workflow_path.display(),
                repo_config_path = %rcp.display(),
                "created repo-local config with beads tracker"
            );
        }
        return Ok((Some(rcp), false));
    }

    // Try to detect a GitHub remote and pre-configure the tracker.
    if let Some(github_repo) = polyphony_git::detect_github_remote(&workflow_root) {
        if seed_repo_config_with_github(&rcp, &source_repo_path, &github_repo)? {
            eprintln!(
                "Detected GitHub repository: {github_repo} — tracker configured automatically."
            );
            tracing::info!(
                workflow_path = %workflow_path.display(),
                repo_config_path = %rcp.display(),
                github_repo = %github_repo,
                "created repo-local config with GitHub tracker"
            );
        }
        return Ok((Some(rcp), false));
    }

    // Fallback: seed with kind = "none" and signal that the user should configure manually.
    if ensure_repo_config_file(&rcp, &source_repo_path)? {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            repo_config_path = %rcp.display(),
            "created default repo-local config file (no GitHub remote detected)"
        );
    }
    Ok((Some(rcp), true))
}

fn should_seed_repo_config(config: &ServiceConfig, workflow_root: &Path) -> bool {
    workflow_root.join(".git").exists()
        && (config.tracker.kind == TrackerKind::None
            || (config.workspace.checkout_kind == CheckoutKind::Directory
                && config.workspace.source_repo_path.is_none()
                && config.workspace.clone_url.is_none()))
}

fn workflow_root_dir(workflow_path: &Path) -> Result<PathBuf, Error> {
    let parent = workflow_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    match parent {
        Some(parent) => Ok(parent.to_path_buf()),
        None => std::env::current_dir().map_err(Error::Io),
    }
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

struct EmptyTracker;

#[async_trait]
impl IssueTracker for EmptyTracker {
    fn component_key(&self) -> String {
        "tracker:none".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issues_by_ids(
        &self,
        _issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::Issue>, polyphony_core::Error> {
        Ok(Vec::new())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        _issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        Ok(Vec::new())
    }
}

#[allow(unused_variables)]
fn build_runtime_components(
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<RuntimeComponents, Error> {
    let tracker: Arc<dyn IssueTracker> = match workflow.config.tracker.kind {
        TrackerKind::None => Arc::new(EmptyTracker),
        #[cfg(feature = "linear")]
        TrackerKind::Linear => {
            let api_key =
                workflow.config.tracker.api_key.clone().ok_or_else(|| {
                    Error::Config("tracker.api_key is required for linear".into())
                })?;
            Arc::new(polyphony_linear::LinearTracker::new(
                workflow.config.tracker.endpoint.clone(),
                api_key,
                workflow.config.tracker.team_id.clone(),
            )?)
        },
        #[cfg(feature = "github")]
        TrackerKind::Github => Arc::new(polyphony_github::GithubIssueTracker::new(
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
        #[cfg(feature = "beads")]
        TrackerKind::Beads => {
            let workflow_root = workflow_root_dir(&workflow.path)?;
            Arc::new(polyphony_beads::BeadsTracker::new(workflow_root)?)
        },
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
    let (pull_request_manager, pull_request_commenter) = if workflow.config.automation.enabled
        && workflow.config.tracker.kind == TrackerKind::Github
    {
        let repository = workflow.config.tracker.repository.clone().ok_or_else(|| {
            Error::Config("tracker.repository is required for github automation".into())
        })?;
        let token = workflow.config.tracker.api_key.clone().ok_or_else(|| {
            Error::Config("tracker.api_key is required for github automation".into())
        })?;
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

    #[test]
    fn fatal_error_formats_workflow_config_errors_for_humans() {
        let error = Error::Workflow(polyphony_workflow::Error::InvalidConfig(
            "tracker.project_slug is required for linear".into(),
        ));

        let rendered = crate::format_fatal_error(&error);

        assert!(rendered.contains("Invalid workflow configuration"));
        assert!(rendered.contains("Linear tracker"));
        assert!(rendered.contains("project_slug"));
    }

    #[test]
    fn fatal_error_drops_debug_style_enum_wrapping() {
        let error = Error::Workflow(polyphony_workflow::Error::InvalidConfig(
            "tracker.repository is required for github".into(),
        ));

        let rendered = crate::format_fatal_error(&error);

        assert!(!rendered.contains("Workflow("));
        assert!(!rendered.contains("InvalidConfig("));
        assert!(rendered.contains("polyphony.toml"));
    }

    #[test]
    fn fatal_error_formats_runtime_config_messages_for_humans() {
        let error = Error::Config("tracker.api_key is required for github automation".into());

        let rendered = crate::format_fatal_error(&error);

        assert!(rendered.contains("GitHub automation is enabled"));
        assert!(rendered.contains("GITHUB_TOKEN"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod bootstrap_tests {
    use std::{fs, io};

    use {
        crate::{WorkflowBootstrap, ensure_bootstrapped_workflow, maybe_seed_repo_config_file},
        polyphony_workflow::repo_config_path,
    };

    fn unique_temp_path(name: &str, extension: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "polyphony-cli-{name}-{}.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            extension
        ))
    }

    #[test]
    fn headless_bootstrap_creates_missing_workflow() {
        let workflow_path = unique_temp_path("bootstrap-headless", "md");

        let outcome =
            ensure_bootstrapped_workflow(&workflow_path, true, |_path| Ok(false)).unwrap();
        let contents = fs::read_to_string(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);

        assert_eq!(outcome, WorkflowBootstrap::Ready);
        assert!(contents.contains("# Polyphony Workflow"));
    }

    #[test]
    fn interactive_bootstrap_can_cancel() {
        let workflow_path = unique_temp_path("bootstrap-cancel", "md");

        let outcome =
            ensure_bootstrapped_workflow(&workflow_path, false, |_path| Ok(false)).unwrap();

        assert_eq!(outcome, WorkflowBootstrap::Canceled);
        assert!(!workflow_path.exists());
    }

    #[test]
    fn interactive_bootstrap_can_create_workflow() {
        let workflow_path = unique_temp_path("bootstrap-create", "md");

        let outcome =
            ensure_bootstrapped_workflow(&workflow_path, false, |_path| Ok(true)).unwrap();
        let contents = fs::read_to_string(&workflow_path).unwrap();
        let _ = fs::remove_file(&workflow_path);

        assert_eq!(outcome, WorkflowBootstrap::Ready);
        assert!(contents.contains("# Polyphony Workflow"));
    }

    #[test]
    fn interactive_bootstrap_propagates_prompt_errors() {
        let workflow_path = unique_temp_path("bootstrap-error", "md");

        let error = ensure_bootstrapped_workflow(&workflow_path, false, |_path| {
            Err(crate::Error::Io(io::Error::other("prompt failed")))
        })
        .unwrap_err();

        assert!(matches!(error, crate::Error::Io(_)));
    }

    #[test]
    fn bootstrap_rejects_directory_paths() {
        let workflow_path = unique_temp_path("bootstrap-dir", "d");
        fs::create_dir_all(&workflow_path).unwrap();

        let error =
            ensure_bootstrapped_workflow(&workflow_path, true, |_path| Ok(true)).unwrap_err();
        let _ = fs::remove_dir_all(&workflow_path);

        assert!(matches!(error, crate::Error::Config(_)));
    }

    #[test]
    fn seeds_repo_config_for_generic_git_repo() {
        let repo_root = unique_temp_path("repo-config-seed", "d");
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        polyphony_workflow::ensure_workflow_file(&workflow_path).unwrap();

        let repo_config = maybe_seed_repo_config_file(&workflow_path, None).unwrap();
        let repo_config_path = repo_config_path(&workflow_path).unwrap();
        let contents = fs::read_to_string(&repo_config_path).unwrap();
        let _ = fs::remove_dir_all(&repo_root);

        assert_eq!(repo_config.as_deref(), Some(repo_config_path.as_path()));
        assert!(contents.contains("Polyphony repo-local config."));
        assert!(contents.contains("checkout_kind = \"linked_worktree\""));
    }

    #[test]
    fn skips_repo_config_seed_when_tracker_and_workspace_are_already_configured() {
        let repo_root = unique_temp_path("repo-config-skip", "d");
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        let workflow_path = repo_root.join("WORKFLOW.md");
        fs::write(
            &workflow_path,
            r#"---
tracker:
  kind: github
  repository: penso/polyphony
  api_key: test-token
workspace:
  checkout_kind: linked_worktree
  source_repo_path: /tmp/polyphony
---
# Prompt
"#,
        )
        .unwrap();

        let repo_config = maybe_seed_repo_config_file(&workflow_path, None).unwrap();
        let _ = fs::remove_dir_all(&repo_root);

        assert!(repo_config.is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tracing_tests {
    use std::io::Write as _;

    use tracing_subscriber::{EnvFilter, layer::SubscriberExt};

    use crate::{LogBuffer, TracingOutput, TracingOutputWriter};

    #[test]
    fn tracing_writer_flushes_into_tui_buffer() {
        let buffer = LogBuffer::default();
        let output = TracingOutput::tui(buffer.clone());
        let mut writer = TracingOutputWriter {
            output,
            buffer: Vec::new(),
        };

        writer.write_all(b"first line\nsecond line\n").unwrap();
        writer.flush().unwrap();

        assert_eq!(buffer.drain_oldest_first(), vec![
            "first line".to_string(),
            "second line".to_string()
        ]);
    }

    #[test]
    fn tracing_subscriber_routes_events_into_tui_buffer() {
        let buffer = LogBuffer::default();
        let output = TracingOutput::tui(buffer.clone());
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("info"))
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_writer(output)
                    .with_ansi(false),
            );
        let dispatch = tracing::Dispatch::new(subscriber);

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(component = "test", "subscriber log path works");
        });

        let lines = buffer.drain_oldest_first();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("subscriber log path works"));
        assert!(lines[0].contains("INFO"));
    }
}
