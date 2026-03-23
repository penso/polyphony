use serde::Serialize;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

use crate::{prelude::*, *};

pub(crate) const MAX_RECENT_EVENTS: usize = 256;
pub(crate) const MAX_RECENT_EVENT_MESSAGE_CHARS: usize = 120;
pub(crate) const MAX_RUN_HISTORY: usize = 256;
pub(crate) const MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES: usize = 24;
pub(crate) const MAX_PERSISTED_RUN_CONTEXT_TRANSCRIPT_ENTRIES: usize = 12;
pub(crate) const MAX_SAVED_CONTEXT_MESSAGE_CHARS: usize = 2_048;

fn path_fingerprint(path: &Path) -> Result<WorkflowFileFingerprint, std::io::Error> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(WorkflowFileFingerprint::Present {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(WorkflowFileFingerprint::Missing)
        },
        Err(error) => Err(error),
    }
}

pub(crate) fn workflow_inputs_fingerprint(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<WorkflowInputsFingerprint, std::io::Error> {
    let mut entries = vec![(
        workflow_path.to_path_buf(),
        path_fingerprint(workflow_path)?,
    )];

    if let Ok(repo_config) = repo_config_path(workflow_path) {
        entries.push((repo_config.clone(), path_fingerprint(&repo_config)?));
    }

    if let Ok(agent_dirs) = agent_prompt_dirs(workflow_path, user_config_path) {
        for dir in agent_dirs {
            entries.push((dir.clone(), path_fingerprint(&dir)?));
            if dir.is_dir() {
                let mut files = fs::read_dir(&dir)?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.is_file()
                            && path.extension().and_then(|ext| ext.to_str()) == Some("md")
                    })
                    .collect::<Vec<_>>();
                files.sort();
                for file in files {
                    entries.push((file.clone(), path_fingerprint(&file)?));
                }
            }
        }
    }

    Ok(WorkflowInputsFingerprint { entries })
}

pub(crate) async fn run_worker_attempt(
    workspace_manager: &WorkspaceManager,
    hooks: &HooksConfig,
    agent: Arc<dyn AgentRuntime>,
    tracker: Arc<dyn IssueTracker>,
    issue: Issue,
    attempt: Option<u32>,
    workspace_path: PathBuf,
    prompt: String,
    active_states: Vec<String>,
    max_turns: u32,
    continuation_prompt_template: Option<String>,
    selected_agent: polyphony_core::AgentDefinition,
    saved_context: Option<AgentContextSnapshot>,
    command_tx: mpsc::UnboundedSender<OrchestratorMessage>,
) -> Result<AgentRunResult, Error> {
    info!(
        issue_identifier = %issue.identifier,
        agent = %selected_agent.name,
        attempt = attempt.unwrap_or(0),
        max_turns,
        "starting worker attempt"
    );
    workspace_manager
        .run_before_run(hooks, &workspace_path)
        .await?;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let issue_id = issue.id.clone();
    let forward_command_tx = command_tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let _ = forward_command_tx.send(OrchestratorMessage::AgentEvent(event));
        }
    });
    let run_spec = AgentRunSpec {
        issue: issue.clone(),
        attempt,
        workspace_path: workspace_path.clone(),
        prompt: prompt.clone(),
        max_turns,
        agent: selected_agent,
        prior_context: saved_context,
    };
    let result = if let Some(mut session) = agent
        .start_session(run_spec.clone(), event_tx.clone())
        .await?
    {
        info!(
            issue_identifier = %run_spec.issue.identifier,
            agent = %run_spec.agent.name,
            "using live agent session"
        );
        let mut current_issue = issue;
        let mut current_prompt = prompt;
        let mut total_turns = 0;
        let mut turn_number = 1;
        let run_result = loop {
            info!(
                issue_identifier = %current_issue.identifier,
                turn_number,
                "starting live agent turn"
            );
            let turn_result = session.run_turn(current_prompt).await?;
            total_turns += turn_result.turns_completed;
            if !matches!(turn_result.status, AttemptStatus::Succeeded) {
                info!(
                    issue_identifier = %current_issue.identifier,
                    turn_number,
                    status = ?turn_result.status,
                    "live agent turn ended without success"
                );
                break Ok(AgentRunResult {
                    status: turn_result.status,
                    turns_completed: total_turns,
                    error: turn_result.error,
                    final_issue_state: turn_result.final_issue_state,
                });
            }

            let state_updates = tracker
                .fetch_issue_states_by_ids(&[current_issue.id.clone()])
                .await?;
            if let Some(updated_issue) = state_updates
                .into_iter()
                .find(|updated_issue| updated_issue.id == current_issue.id)
            {
                current_issue.state = updated_issue.state;
                current_issue.updated_at = updated_issue.updated_at;
            }
            debug!(
                issue_identifier = %current_issue.identifier,
                turn_number,
                state = %current_issue.state,
                "refreshed issue state after live turn"
            );

            if turn_number >= max_turns || !is_active_state(&active_states, &current_issue.state) {
                info!(
                    issue_identifier = %current_issue.identifier,
                    turn_number,
                    total_turns,
                    state = %current_issue.state,
                    "stopping live agent session"
                );
                break Ok(AgentRunResult {
                    status: AttemptStatus::Succeeded,
                    turns_completed: total_turns,
                    error: None,
                    final_issue_state: Some(current_issue.state.clone()),
                });
            }

            turn_number += 1;
            info!(
                issue_identifier = %current_issue.identifier,
                turn_number,
                state = %current_issue.state,
                "continuing live agent session"
            );
            current_prompt = build_continuation_prompt(
                &current_issue,
                attempt,
                turn_number,
                max_turns,
                continuation_prompt_template.as_deref(),
            )
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        };
        let stop_result = session.stop().await;
        match (run_result, stop_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(result), Ok(())) => Ok(result),
        }
    } else {
        info!(
            issue_identifier = %run_spec.issue.identifier,
            agent = %run_spec.agent.name,
            "provider does not support live sessions, falling back to single run"
        );
        agent.run(run_spec, event_tx).await
    };
    forwarder.abort();
    workspace_manager
        .run_after_run_best_effort(hooks, &workspace_path)
        .await;
    match result {
        Ok(result) => Ok(result),
        Err(CoreError::RateLimited(signal)) => {
            let _ = command_tx.send(OrchestratorMessage::RateLimited(signal.as_ref().clone()));
            warn!(issue_id = %issue_id, "worker attempt hit provider rate limit");
            Err(Error::Core(CoreError::RateLimited(signal)))
        },
        Err(error) => {
            warn!(issue_id = %issue_id, %error, "worker attempt failed");
            Err(Error::Core(error))
        },
    }
}

pub(crate) fn is_active_state(active_states: &[String], state: &str) -> bool {
    active_states
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(state))
}

pub(crate) fn synthetic_issue_for_pull_request_review(trigger: &PullRequestReviewTrigger) -> Issue {
    Issue {
        id: trigger.synthetic_issue_id(),
        identifier: trigger.display_identifier(),
        title: format!("Review PR #{}: {}", trigger.number, trigger.title),
        description: Some(format!(
            "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nCheckout ref: {}\nAuthor: {}\nLabels: {}",
            trigger.repository,
            trigger.base_branch,
            trigger.head_branch,
            trigger.head_sha,
            trigger.checkout_ref.as_deref().unwrap_or("<none>"),
            trigger.author_login.as_deref().unwrap_or("<unknown>"),
            if trigger.labels.is_empty() {
                "<none>".to_string()
            } else {
                trigger.labels.join(", ")
            }
        )),
        state: "Review".into(),
        branch_name: Some(format!("pr-review/{}", trigger.number)),
        url: trigger.url.clone(),
        updated_at: trigger.updated_at,
        ..Issue::default()
    }
}

pub(crate) fn synthetic_issue_for_pull_request_comment(
    trigger: &PullRequestCommentTrigger,
) -> Issue {
    let line = trigger
        .line
        .map(|line| format!(":{line}"))
        .unwrap_or_default();
    Issue {
        id: trigger.synthetic_issue_id(),
        identifier: trigger.display_identifier(),
        title: format!(
            "Review unresolved PR comment on {}{}: {}",
            trigger.path, line, trigger.pull_request_title
        ),
        description: Some(format!(
            "Repository: {}\nBase branch: {}\nHead branch: {}\nHead SHA: {}\nCheckout ref: {}\nPath: {}\nLine: {}\nAuthor: {}\nLabels: {}\n\nComment:\n{}",
            trigger.repository,
            trigger.base_branch,
            trigger.head_branch,
            trigger.head_sha,
            trigger.checkout_ref.as_deref().unwrap_or("<none>"),
            trigger.path,
            trigger
                .line
                .map(|line| line.to_string())
                .unwrap_or_else(|| "<none>".into()),
            trigger.author_login.as_deref().unwrap_or("<unknown>"),
            if trigger.labels.is_empty() {
                "<none>".to_string()
            } else {
                trigger.labels.join(", ")
            },
            trigger.body
        )),
        state: "Review".into(),
        branch_name: Some(format!("pr-comment-review/{}", trigger.number)),
        url: trigger.url.clone(),
        updated_at: trigger.updated_at.or(trigger.created_at),
        ..Issue::default()
    }
}

pub(crate) fn is_probably_bot_author(author: &str) -> bool {
    author.ends_with("[bot]")
        || author.ends_with("-bot")
        || author == "dependabot"
        || author.starts_with("dependabot-")
}

pub(crate) fn review_target_key(target: &ReviewTarget) -> String {
    format!(
        "pr_review:{}:{}:{}:{}",
        target.provider, target.repository, target.number, target.head_sha
    )
}

pub(crate) fn pull_request_review_comment_marker(target: &ReviewTarget) -> String {
    format!(
        "<!-- polyphony:pr-review {} {}#{} sha={} -->",
        target.provider, target.repository, target.number, target.head_sha
    )
}

pub(crate) fn pull_request_comment_review_comment_marker(
    target: &ReviewTarget,
    thread_id: &str,
) -> String {
    format!(
        "<!-- polyphony:pr-comment-review {} {}#{} sha={} thread={} -->",
        target.provider, target.repository, target.number, target.head_sha, thread_id
    )
}

pub(crate) fn pull_request_trigger_author(trigger: &PullRequestTrigger) -> Option<&str> {
    match trigger {
        PullRequestTrigger::Review(trigger) => trigger.author_login.as_deref(),
        PullRequestTrigger::Comment(trigger) => trigger.author_login.as_deref(),
        PullRequestTrigger::Conflict(trigger) => trigger.author_login.as_deref(),
    }
}

pub(crate) fn pull_request_trigger_subject(trigger: &PullRequestTrigger) -> String {
    match trigger {
        PullRequestTrigger::Review(trigger) => {
            format!("PR review {}", trigger.display_identifier())
        },
        PullRequestTrigger::Comment(trigger) => format!(
            "PR comment {} {}",
            trigger.display_identifier(),
            trigger.path
        ),
        PullRequestTrigger::Conflict(trigger) => format!(
            "PR conflict {} against {}",
            trigger.display_identifier(),
            trigger.base_branch
        ),
    }
}

pub(crate) fn pull_request_trigger_kind_label(trigger: &PullRequestTrigger) -> &'static str {
    match trigger {
        PullRequestTrigger::Review(_) => "PR review",
        PullRequestTrigger::Comment(_) => "PR comment",
        PullRequestTrigger::Conflict(_) => "PR conflict",
    }
}

pub(crate) fn truncate_for_trigger_title(value: &str, max_chars: usize) -> String {
    let trimmed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= max_chars {
        return trimmed;
    }
    let end = trimmed.floor_char_boundary(max_chars.saturating_sub(1));
    format!("{}…", &trimmed[..end])
}

pub(crate) async fn load_pull_request_review_comments(
    path: &Path,
) -> Result<Vec<PullRequestReviewComment>, Error> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::Io(error)),
    };
    let _ = tokio::fs::remove_file(path).await;
    let drafts = serde_json::from_str::<Vec<PullRequestReviewComment>>(&raw).map_err(|error| {
        Error::Core(CoreError::Adapter(format!(
            "invalid `.polyphony/review-comments.json`: {error}"
        )))
    })?;
    let mut comments = Vec::with_capacity(drafts.len());
    for draft in drafts {
        let path = draft.path.trim();
        let body = draft.body.trim();
        if path.is_empty() || body.is_empty() || draft.line == 0 {
            return Err(Error::Core(CoreError::Adapter(
                "review comments must include non-empty `path`, non-empty `body`, and `line` > 0"
                    .into(),
            )));
        }
        comments.push(PullRequestReviewComment {
            path: path.to_string(),
            line: draft.line,
            body: body.to_string(),
        });
    }
    Ok(comments)
}

pub(crate) fn build_continuation_prompt(
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
    template: Option<&str>,
) -> Result<String, polyphony_workflow::Error> {
    let source = template.unwrap_or(
        "Continue working on issue {{ issue.identifier }}: {{ issue.title }}.\n\
You are continuing the same live agent thread in the current workspace.\n\
Do not restart from scratch or repeat the original prompt.\n\
Current tracker state: {{ issue.state }}.\n\
This is continuation turn {{ turn_number }} of {{ max_turns }}.\n\
If the work is complete or blocked, say so explicitly. Otherwise continue with the next concrete steps.",
    );
    render_turn_template(source, issue, attempt, turn_number, max_turns)
}

pub(crate) fn agent_run_result_from_error(error: &Error) -> AgentRunResult {
    AgentRunResult {
        status: attempt_status_from_error(error),
        turns_completed: 0,
        error: Some(normalized_worker_error_message(error)),
        final_issue_state: None,
    }
}

pub(crate) fn attempt_status_from_error(error: &Error) -> AttemptStatus {
    match error {
        Error::Core(CoreError::Adapter(message))
            if matches!(message.as_str(), "response_timeout" | "turn_timeout") =>
        {
            AttemptStatus::TimedOut
        },
        _ => AttemptStatus::Failed,
    }
}

pub(crate) fn normalized_worker_error_message(error: &Error) -> String {
    match error {
        Error::Core(CoreError::Adapter(message)) => message.clone(),
        Error::Core(CoreError::RateLimited(signal)) => {
            format!("rate_limited: {}", signal.reason)
        },
        _ => error.to_string(),
    }
}

pub(crate) fn is_rate_limited_error(error: Option<&str>) -> bool {
    error.is_some_and(|message| {
        let lowered = message.to_ascii_lowercase();
        lowered.starts_with("rate_limited:")
            || lowered.contains("rate limit")
            || lowered.contains("usage limit")
            || lowered.contains("quota exhausted")
            || lowered.contains("out of tokens")
            || lowered.contains("no more tokens")
    })
}

pub(crate) fn should_skip_workspace_sync_for_retry(error: Option<&str>) -> bool {
    is_rate_limited_error(error)
}

pub(crate) fn apply_usage_delta(
    totals: &mut CodexTotals,
    running: &mut RunningTask,
    usage: TokenUsage,
) {
    let delta_input = usage
        .input_tokens
        .saturating_sub(running.last_reported_tokens.input_tokens);
    let delta_output = usage
        .output_tokens
        .saturating_sub(running.last_reported_tokens.output_tokens);
    let delta_total = usage
        .total_tokens
        .saturating_sub(running.last_reported_tokens.total_tokens);
    totals.input_tokens += delta_input;
    totals.output_tokens += delta_output;
    totals.total_tokens += delta_total;
    running.tokens = usage.clone();
    running.last_reported_tokens = usage;
}

pub(crate) fn dispatch_order(left: &Issue, right: &Issue) -> std::cmp::Ordering {
    left.priority
        .unwrap_or(i32::MAX)
        .cmp(&right.priority.unwrap_or(i32::MAX))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.identifier.cmp(&right.identifier))
}

pub(crate) fn empty_snapshot() -> RuntimeSnapshot {
    RuntimeSnapshot {
        generated_at: Utc::now(),
        counts: SnapshotCounts::default(),
        cadence: RuntimeCadence::default(),
        visible_issues: Vec::new(),
        visible_triggers: Vec::new(),
        approved_issue_keys: Vec::new(),
        running: Vec::new(),
        agent_history: Vec::new(),
        retrying: Vec::new(),
        codex_totals: CodexTotals::default(),
        rate_limits: None,
        throttles: Vec::new(),
        budgets: Vec::new(),
        agent_catalogs: Vec::new(),
        saved_contexts: Vec::new(),
        recent_events: Vec::new(),
        pending_user_interactions: Vec::new(),
        movements: Vec::new(),
        tasks: Vec::new(),
        loading: LoadingState::default(),
        dispatch_mode: polyphony_core::DispatchMode::default(),
        tracker_kind: polyphony_core::TrackerKind::default(),
        tracker_connection: None,
        from_cache: false,
        cached_at: None,
        agent_profile_names: Vec::new(),
        agent_profiles: Vec::new(),
    }
}

pub(crate) fn build_persisted_run_record(
    running: &RunningTask,
    status: AttemptStatus,
    finished_at: DateTime<Utc>,
    error: Option<String>,
    saved_context: Option<AgentContextSnapshot>,
) -> PersistedRunRecord {
    PersistedRunRecord {
        issue_id: running.issue.id.clone(),
        issue_identifier: running.issue.identifier.clone(),
        agent_name: running.agent_name.clone(),
        model: running.model.clone(),
        session_id: running.session_id.clone(),
        thread_id: running.thread_id.clone(),
        turn_id: running.turn_id.clone(),
        codex_app_server_pid: running.codex_app_server_pid.clone(),
        status,
        attempt: running.attempt,
        max_turns: running.max_turns,
        turn_count: running.turn_count,
        last_event: running.last_event.clone(),
        last_message: running.last_message.clone(),
        started_at: running.started_at,
        finished_at: Some(finished_at),
        last_event_at: running.last_event_at,
        tokens: running.tokens.clone(),
        workspace_path: Some(running.workspace_path.clone()),
        error,
        saved_context: compact_saved_context_for_history(saved_context),
    }
}

pub(crate) fn compact_saved_context_in_place(context: &mut AgentContextSnapshot) {
    for entry in &mut context.transcript {
        entry.message = truncate_chars(&entry.message, MAX_SAVED_CONTEXT_MESSAGE_CHARS);
    }
    if context.transcript.len() > MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES {
        let drain = context
            .transcript
            .len()
            .saturating_sub(MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES);
        context.transcript.drain(..drain);
    }
}

pub(crate) fn compact_saved_context_for_history(
    saved_context: Option<AgentContextSnapshot>,
) -> Option<AgentContextSnapshot> {
    saved_context.map(|mut context| {
        compact_saved_context_in_place(&mut context);
        if context.transcript.len() > MAX_PERSISTED_RUN_CONTEXT_TRANSCRIPT_ENTRIES {
            let drain = context
                .transcript
                .len()
                .saturating_sub(MAX_PERSISTED_RUN_CONTEXT_TRANSCRIPT_ENTRIES);
            context.transcript.drain(..drain);
        }
        context
    })
}

pub(crate) fn saved_context_metadata(mut context: AgentContextSnapshot) -> AgentContextSnapshot {
    context.transcript.clear();
    context
}

pub(crate) fn persisted_run_metadata(mut run: PersistedRunRecord) -> PersistedRunRecord {
    run.saved_context = None;
    run
}

pub(crate) fn truncate_runtime_event_message(message: String) -> String {
    // Strip multi-line noise: keep only the first meaningful line.
    // Agent events often embed full startup banners separated by newlines
    // or `--------` dividers, producing unreadable walls of text in the TUI.
    let first_line = message
        .split('\n')
        .next()
        .unwrap_or(&message)
        .split("--------")
        .next()
        .unwrap_or(&message)
        .trim_end();
    truncate_chars(first_line, MAX_RECENT_EVENT_MESSAGE_CHARS)
}

pub(crate) async fn append_workspace_agent_event_artifact(
    workspace_path: &Path,
    event: &AgentEvent,
) -> Result<(), Error> {
    append_workspace_jsonl_record(&workspace_agent_events_artifact_path(workspace_path), event)
        .await
}

pub(crate) async fn append_workspace_run_record_artifact(
    workspace_path: &Path,
    run: &PersistedRunRecord,
) -> Result<(), Error> {
    append_workspace_jsonl_record(&workspace_run_history_artifact_path(workspace_path), run).await
}

pub(crate) async fn persist_workspace_saved_context_artifact(
    workspace_path: &Path,
    context: &AgentContextSnapshot,
) -> Result<(), Error> {
    let artifact_dir = workspace_runtime_artifact_dir(workspace_path);
    tokio::fs::create_dir_all(&artifact_dir).await?;
    let payload = serde_json::to_vec_pretty(context)
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    tokio::fs::write(
        workspace_saved_context_artifact_path(workspace_path),
        payload,
    )
    .await?;
    Ok(())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let mut truncated = value[..end].to_string();
    truncated.push('…');
    truncated
}

async fn append_workspace_jsonl_record<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let payload =
        serde_json::to_vec(value).map_err(|error| CoreError::Adapter(error.to_string()))?;
    file.write_all(&payload).await?;
    file.write_all(b"\n").await?;
    Ok(())
}

pub(crate) fn issue_trigger_source(
    tracker_kind: polyphony_core::TrackerKind,
    row: &VisibleIssueRow,
) -> String {
    row.issue_id
        .split(':')
        .next()
        .filter(|prefix| matches!(*prefix, "github" | "beads" | "gitlab"))
        .map(str::to_string)
        .unwrap_or_else(|| tracker_kind.to_string())
}

pub(crate) fn issue_key_for_source(source: &str, issue_id: &str) -> String {
    if issue_id
        .split(':')
        .next()
        .is_some_and(|prefix| matches!(prefix, "github" | "beads" | "gitlab" | "linear" | "mock"))
    {
        issue_id.to_string()
    } else {
        format!("{source}:{issue_id}")
    }
}

pub(crate) fn approval_key_for_issue(
    tracker_kind: polyphony_core::TrackerKind,
    issue: &Issue,
) -> String {
    issue_key_for_source(&issue_source_for_issue(tracker_kind, issue), &issue.id)
}

pub(crate) fn approval_key_for_row(
    tracker_kind: polyphony_core::TrackerKind,
    row: &VisibleIssueRow,
) -> String {
    issue_key_for_source(&issue_trigger_source(tracker_kind, row), &row.issue_id)
}

pub(crate) fn issue_source_for_issue(
    tracker_kind: polyphony_core::TrackerKind,
    issue: &Issue,
) -> String {
    issue
        .id
        .split(':')
        .next()
        .filter(|prefix| matches!(*prefix, "github" | "beads" | "gitlab" | "linear" | "mock"))
        .map(str::to_string)
        .unwrap_or_else(|| tracker_kind.to_string())
}

pub(crate) fn summarize_issue(issue: &Issue) -> VisibleIssueRow {
    VisibleIssueRow {
        issue_id: issue.id.clone(),
        issue_identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        state: issue.state.clone(),
        approval_state: issue.approval_state,
        priority: issue.priority,
        labels: issue.labels.clone(),
        description: issue.description.clone(),
        url: issue.url.clone(),
        author: issue
            .author
            .as_ref()
            .and_then(|a| a.username.clone().or(a.display_name.clone())),
        parent_id: issue.parent_id.clone(),
        updated_at: issue.updated_at,
        created_at: issue.created_at,
        has_workspace: false,
    }
}

pub(crate) fn append_saved_context(
    prompt: String,
    saved_context: Option<&AgentContextSnapshot>,
    include: bool,
) -> String {
    if !include {
        return prompt;
    }
    let Some(saved_context) = saved_context else {
        return prompt;
    };
    let mut result = prompt;
    result.push_str("\n\n## Saved Polyphony Context\n");
    result.push_str(&format!(
        "Last agent: {}{}\n",
        saved_context.agent_name,
        saved_context
            .model
            .as_ref()
            .map(|model| format!(" ({model})"))
            .unwrap_or_default()
    ));
    if let Some(status) = &saved_context.status {
        result.push_str(&format!("Last status: {status}\n"));
    }
    if let Some(error) = &saved_context.error {
        result.push_str(&format!("Last error: {error}\n"));
    }
    result.push_str("Recent transcript:\n");
    for entry in saved_context.transcript.iter().rev().take(12).rev() {
        result.push_str(&format!(
            "- [{:?}] {}: {}\n",
            entry.kind,
            entry.at.to_rfc3339(),
            entry.message
        ));
    }
    result
}

pub(crate) fn prepend_manual_dispatch_directives(
    prompt: String,
    directives: Option<&str>,
) -> String {
    let Some(directives) = directives.map(str::trim).filter(|text| !text.is_empty()) else {
        return prompt;
    };
    format!(
        "## Operator Directives (Highest Priority)\n\
         Follow these directives before lower-priority issue text, generated plans, or prior assumptions.\n\
         If they conflict with the issue, call out the conflict and follow these directives.\n\n\
         {directives}\n\n\
         {prompt}"
    )
}

pub(crate) fn rotate_agent_candidates(
    candidate_agents: &[polyphony_core::AgentDefinition],
    previous_agent_name: Option<&str>,
    prefer_alternate_agent: bool,
) -> Vec<polyphony_core::AgentDefinition> {
    if !prefer_alternate_agent {
        return candidate_agents.to_vec();
    }
    let Some(previous_agent_name) = previous_agent_name else {
        return candidate_agents.to_vec();
    };
    let Some(previous_index) = candidate_agents
        .iter()
        .position(|agent| agent.name == previous_agent_name)
    else {
        return candidate_agents.to_vec();
    };
    if candidate_agents.len() <= 1 {
        return candidate_agents.to_vec();
    }

    candidate_agents[previous_index + 1..]
        .iter()
        .chain(candidate_agents[..=previous_index].iter())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn compact_saved_context_trims_old_entries_and_long_messages() {
        let now = Utc::now();
        let mut context = AgentContextSnapshot {
            issue_id: "1".into(),
            issue_identifier: "ISSUE-1".into(),
            updated_at: now,
            agent_name: "router".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: None,
            error: None,
            usage: TokenUsage::default(),
            transcript: (0..40)
                .map(|index| AgentContextEntry {
                    at: now,
                    kind: AgentEventKind::Notification,
                    message: format!("{index}-{}", "x".repeat(3_000)),
                })
                .collect(),
        };

        compact_saved_context_in_place(&mut context);

        assert_eq!(
            context.transcript.len(),
            MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES
        );
        assert!(context.transcript[0].message.starts_with("16-"));
        assert!(
            context
                .transcript
                .iter()
                .all(|entry| entry.message.chars().count() <= MAX_SAVED_CONTEXT_MESSAGE_CHARS + 1)
        );
    }

    #[test]
    fn compact_saved_context_for_history_keeps_only_recent_tail() {
        let now = Utc::now();
        let saved_context = AgentContextSnapshot {
            issue_id: "1".into(),
            issue_identifier: "ISSUE-1".into(),
            updated_at: now,
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: None,
            error: None,
            usage: TokenUsage::default(),
            transcript: (0..20)
                .map(|index| AgentContextEntry {
                    at: now,
                    kind: AgentEventKind::Notification,
                    message: format!("entry-{index}"),
                })
                .collect(),
        };

        let compacted = compact_saved_context_for_history(Some(saved_context)).unwrap();

        assert_eq!(
            compacted.transcript.len(),
            MAX_PERSISTED_RUN_CONTEXT_TRANSCRIPT_ENTRIES
        );
        assert_eq!(compacted.transcript[0].message, "entry-8");
        assert_eq!(compacted.transcript.last().unwrap().message, "entry-19");
    }

    #[tokio::test]
    async fn workspace_runtime_artifacts_are_written_under_polyphony_dir() {
        let workspace_path = std::env::temp_dir().join(format!(
            "polyphony-artifacts-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        if workspace_path.exists() {
            std::fs::remove_dir_all(&workspace_path).unwrap();
        }
        tokio::fs::create_dir_all(&workspace_path).await.unwrap();
        let now = Utc::now();
        let event = AgentEvent {
            issue_id: "issue-7".into(),
            issue_identifier: "DOG-7".into(),
            agent_name: "implementer".into(),
            session_id: Some("sess-1".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            kind: AgentEventKind::Notification,
            at: now,
            message: Some("hello".into()),
            usage: None,
            rate_limits: None,
            raw: None,
        };
        let context = AgentContextSnapshot {
            issue_id: "issue-7".into(),
            issue_identifier: "DOG-7".into(),
            updated_at: now,
            agent_name: "implementer".into(),
            model: None,
            session_id: Some("sess-1".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Succeeded),
            error: None,
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: now,
                kind: AgentEventKind::Notification,
                message: "hello".into(),
            }],
        };
        let run = PersistedRunRecord {
            issue_id: "issue-7".into(),
            issue_identifier: "DOG-7".into(),
            agent_name: "implementer".into(),
            model: None,
            session_id: Some("sess-1".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 3,
            turn_count: 1,
            last_event: Some("Notification".into()),
            last_message: Some("hello".into()),
            started_at: now,
            finished_at: Some(now),
            last_event_at: Some(now),
            tokens: TokenUsage::default(),
            workspace_path: Some(workspace_path.clone()),
            error: None,
            saved_context: Some(context.clone()),
        };

        append_workspace_agent_event_artifact(&workspace_path, &event)
            .await
            .unwrap();
        persist_workspace_saved_context_artifact(&workspace_path, &context)
            .await
            .unwrap();
        append_workspace_run_record_artifact(&workspace_path, &run)
            .await
            .unwrap();

        let artifact_dir = workspace_path.join(".polyphony").join("runtime");
        let events: String = tokio::fs::read_to_string(artifact_dir.join("agent-events.jsonl"))
            .await
            .unwrap();
        let saved_context: String =
            tokio::fs::read_to_string(artifact_dir.join("saved-context.json"))
                .await
                .unwrap();
        let runs: String = tokio::fs::read_to_string(artifact_dir.join("run-history.jsonl"))
            .await
            .unwrap();

        assert!(events.contains("\"issue_identifier\":\"DOG-7\""));
        assert!(saved_context.contains("\"issue_identifier\": \"DOG-7\""));
        assert!(runs.contains("\"issue_identifier\":\"DOG-7\""));
        std::fs::remove_dir_all(&workspace_path).unwrap();
    }
}
