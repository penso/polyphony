use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use {
    chrono::{Local, TimeZone, Utc},
    futures_util::StreamExt,
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentModel, AgentModelCatalog, AgentRunResult,
        AgentRunSpec, BudgetSnapshot, Error as CoreError, RateLimitSignal, TokenUsage,
        ToolCallRequest, ToolExecutor,
    },
    reqwest::header::CONTENT_TYPE,
    serde_json::{Value, json},
    tokio::{
        fs,
        io::{AsyncBufReadExt, BufReader},
        process::Command,
        sync::mpsc,
    },
    tracing::{debug, info, warn},
    uuid::Uuid,
};

#[derive(Clone, Copy)]
pub enum BudgetField {
    Credits,
    Spending,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleChatRequest {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub startup_message: &'static str,
    pub rate_limit_reason: &'static str,
    pub provider_label: &'static str,
}

pub fn emit(
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    spec: &AgentRunSpec,
    kind: AgentEventKind,
    message: Option<String>,
    session_id: Option<String>,
    usage: Option<TokenUsage>,
    rate_limits: Option<Value>,
    raw: Option<Value>,
) {
    emit_with_metadata(
        event_tx,
        spec,
        kind,
        message,
        session_id,
        None,
        None,
        None,
        usage,
        rate_limits,
        raw,
    );
}

pub fn emit_with_metadata(
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    spec: &AgentRunSpec,
    kind: AgentEventKind,
    message: Option<String>,
    session_id: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    codex_app_server_pid: Option<String>,
    usage: Option<TokenUsage>,
    rate_limits: Option<Value>,
    raw: Option<Value>,
) {
    let _ = event_tx.send(AgentEvent {
        issue_id: spec.issue.id.clone(),
        issue_identifier: spec.issue.identifier.clone(),
        agent_name: spec.agent.name.clone(),
        session_id,
        thread_id,
        turn_id,
        codex_app_server_pid,
        kind,
        at: Utc::now(),
        message,
        usage,
        rate_limits,
        raw,
    });
}

pub async fn prepare_prompt_file(spec: &AgentRunSpec) -> Result<PathBuf, CoreError> {
    let run_dir = spec.workspace_path.join(".polyphony");
    fs::create_dir_all(&run_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let prompt_file = run_dir.join(format!("{}-prompt.md", spec.agent.name));
    fs::write(&prompt_file, &spec.prompt)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    Ok(prompt_file)
}

pub async fn prepare_context_file(spec: &AgentRunSpec) -> Result<Option<PathBuf>, CoreError> {
    let Some(prior_context) = &spec.prior_context else {
        return Ok(None);
    };
    let run_dir = spec.workspace_path.join(".polyphony");
    fs::create_dir_all(&run_dir)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    let context_file = run_dir.join(format!("{}-context.json", spec.agent.name));
    let payload = serde_json::to_vec_pretty(prior_context)
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    fs::write(&context_file, payload)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    Ok(Some(context_file))
}

pub fn base_agent_env(
    spec: &AgentRunSpec,
    prompt_file: &Path,
    context_file: Option<&Path>,
    model: Option<&str>,
) -> BTreeMap<String, String> {
    let mut envs = BTreeMap::new();
    envs.insert("POLYPHONY_PROMPT".into(), spec.prompt.clone());
    envs.insert(
        "POLYPHONY_PROMPT_FILE".into(),
        prompt_file.to_string_lossy().to_string(),
    );
    envs.insert("POLYPHONY_ISSUE_ID".into(), spec.issue.id.clone());
    envs.insert(
        "POLYPHONY_ISSUE_IDENTIFIER".into(),
        spec.issue.identifier.clone(),
    );
    envs.insert("POLYPHONY_ISSUE_TITLE".into(), spec.issue.title.clone());
    envs.insert("POLYPHONY_AGENT_NAME".into(), spec.agent.name.clone());
    if let Some(model) = model {
        envs.insert("POLYPHONY_AGENT_MODEL".into(), model.to_string());
    }
    if let Some(context_file) = context_file {
        envs.insert(
            "POLYPHONY_CONTEXT_FILE".into(),
            context_file.to_string_lossy().to_string(),
        );
    }
    if let Some(prior_context) = &spec.prior_context {
        envs.insert(
            "POLYPHONY_CONTEXT_JSON".into(),
            serde_json::to_string(prior_context).unwrap_or_default(),
        );
        envs.insert(
            "POLYPHONY_PRIOR_AGENT".into(),
            prior_context.agent_name.clone(),
        );
    }
    envs
}

pub fn shell_command(
    command: &str,
    cwd: &Path,
    extra_env: &BTreeMap<String, String>,
    spec: &AgentRunSpec,
    prompt_file: &Path,
    context_file: Option<&Path>,
    model: Option<&str>,
) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command).current_dir(cwd);
    // Clear nesting-guard env vars so child CLI agents can start their own
    // sessions (e.g. Claude Code's CLAUDECODE check).
    cmd.env_remove("CLAUDECODE");
    for (key, value) in base_agent_env(spec, prompt_file, context_file, model) {
        cmd.env(key, value);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd
}

pub async fn run_shell_capture(
    command: &str,
    cwd: Option<&Path>,
    extra_env: &BTreeMap<String, String>,
) -> Result<String, CoreError> {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let output = cmd
        .output()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    if !output.status.success() {
        return Err(CoreError::Adapter(format!(
            "command `{command}` exited with status {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn forward_reader_lines<R>(
    reader: BufReader<R>,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    spec: AgentRunSpec,
    session_id: String,
    stream_name: String,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let message = if stream_name == "stdout" {
            line
        } else {
            format!("{stream_name}: {line}")
        };
        emit(
            &event_tx,
            &spec,
            AgentEventKind::Notification,
            Some(message),
            Some(session_id.clone()),
            None,
            None,
            None,
        );
    }
}

pub fn status_to_result(
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    session_id: Option<String>,
    code: Option<i32>,
) -> AgentRunResult {
    if code.unwrap_or(1) == 0 {
        emit(
            event_tx,
            spec,
            AgentEventKind::TurnCompleted,
            Some("turn completed".into()),
            session_id,
            None,
            None,
            None,
        );
        AgentRunResult::succeeded(1)
    } else {
        let error = format!("agent exited with status {}", code.unwrap_or(-1));
        emit(
            event_tx,
            spec,
            AgentEventKind::TurnFailed,
            Some(error.clone()),
            session_id,
            None,
            None,
            None,
        );
        AgentRunResult::failed(error)
    }
}

pub fn extract_text_rate_limit_signal(spec: &AgentRunSpec, text: &str) -> Option<RateLimitSignal> {
    let lowered = text.to_ascii_lowercase();
    let matched = lowered.contains("usage limit")
        || lowered.contains("rate limit")
        || lowered.contains("rate-limited")
        || lowered.contains("hit your limit")
        || lowered.contains("try again later")
        || lowered.contains("quota exhausted")
        || lowered.contains("out of tokens")
        || lowered.contains("no more tokens");
    if !matched {
        return None;
    }

    let reset_at = extract_reset_at(&lowered);
    let retry_after_ms = extract_retry_after_ms(&lowered)
        .or_else(|| reset_at.and_then(duration_until_reset_ms))
        .or_else(|| {
            if spec.agent.kind.eq_ignore_ascii_case("claude") {
                Some(5 * 60 * 60 * 1000)
            } else {
                None
            }
        });
    let reset_at = reset_at.or_else(|| {
        retry_after_ms.map(|ms| Utc::now() + chrono::Duration::milliseconds(ms as i64))
    });
    let reason = first_non_empty_line(text)
        .unwrap_or("agent rate limited")
        .trim()
        .to_string();

    Some(RateLimitSignal {
        component: format!("agent:{}", spec.agent.name),
        reason,
        limited_at: Utc::now(),
        retry_after_ms,
        reset_at,
        status_code: Some(429),
        raw: Some(json!({
            "kind": spec.agent.kind,
            "agent": spec.agent.name,
            "text": text,
        })),
    })
}

fn extract_retry_after_ms(text: &str) -> Option<u64> {
    let tokens: Vec<&str> = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();

    for window in tokens.windows(2) {
        let Some(number) = window[0].parse::<u64>().ok() else {
            continue;
        };
        let multiplier = match window[1] {
            "second" | "seconds" | "sec" | "secs" => 1_000,
            "minute" | "minutes" | "min" | "mins" => 60_000,
            "hour" | "hours" | "hr" | "hrs" => 3_600_000,
            _ => continue,
        };
        return Some(number.saturating_mul(multiplier));
    }

    None
}

fn extract_reset_at(text: &str) -> Option<chrono::DateTime<Utc>> {
    let tokens: Vec<&str> = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();

    for window in tokens.windows(2) {
        if window[0] != "resets" && window[0] != "reset" {
            continue;
        }
        if let Some(reset_at) = parse_clock_token(window[1]) {
            return Some(reset_at);
        }
    }

    None
}

fn parse_clock_token(token: &str) -> Option<chrono::DateTime<Utc>> {
    let (hour_12, is_pm) = if let Some(value) = token.strip_suffix("am") {
        (value.parse::<u32>().ok()?, false)
    } else if let Some(value) = token.strip_suffix("pm") {
        (value.parse::<u32>().ok()?, true)
    } else {
        return None;
    };

    if !(1..=12).contains(&hour_12) {
        return None;
    }

    let hour_24 = match (hour_12, is_pm) {
        (12, false) => 0,
        (12, true) => 12,
        (hour, true) => hour + 12,
        (hour, false) => hour,
    };

    let now_local = Local::now();
    let today = now_local.date_naive();
    let naive_time = chrono::NaiveTime::from_hms_opt(hour_24, 0, 0)?;
    let naive_dt = today.and_time(naive_time);
    let mut local_dt = Local.from_local_datetime(&naive_dt).single()?;
    if local_dt <= now_local {
        local_dt += chrono::Duration::days(1);
    }
    Some(local_dt.with_timezone(&Utc))
}

fn duration_until_reset_ms(reset_at: chrono::DateTime<Utc>) -> Option<u64> {
    reset_at
        .signed_duration_since(Utc::now())
        .to_std()
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn first_non_empty_line(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

pub fn parse_model_list(output: &str) -> Result<Vec<AgentModel>, CoreError> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(items) = value.get("data").and_then(Value::as_array) {
            return Ok(items.iter().filter_map(model_from_json).collect());
        }
        if let Some(items) = value.as_array() {
            return Ok(items.iter().filter_map(model_from_json).collect());
        }
    }
    Ok(trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| AgentModel {
            id: line.to_string(),
            display_name: None,
            created_at: None,
        })
        .collect())
}

pub fn model_from_json(value: &Value) -> Option<AgentModel> {
    if let Some(id) = value.as_str() {
        return Some(AgentModel {
            id: id.to_string(),
            display_name: None,
            created_at: None,
        });
    }
    Some(AgentModel {
        id: value.get("id")?.as_str()?.to_string(),
        display_name: value
            .get("display_name")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        created_at: None,
    })
}

pub fn merge_models(configured: Vec<AgentModel>, discovered: Vec<AgentModel>) -> Vec<AgentModel> {
    let mut seen = HashSet::new();
    configured
        .into_iter()
        .chain(discovered)
        .filter(|m| seen.insert(m.id.clone()))
        .collect()
}

pub fn selected_model(agent: &AgentDefinition, models: &[AgentModel]) -> Option<String> {
    agent.model.clone().or_else(|| {
        agent
            .models
            .first()
            .cloned()
            .or_else(|| models.first().map(|model| model.id.clone()))
    })
}

pub async fn discover_models_from_command(
    agent: &AgentDefinition,
) -> Result<Option<AgentModelCatalog>, CoreError> {
    let Some(command) = &agent.models_command else {
        return Ok(None);
    };
    let configured_models = agent
        .models
        .iter()
        .cloned()
        .map(|id| AgentModel {
            id,
            display_name: None,
            created_at: None,
        })
        .collect::<Vec<_>>();
    let discovered = parse_model_list(&run_shell_capture(command, None, &agent.env).await?)?;
    let merged = merge_models(configured_models, discovered);
    if merged.is_empty() && agent.model.is_none() {
        return Ok(None);
    }
    Ok(Some(AgentModelCatalog {
        agent_name: agent.name.clone(),
        provider_kind: agent.kind.clone(),
        fetched_at: Utc::now(),
        selected_model: selected_model(agent, &merged),
        models: merged,
    }))
}

pub fn apply_budget_probe(
    snapshot: &mut BudgetSnapshot,
    output: &str,
    field: BudgetField,
) -> Result<(), CoreError> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        snapshot.raw = Some(value.clone());
        if let Some(number) = value.as_f64() {
            match field {
                BudgetField::Credits => snapshot.credits_remaining = Some(number),
                BudgetField::Spending => snapshot.spent_usd = Some(number),
            }
            return Ok(());
        }
        if let Some(obj) = value.as_object() {
            snapshot.credits_remaining = obj
                .get("credits_remaining")
                .and_then(Value::as_f64)
                .or(snapshot.credits_remaining);
            snapshot.credits_total = obj
                .get("credits_total")
                .and_then(Value::as_f64)
                .or(snapshot.credits_total);
            snapshot.spent_usd = obj
                .get("spent_usd")
                .and_then(Value::as_f64)
                .or(snapshot.spent_usd);
            snapshot.soft_limit_usd = obj
                .get("soft_limit_usd")
                .and_then(Value::as_f64)
                .or(snapshot.soft_limit_usd);
            snapshot.hard_limit_usd = obj
                .get("hard_limit_usd")
                .and_then(Value::as_f64)
                .or(snapshot.hard_limit_usd);
            return Ok(());
        }
    }
    if let Ok(number) = trimmed.parse::<f64>() {
        match field {
            BudgetField::Credits => snapshot.credits_remaining = Some(number),
            BudgetField::Spending => snapshot.spent_usd = Some(number),
        }
        return Ok(());
    }
    Err(CoreError::Adapter(format!(
        "unable to parse budget command output for {}",
        snapshot.component
    )))
}

pub async fn fetch_budget_for_agent(
    agent: &AgentDefinition,
) -> Result<Option<BudgetSnapshot>, CoreError> {
    if agent.credits_command.is_none() && agent.spending_command.is_none() {
        return Ok(None);
    }
    let mut snapshot = BudgetSnapshot {
        component: format!("agent:{}", agent.name),
        captured_at: Utc::now(),
        credits_remaining: None,
        credits_total: None,
        spent_usd: None,
        soft_limit_usd: None,
        hard_limit_usd: None,
        reset_at: None,
        raw: None,
    };
    if let Some(command) = &agent.credits_command {
        let output = run_shell_capture(command, None, &agent.env).await?;
        apply_budget_probe(&mut snapshot, &output, BudgetField::Credits)?;
    }
    if let Some(command) = &agent.spending_command {
        let output = run_shell_capture(command, None, &agent.env).await?;
        apply_budget_probe(&mut snapshot, &output, BudgetField::Spending)?;
    }
    Ok(Some(snapshot))
}

pub async fn run_openai_compatible_chat(
    client: &reqwest::Client,
    spec: AgentRunSpec,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    tool_executor: Option<&Arc<dyn ToolExecutor>>,
    request: OpenAiCompatibleChatRequest,
) -> Result<AgentRunResult, CoreError> {
    let session_id = format!("{}-{}", spec.agent.name, Uuid::new_v4());
    emit(
        &event_tx,
        &spec,
        AgentEventKind::SessionStarted,
        Some(request.startup_message.into()),
        Some(session_id.clone()),
        None,
        None,
        None,
    );
    emit(
        &event_tx,
        &spec,
        AgentEventKind::TurnStarted,
        Some("turn started".into()),
        Some(session_id.clone()),
        None,
        None,
        None,
    );

    let url = format!(
        "{}/chat/completions",
        request.base_url.trim_end_matches('/')
    );
    let mut messages = vec![json!({
        "role": "user",
        "content": spec.prompt,
    })];
    info!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        provider_kind = %spec.agent.kind,
        provider = request.provider_label,
        model = %request.model,
        base_url = %request.base_url,
        session_id = %session_id,
        "starting OpenAI-compatible agent session"
    );

    for tool_round in 0..4 {
        debug!(
            issue_identifier = %spec.issue.identifier,
            agent_name = %spec.agent.name,
            provider = request.provider_label,
            session_id = %session_id,
            tool_round = tool_round + 1,
            message_count = messages.len(),
            "sending OpenAI-compatible completion request"
        );
        let mut builder = client.post(&url).header("User-Agent", "polyphony");
        if let Some(api_key) = request.api_key.as_ref() {
            builder = builder.bearer_auth(api_key);
        }
        let response = builder
            .json(&json!({
                "model": request.model,
                "messages": messages,
                "stream": true,
                "stream_options": {"include_usage": true},
            }))
            .send()
            .await
            .map_err(|error| CoreError::Adapter(error.to_string()))?;
        let status = response.status();
        if status.as_u16() == 429 {
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                provider = request.provider_label,
                session_id = %session_id,
                "OpenAI-compatible agent turn hit rate limit"
            );
            return Err(CoreError::RateLimited(Box::new(RateLimitSignal {
                component: format!("agent:{}", spec.agent.name),
                reason: request.rate_limit_reason.into(),
                limited_at: Utc::now(),
                retry_after_ms: None,
                reset_at: None,
                status_code: Some(429),
                raw: None,
            })));
        }
        if !status.is_success() {
            let payload = response
                .text()
                .await
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            warn!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                provider = request.provider_label,
                session_id = %session_id,
                status = %status,
                "OpenAI-compatible agent turn failed"
            );
            return Err(CoreError::Adapter(format!(
                "{} failed with status {status}: {payload}",
                request.provider_label
            )));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        let turn = if content_type.contains("text/event-stream") {
            consume_openai_sse(response, &spec, &event_tx, &session_id).await?
        } else {
            consume_openai_json(response).await?
        };

        if !turn.text.is_empty() {
            emit(
                &event_tx,
                &spec,
                AgentEventKind::Notification,
                Some(turn.text.clone()),
                Some(session_id.clone()),
                None,
                None,
                None,
            );
        }
        if let Some(usage) = turn.usage.clone() {
            emit(
                &event_tx,
                &spec,
                AgentEventKind::UsageUpdated,
                Some("usage updated".into()),
                Some(session_id.clone()),
                Some(usage),
                None,
                None,
            );
        }

        if turn.tool_calls.is_empty() {
            emit(
                &event_tx,
                &spec,
                AgentEventKind::TurnCompleted,
                Some("turn completed".into()),
                Some(session_id.clone()),
                None,
                None,
                None,
            );
            info!(
                issue_identifier = %spec.issue.identifier,
                agent_name = %spec.agent.name,
                provider = request.provider_label,
                session_id = %session_id,
                "OpenAI-compatible agent turn completed"
            );
            return Ok(AgentRunResult::succeeded(1));
        }

        messages.push(json!({
            "role": "assistant",
            "content": turn.text,
            "tool_calls": turn.tool_calls,
        }));
        for call in turn.tool_calls {
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unsupported-tool");
            let call_name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            emit(
                &event_tx,
                &spec,
                AgentEventKind::ToolCallStarted,
                Some(format!("dynamic tool call requested ({call_name})")),
                Some(session_id.clone()),
                None,
                None,
                Some(call.clone()),
            );
            let result = execute_openai_tool_call(
                tool_executor,
                &spec,
                call_name,
                call_id,
                call.pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}"),
                &event_tx,
                &session_id,
                &call,
            )
            .await;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result,
            }));
        }
    }

    warn!(
        issue_identifier = %spec.issue.identifier,
        agent_name = %spec.agent.name,
        provider = request.provider_label,
        session_id = %session_id,
        "OpenAI-compatible tool loop exhausted"
    );
    Err(CoreError::Adapter(format!(
        "{} tool loop exhausted without a terminal response",
        request.provider_label
    )))
}

pub fn sanitize_session_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

pub fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn selected_model_hint(agent: &AgentDefinition) -> Option<String> {
    agent
        .model
        .clone()
        .or_else(|| agent.models.first().cloned())
}

async fn execute_openai_tool_call(
    tool_executor: Option<&Arc<dyn ToolExecutor>>,
    spec: &AgentRunSpec,
    call_name: &str,
    call_id: &str,
    raw_arguments: &str,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    session_id: &str,
    raw_call: &Value,
) -> String {
    let Some(executor) = tool_executor else {
        emit(
            event_tx,
            spec,
            AgentEventKind::ToolCallFailed,
            Some(format!("unsupported tool call requested: {call_name}")),
            Some(session_id.to_string()),
            None,
            None,
            Some(raw_call.clone()),
        );
        return "{\"success\":false,\"error\":\"unsupported_tool_call\"}".into();
    };
    let arguments = serde_json::from_str::<Value>(raw_arguments).unwrap_or_else(|_| json!({}));
    let supported = executor
        .list_tools(&spec.agent.name)
        .into_iter()
        .any(|tool| tool.name == call_name);
    if !supported {
        emit(
            event_tx,
            spec,
            AgentEventKind::ToolCallFailed,
            Some(format!("unsupported tool call requested: {call_name}")),
            Some(session_id.to_string()),
            None,
            None,
            Some(raw_call.clone()),
        );
        return "{\"success\":false,\"error\":\"unsupported_tool_call\"}".into();
    }
    match executor
        .execute(ToolCallRequest {
            name: call_name.to_string(),
            arguments,
            issue: spec.issue.clone(),
            workspace_path: spec.workspace_path.clone(),
            agent_name: spec.agent.name.clone(),
            call_id: Some(call_id.to_string()),
            thread_id: None,
            turn_id: None,
        })
        .await
    {
        Ok(result) => {
            emit(
                event_tx,
                spec,
                if result.success {
                    AgentEventKind::ToolCallCompleted
                } else {
                    AgentEventKind::ToolCallFailed
                },
                Some(format!(
                    "dynamic tool call {} ({call_name})",
                    if result.success {
                        "completed"
                    } else {
                        "failed"
                    }
                )),
                Some(session_id.to_string()),
                None,
                None,
                Some(json!({
                    "tool": call_name,
                    "result": result,
                })),
            );
            json!({
                "success": result.success,
                "output": result.output,
                "contentItems": result.content_items,
            })
            .to_string()
        },
        Err(error) => {
            emit(
                event_tx,
                spec,
                AgentEventKind::ToolCallFailed,
                Some(format!("dynamic tool call failed ({call_name})")),
                Some(session_id.to_string()),
                None,
                None,
                Some(json!({
                    "tool": call_name,
                    "error": error.to_string(),
                })),
            );
            json!({
                "success": false,
                "output": error.to_string(),
                "contentItems": [{
                    "type": "inputText",
                    "text": error.to_string(),
                }]
            })
            .to_string()
        },
    }
}

#[derive(Default)]
struct StreamedTurn {
    text: String,
    usage: Option<TokenUsage>,
    tool_calls: Vec<Value>,
}

async fn consume_openai_json(response: reqwest::Response) -> Result<StreamedTurn, CoreError> {
    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?;
    Ok(StreamedTurn {
        text: payload["choices"][0]["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned)
            .or_else(|| payload["output_text"].as_str().map(ToOwned::to_owned))
            .unwrap_or_default(),
        usage: parse_openai_usage(&payload),
        tool_calls: payload["choices"][0]["message"]["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    })
}

async fn consume_openai_sse(
    response: reqwest::Response,
    spec: &AgentRunSpec,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    session_id: &str,
) -> Result<StreamedTurn, CoreError> {
    let mut turn = StreamedTurn::default();
    let mut tool_builders: BTreeMap<usize, Value> = BTreeMap::new();
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| CoreError::Adapter(error.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();
            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }
            let data = line.trim_start_matches("data:").trim();
            if data == "[DONE]" {
                break;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            if let Some(delta) = value
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                turn.text.push_str(delta);
                emit(
                    event_tx,
                    spec,
                    AgentEventKind::Notification,
                    Some(delta.to_string()),
                    Some(session_id.to_string()),
                    None,
                    None,
                    Some(value.clone()),
                );
            }
            if let Some(usage) = parse_openai_usage(&value) {
                turn.usage = Some(usage);
            }
            if let Some(tool_calls) = value
                .pointer("/choices/0/delta/tool_calls")
                .and_then(Value::as_array)
            {
                for entry in tool_calls {
                    let index = entry.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let builder = tool_builders.entry(index).or_insert_with(|| json!({
                        "id": entry.get("id").cloned().unwrap_or(Value::Null),
                        "type": entry.get("type").cloned().unwrap_or(Value::String("function".into())),
                        "function": {
                            "name": entry.pointer("/function/name").cloned().unwrap_or(Value::String(String::new())),
                            "arguments": entry.pointer("/function/arguments").cloned().unwrap_or(Value::String(String::new())),
                        }
                    }));
                    if builder.get("id").is_some_and(Value::is_null) {
                        builder["id"] = entry.get("id").cloned().unwrap_or(Value::Null);
                    }
                    if let Some(name) = entry.pointer("/function/name").and_then(Value::as_str) {
                        builder["function"]["name"] = Value::String(name.to_string());
                    }
                    if let Some(args_delta) =
                        entry.pointer("/function/arguments").and_then(Value::as_str)
                    {
                        let existing = builder["function"]["arguments"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        builder["function"]["arguments"] =
                            Value::String(format!("{existing}{args_delta}"));
                    }
                }
            }
        }
    }
    turn.tool_calls = tool_builders.into_values().collect();
    Ok(turn)
}

pub fn parse_openai_usage(payload: &Value) -> Option<TokenUsage> {
    let usage = payload.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("input_tokens").and_then(Value::as_u64))
            .unwrap_or_default(),
        output_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
            .unwrap_or_default(),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use {
        super::{
            BudgetField, apply_budget_probe, base_agent_env, extract_text_rate_limit_signal,
            parse_model_list,
        },
        polyphony_core::{AgentContextSnapshot, AgentDefinition, AgentRunSpec, Issue, TokenUsage},
    };

    #[test]
    fn parses_model_list_from_json() {
        let models = parse_model_list(
            r#"{"data":[{"id":"gpt-4.1","display_name":"GPT-4.1"},{"id":"gpt-4.1-mini"}]}"#,
        )
        .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4.1");
        assert_eq!(models[0].display_name.as_deref(), Some("GPT-4.1"));
    }

    #[test]
    fn parses_budget_probe_json() {
        let mut snapshot = polyphony_core::BudgetSnapshot {
            component: "agent:test".into(),
            captured_at: chrono::Utc::now(),
            credits_remaining: None,
            credits_total: None,
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: None,
        };
        apply_budget_probe(
            &mut snapshot,
            r#"{"credits_remaining":12.5,"spent_usd":3.5}"#,
            BudgetField::Credits,
        )
        .unwrap();
        assert_eq!(snapshot.credits_remaining, Some(12.5));
        assert_eq!(snapshot.spent_usd, Some(3.5));
    }

    #[test]
    fn base_agent_env_exposes_prior_context_metadata() {
        let spec = AgentRunSpec {
            issue: Issue {
                id: "issue-1".into(),
                identifier: "FAC-1".into(),
                title: "Title".into(),
                state: "Todo".into(),
                ..Issue::default()
            },
            attempt: Some(2),
            workspace_path: std::env::temp_dir(),
            prompt: "Prompt".into(),
            max_turns: 4,
            agent: AgentDefinition {
                name: "kimi".into(),
                ..AgentDefinition::default()
            },
            prior_context: Some(AgentContextSnapshot {
                issue_id: "issue-1".into(),
                issue_identifier: "FAC-1".into(),
                updated_at: chrono::Utc::now(),
                agent_name: "codex".into(),
                model: Some("gpt-5-codex".into()),
                session_id: Some("sess-1".into()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                status: Some(polyphony_core::AttemptStatus::Failed),
                error: Some("rate limited".into()),
                usage: TokenUsage::default(),
                transcript: Vec::new(),
            }),
        };
        let prompt_file = std::env::temp_dir().join("polyphony-prompt.md");
        let context_file = std::env::temp_dir().join("polyphony-context.json");

        let env = base_agent_env(&spec, &prompt_file, Some(&context_file), Some("kimi-2.5"));

        assert_eq!(
            env.get("POLYPHONY_AGENT_MODEL").map(String::as_str),
            Some("kimi-2.5")
        );
        assert_eq!(
            env.get("POLYPHONY_PRIOR_AGENT").map(String::as_str),
            Some("codex")
        );
        assert_eq!(
            env.get("POLYPHONY_CONTEXT_FILE").map(String::as_str),
            Some(context_file.to_string_lossy().as_ref())
        );
        assert!(
            env.get("POLYPHONY_CONTEXT_JSON")
                .is_some_and(|payload| payload.contains("\"agent_name\":\"codex\""))
        );
    }

    #[test]
    fn detects_claude_limit_messages_from_text() {
        let spec = AgentRunSpec {
            issue: Issue {
                id: "1".into(),
                identifier: "TEST-1".into(),
                title: "Test".into(),
                state: "Todo".into(),
                ..Issue::default()
            },
            attempt: Some(0),
            workspace_path: std::env::temp_dir(),
            prompt: "Prompt".into(),
            max_turns: 1,
            prior_context: None,
            agent: AgentDefinition {
                name: "opus".into(),
                kind: "claude".into(),
                ..AgentDefinition::default()
            },
        };

        let signal = extract_text_rate_limit_signal(
            &spec,
            "You've hit your limit · resets 2am (Europe/Lisbon)",
        )
        .unwrap();

        assert_eq!(signal.component, "agent:opus");
        assert_eq!(signal.status_code, Some(429));
        assert!(signal.retry_after_ms.is_some());
        assert!(signal.reset_at.is_some());
    }
}
