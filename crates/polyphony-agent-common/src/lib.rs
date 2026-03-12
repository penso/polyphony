use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
};

use {
    chrono::Utc,
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentModel, AgentModelCatalog, AgentRunResult,
        AgentRunSpec, AttemptStatus, BudgetSnapshot, Error as CoreError, TokenUsage,
    },
    serde_json::Value,
    thiserror::Error,
    tokio::{
        fs,
        io::{AsyncBufReadExt, BufReader},
        process::Command,
        sync::mpsc,
    },
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("agent common error: {0}")]
    Common(String),
}

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
    let _ = event_tx.send(AgentEvent {
        issue_id: spec.issue.id.clone(),
        issue_identifier: spec.issue.identifier.clone(),
        agent_name: spec.agent.name.clone(),
        session_id,
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

pub fn base_agent_env(
    spec: &AgentRunSpec,
    prompt_file: &Path,
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
    envs
}

pub fn shell_command(
    command: &str,
    cwd: &Path,
    extra_env: &BTreeMap<String, String>,
    spec: &AgentRunSpec,
    prompt_file: &Path,
    model: Option<&str>,
) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command).current_dir(cwd);
    for (key, value) in base_agent_env(spec, prompt_file, model) {
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
    stream_name: &str,
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
        AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: 1,
            error: None,
            final_issue_state: None,
        }
    } else {
        emit(
            event_tx,
            spec,
            AgentEventKind::TurnFailed,
            Some(format!("agent exited with status {}", code.unwrap_or(-1))),
            session_id,
            None,
            None,
            None,
        );
        AgentRunResult {
            status: AttemptStatus::Failed,
            turns_completed: 0,
            error: Some(format!("agent exited with status {}", code.unwrap_or(-1))),
            final_issue_state: None,
        }
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
    let mut merged = Vec::new();
    for model in configured.into_iter().chain(discovered) {
        if seen.insert(model.id.clone()) {
            merged.push(model);
        }
    }
    merged
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

pub fn command_with_pipes(mut command: Command) -> Command {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    command
}

#[cfg(test)]
mod tests {
    use super::{BudgetField, apply_budget_probe, parse_model_list};

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
}
