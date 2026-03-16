mod budget;

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use {
    chrono::{Local, TimeZone, Utc},
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentModel, AgentModelCatalog, AgentRunResult,
        AgentRunSpec, BudgetSnapshot, Error as CoreError, RateLimitSignal, TokenUsage,
    },
    serde_json::{Value, json},
    tokio::{
        fs,
        io::{AsyncBufReadExt, BufReader},
        process::Command,
        sync::mpsc,
    },
};

pub use crate::budget::fetch_budget_for_agent;

#[derive(Clone, Copy)]
pub enum BudgetField {
    Credits,
    Spending,
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
