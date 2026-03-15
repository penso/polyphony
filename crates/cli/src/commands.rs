use crate::{prelude::*, tracker_factory::EmptyTracker, *};

const SNAPSHOT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn build_tracker(
    workflow: &polyphony_workflow::LoadedWorkflow,
) -> Result<Arc<dyn IssueTracker>, Error> {
    #[allow(unused_variables)]
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
    Ok(tracker)
}

pub(crate) async fn handle_issue_command(
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
                serde_json::to_string_pretty(&issue).map_err(|e| Error::Config(e.to_string()))?
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
                serde_json::to_string_pretty(&issue).map_err(|e| Error::Config(e.to_string()))?
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
                .fetch_issues_by_states(workflow.config.tracker.project_slug.as_deref(), &states)
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&issues).map_err(|e| Error::Config(e.to_string()))?
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
        IssueAction::Comment { identifier, body } => {
            let comment = tracker
                .comment_on_issue(&polyphony_core::AddIssueCommentRequest {
                    id: identifier,
                    body,
                })
                .await?;
            print_json(&comment)?;
        },
    }
    Ok(())
}

pub(crate) async fn handle_data_command(
    action: DataAction,
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_path: &Path,
    sqlite_url: Option<&str>,
) -> Result<(), Error> {
    let snapshot = load_runtime_snapshot(workflow, workflow_path, sqlite_url).await?;
    match action {
        DataAction::Workspaces => {
            let workspaces = list_workspace_entries(workflow, &snapshot).await?;
            print_json(&workspaces)?;
        },
        DataAction::Snapshot => {
            print_json(&snapshot)?;
        },
        DataAction::Counts => {
            print_json(&snapshot.counts)?;
        },
        DataAction::Cadence => {
            print_json(&snapshot.cadence)?;
        },
        DataAction::Issues => {
            print_json(&snapshot.visible_issues)?;
        },
        DataAction::Triggers => {
            print_json(&snapshot.visible_triggers)?;
        },
        DataAction::Running => {
            print_json(&snapshot.running)?;
        },
        DataAction::History => {
            print_json(&snapshot.agent_history)?;
        },
        DataAction::Retrying => {
            print_json(&snapshot.retrying)?;
        },
        DataAction::Catalogs => {
            print_json(&snapshot.agent_catalogs)?;
        },
        DataAction::Events { limit } => {
            let events = snapshot
                .recent_events
                .into_iter()
                .rev()
                .take(limit)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>();
            print_json(&events)?;
        },
        DataAction::Agents => {
            let value = serde_json::json!({
                "running": snapshot.running,
                "history": snapshot.agent_history,
                "retrying": snapshot.retrying,
                "catalogs": snapshot.agent_catalogs,
            });
            print_json_value(&value)?;
        },
        DataAction::Tasks => {
            print_json(&snapshot.tasks)?;
        },
        DataAction::Movements => {
            print_json(&snapshot.movements)?;
        },
        DataAction::Budgets => {
            print_json(&snapshot.budgets)?;
        },
        DataAction::CodexTotals => {
            print_json(&snapshot.codex_totals)?;
        },
        DataAction::RateLimits => {
            print_json_value(&snapshot.rate_limits.unwrap_or(serde_json::Value::Null))?;
        },
        DataAction::Throttles => {
            print_json(&snapshot.throttles)?;
        },
        DataAction::Contexts => {
            print_json(&snapshot.saved_contexts)?;
        },
        DataAction::Loading => {
            print_json(&snapshot.loading)?;
        },
        DataAction::Tracker => {
            let value = serde_json::json!({
                "tracker_kind": snapshot.tracker_kind,
                "tracker_connection": snapshot.tracker_connection,
                "dispatch_mode": snapshot.dispatch_mode,
                "generated_at": snapshot.generated_at,
                "from_cache": snapshot.from_cache,
                "cached_at": snapshot.cached_at,
            });
            print_json_value(&value)?;
        },
        DataAction::Profiles => {
            print_json(&snapshot.agent_profile_names)?;
        },
    }
    Ok(())
}

pub(crate) fn handle_config_command(
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
    let mut value = serde_json::to_value(config).map_err(|e| Error::Config(e.to_string()))?;
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
        println!(
            "  active states: {}",
            config.tracker.active_states.join(", ")
        );
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
            let transport = profile.transport.as_deref().unwrap_or(&profile.kind);
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

pub(crate) fn handle_doctor_command(
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
                println!("  fallback `{fallback}` ... \u{2717} profile not defined");
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

async fn load_runtime_snapshot(
    workflow: &polyphony_workflow::LoadedWorkflow,
    workflow_path: &Path,
    sqlite_url: Option<&str>,
) -> Result<RuntimeSnapshot, Error> {
    let components = build_runtime_components(workflow)?;
    let provisioner: Arc<dyn WorkspaceProvisioner> =
        Arc::new(polyphony_git::GitWorkspaceProvisioner);
    let store = build_store(workflow_path, sqlite_url).await?;
    let cache_path = workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join("cache.json");
    let cache: Option<Arc<dyn NetworkCache>> = Some(Arc::new(
        polyphony_core::file_cache::FileNetworkCache::new(cache_path),
    ));
    let (_workflow_tx, workflow_rx) = watch::channel(workflow.clone());
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
    let mut snapshot_rx = handle.snapshot_rx.clone();
    let command_tx = handle.command_tx.clone();
    let service_task = tokio::spawn(service.run());

    let snapshot_result = wait_for_runtime_snapshot(&mut snapshot_rx).await;
    let _ = command_tx.send(RuntimeCommand::Shutdown);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        if let Ok(result) = service_task.await {
            let _ = result;
        }
    })
    .await;

    snapshot_result
}

async fn wait_for_runtime_snapshot(
    snapshot_rx: &mut watch::Receiver<RuntimeSnapshot>,
) -> Result<RuntimeSnapshot, Error> {
    let initial = snapshot_rx.borrow().clone();
    if snapshot_is_ready(&initial) {
        return Ok(initial);
    }

    let started = tokio::time::Instant::now();
    loop {
        let remaining = SNAPSHOT_WAIT_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            let snapshot = snapshot_rx.borrow().clone();
            return Err(Error::Config(format!(
                "timed out waiting for runtime snapshot; last generated_at={}",
                snapshot.generated_at
            )));
        }
        tokio::time::timeout(remaining, snapshot_rx.changed())
            .await
            .map_err(|_| Error::Config("timed out waiting for runtime snapshot".into()))?
            .map_err(|error| Error::Config(format!("runtime snapshot channel closed: {error}")))?;
        let snapshot = snapshot_rx.borrow().clone();
        if snapshot_is_ready(&snapshot) {
            return Ok(snapshot);
        }
    }
}

fn snapshot_is_ready(snapshot: &RuntimeSnapshot) -> bool {
    snapshot.cadence.last_tracker_poll_at.is_some()
        && !snapshot.loading.fetching_issues
        && !snapshot.loading.fetching_budgets
        && !snapshot.loading.fetching_models
        && !snapshot.loading.reconciling
}

async fn list_workspace_entries(
    workflow: &polyphony_workflow::LoadedWorkflow,
    snapshot: &RuntimeSnapshot,
) -> Result<serde_json::Value, Error> {
    let manager = polyphony_workspace::WorkspaceManager::new(
        workflow.config.workspace.root.clone(),
        Arc::new(polyphony_git::GitWorkspaceProvisioner),
        workflow.config.workspace.checkout_kind,
        workflow.config.workspace.sync_on_reuse,
        workflow.config.workspace.transient_paths.clone(),
        workflow.config.workspace.source_repo_path.clone(),
        workflow.config.workspace.clone_url.clone(),
        workflow.config.workspace.default_branch.clone(),
    );
    let mut known = manager.list_workspaces().await;
    known.sort_by(|left, right| left.0.cmp(&right.0));

    let movement_by_workspace = snapshot
        .movements
        .iter()
        .filter_map(|movement| {
            movement
                .workspace_key
                .as_ref()
                .map(|key| (key.clone(), movement))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let running_by_workspace = snapshot
        .running
        .iter()
        .map(|running| {
            (
                running.workspace_path.clone(),
                serde_json::json!({
                    "issue_identifier": running.issue_identifier,
                    "agent_name": running.agent_name,
                    "state": running.state,
                }),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();

    Ok(serde_json::Value::Array(
        known
            .into_iter()
            .map(|(workspace_key, path)| {
                let movement = movement_by_workspace.get(&workspace_key);
                serde_json::json!({
                    "workspace_key": workspace_key,
                    "path": path,
                    "movement": movement.map(|movement| serde_json::json!({
                        "id": movement.id,
                        "title": movement.title,
                        "status": movement.status,
                        "issue_identifier": movement.issue_identifier,
                    })),
                    "running": running_by_workspace.get(&path).cloned(),
                })
            })
            .collect(),
    ))
}

fn print_json(value: &impl serde::Serialize) -> Result<(), Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| Error::Config(error.to_string()))?
    );
    Ok(())
}

fn print_json_value(value: &serde_json::Value) -> Result<(), Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| Error::Config(error.to_string()))?
    );
    Ok(())
}

fn which_binary(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(name);
            if full.is_file() {
                Some(full)
            } else {
                None
            }
        })
    })
}

fn run_shell_command(cmd: &str) -> Result<std::process::Output, std::io::Error> {
    Command::new("bash").arg("-c").arg(cmd).output()
}
