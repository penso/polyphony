use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use {
    chrono::Utc,
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentModel, AgentModelCatalog, AgentRunResult,
        AgentRunSpec, BudgetSnapshot, Error as CoreError, TokenUsage,
    },
    serde_json::Value,
    tokio::{
        fs,
        io::{AsyncBufReadExt, BufReader},
        process::Command,
        sync::mpsc,
    },
};

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
        super::{BudgetField, apply_budget_probe, base_agent_env, parse_model_list},
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
}
