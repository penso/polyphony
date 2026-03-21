use std::{path::PathBuf, time::Duration};

use chrono::{DateTime, Utc};
use polyphony_core::{AgentDefinition, BudgetSnapshot, Error as CoreError};
use serde_json::{Value, json};
use tokio::{fs, process::Command};

use crate::{BudgetField, apply_budget_probe, run_shell_capture};

const CODEX_USAGE_BASE_URL_ENV: &str = "POLYPHONY_CODEX_USAGE_BASE_URL";
const CODEX_TOKEN_URL_ENV: &str = "POLYPHONY_CODEX_OAUTH_TOKEN_URL";
const CLAUDE_USAGE_BASE_URL_ENV: &str = "POLYPHONY_CLAUDE_OAUTH_BASE_URL";
const CLAUDE_TOKEN_URL_ENV: &str = "POLYPHONY_CLAUDE_OAUTH_TOKEN_URL";
const CLAUDE_OAUTH_TOKEN_ENV: &str = "CODEXBAR_CLAUDE_OAUTH_TOKEN";

#[derive(Clone)]
struct CodexCredentials {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    account_id: Option<String>,
    auth_path: PathBuf,
    source_is_api_key: bool,
}

#[derive(Clone)]
struct ClaudeCredentials {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    source: ClaudeCredentialSource,
}

#[derive(Clone)]
enum ClaudeCredentialSource {
    File(PathBuf),
    Keychain,
    Environment,
}

struct WindowSnapshot {
    used_percent: f64,
    remaining_percent: f64,
    reset_at: Option<DateTime<Utc>>,
    window_seconds: Option<i64>,
}

struct PaceSnapshot {
    expected_used_percent: f64,
    deficit_percent: f64,
    reserve_percent: f64,
    eta_seconds: Option<i64>,
    will_last_to_reset: bool,
}

pub async fn fetch_budget_for_agent(
    agent: &AgentDefinition,
) -> Result<Option<BudgetSnapshot>, CoreError> {
    let native = fetch_native_budget_for_agent(agent).await?;
    if native.is_none() && agent.credits_command.is_none() && agent.spending_command.is_none() {
        return Ok(None);
    }

    let mut snapshot = native.unwrap_or_else(|| blank_budget_snapshot(agent));
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

async fn fetch_native_budget_for_agent(
    agent: &AgentDefinition,
) -> Result<Option<BudgetSnapshot>, CoreError> {
    match agent.kind.to_ascii_lowercase().as_str() {
        "codex" => fetch_codex_budget(agent).await,
        "claude" => fetch_claude_budget(agent).await,
        _ => Ok(None),
    }
}

fn blank_budget_snapshot(agent: &AgentDefinition) -> BudgetSnapshot {
    BudgetSnapshot {
        component: format!("agent:{}", agent.name),
        captured_at: Utc::now(),
        credits_remaining: None,
        credits_total: None,
        spent_usd: None,
        soft_limit_usd: None,
        hard_limit_usd: None,
        reset_at: None,
        raw: None,
    }
}

async fn fetch_codex_budget(agent: &AgentDefinition) -> Result<Option<BudgetSnapshot>, CoreError> {
    let Some(mut credentials) = load_codex_credentials(agent).await? else {
        return Ok(None);
    };
    let url = resolve_codex_usage_url(agent).await?;
    let client = budget_http_client()?;

    let usage = match fetch_json(
        &client,
        url.as_str(),
        &credentials.access_token,
        credentials.account_id.as_deref(),
        &[("User-Agent", "polyphony"), ("Accept", "application/json")],
    )
    .await
    {
        Ok(usage) => usage,
        Err(error)
            if should_retry_codex_with_refresh(&error)
                && credentials
                    .refresh_token
                    .as_ref()
                    .is_some_and(|token| !token.is_empty()) =>
        {
            credentials = refresh_codex_credentials(agent, &credentials, &client).await?;
            persist_codex_credentials(&credentials).await?;
            fetch_json(
                &client,
                url.as_str(),
                &credentials.access_token,
                credentials.account_id.as_deref(),
                &[("User-Agent", "polyphony"), ("Accept", "application/json")],
            )
            .await?
        },
        Err(error) => return Err(error),
    };

    let session = parse_codex_window(usage.pointer("/rate_limit/primary_window"));
    let weekly = parse_codex_window(usage.pointer("/rate_limit/secondary_window"));
    let credits_balance = json_number(usage.pointer("/credits/balance"));
    let plan_type = usage
        .get("plan_type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let weekly_pace = weekly
        .as_ref()
        .and_then(|window| pace_for_window(window, Utc::now()));

    let mut raw = json!({
        "provider": "codex",
        "plan_type": plan_type,
        "credits_balance": credits_balance,
    });
    if let Some(session) = &session {
        raw["credits_remaining"] = json!(session.remaining_percent);
        raw["session"] = json!({
            "used_percent": session.used_percent,
            "remaining_percent": session.remaining_percent,
            "reset_at": session.reset_at.as_ref().map(DateTime::<Utc>::to_rfc3339),
            "window_seconds": session.window_seconds,
        });
    }
    if let Some(weekly) = &weekly {
        raw["weekly_remaining"] = json!(weekly.remaining_percent);
        raw["weekly"] = json!({
            "used_percent": weekly.used_percent,
            "remaining_percent": weekly.remaining_percent,
            "reset_at": weekly.reset_at.as_ref().map(DateTime::<Utc>::to_rfc3339),
            "window_seconds": weekly.window_seconds,
            "expected_used_percent": weekly_pace.as_ref().map(|pace| pace.expected_used_percent),
            "deficit_percent": weekly_pace.as_ref().map(|pace| pace.deficit_percent).unwrap_or(0.0),
            "reserve_percent": weekly_pace.as_ref().map(|pace| pace.reserve_percent).unwrap_or(0.0),
            "eta_seconds": weekly_pace.as_ref().and_then(|pace| pace.eta_seconds),
            "will_last_to_reset": weekly_pace.as_ref().map(|pace| pace.will_last_to_reset).unwrap_or(false),
        });
        raw["weekly_deficit"] = json!(
            weekly_pace
                .as_ref()
                .map(|pace| pace.deficit_percent)
                .unwrap_or(0.0)
        );
    }

    Ok(Some(BudgetSnapshot {
        component: format!("agent:{}", agent.name),
        captured_at: Utc::now(),
        credits_remaining: session.as_ref().map(|window| window.remaining_percent),
        credits_total: session.as_ref().map(|_| 100.0),
        spent_usd: None,
        soft_limit_usd: None,
        hard_limit_usd: None,
        reset_at: session.as_ref().and_then(|window| window.reset_at),
        raw: Some(raw),
    }))
}

async fn fetch_claude_budget(agent: &AgentDefinition) -> Result<Option<BudgetSnapshot>, CoreError> {
    let Some(mut credentials) = load_claude_credentials(agent).await? else {
        return Ok(None);
    };
    let client = budget_http_client()?;
    if credentials
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
        && credentials
            .refresh_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
    {
        credentials = refresh_claude_credentials(agent, &credentials, &client).await?;
        persist_claude_credentials(&credentials).await?;
    }

    let url = format!(
        "{}/api/oauth/usage",
        env_or_agent(agent, CLAUDE_USAGE_BASE_URL_ENV)
            .unwrap_or_else(|| "https://api.anthropic.com".to_string())
            .trim_end_matches('/')
    );
    let usage = match fetch_json(&client, &url, &credentials.access_token, None, &[
        ("Accept", "application/json"),
        ("Content-Type", "application/json"),
        ("anthropic-beta", "oauth-2025-04-20"),
        ("User-Agent", "claude-code/2.1.0"),
    ])
    .await
    {
        Ok(usage) => usage,
        Err(error)
            if should_retry_claude_with_refresh(&error)
                && credentials
                    .refresh_token
                    .as_ref()
                    .is_some_and(|token| !token.is_empty()) =>
        {
            credentials = refresh_claude_credentials(agent, &credentials, &client).await?;
            persist_claude_credentials(&credentials).await?;
            fetch_json(&client, &url, &credentials.access_token, None, &[
                ("Accept", "application/json"),
                ("Content-Type", "application/json"),
                ("anthropic-beta", "oauth-2025-04-20"),
                ("User-Agent", "claude-code/2.1.0"),
            ])
            .await?
        },
        Err(error) => return Err(error),
    };

    let session = parse_claude_window(usage.get("five_hour"), 5 * 60 * 60);
    let weekly = parse_claude_window(usage.get("seven_day"), 7 * 24 * 60 * 60);
    let sonnet = parse_claude_window(usage.get("seven_day_sonnet"), 7 * 24 * 60 * 60)
        .or_else(|| parse_claude_window(usage.get("seven_day_opus"), 7 * 24 * 60 * 60));
    let extra_usage = usage.get("extra_usage").cloned();

    let weekly_pace = weekly
        .as_ref()
        .and_then(|window| pace_for_window(window, Utc::now()));

    let mut raw = json!({
        "provider": "claude",
    });
    if let Some(session) = &session {
        raw["credits_remaining"] = json!(session.remaining_percent);
        raw["session"] = json!({
            "used_percent": session.used_percent,
            "remaining_percent": session.remaining_percent,
            "reset_at": session.reset_at.as_ref().map(DateTime::<Utc>::to_rfc3339),
            "window_seconds": session.window_seconds,
        });
    }
    if let Some(weekly) = &weekly {
        raw["weekly_remaining"] = json!(weekly.remaining_percent);
        raw["weekly"] = json!({
            "used_percent": weekly.used_percent,
            "remaining_percent": weekly.remaining_percent,
            "reset_at": weekly.reset_at.as_ref().map(DateTime::<Utc>::to_rfc3339),
            "window_seconds": weekly.window_seconds,
            "expected_used_percent": weekly_pace.as_ref().map(|pace| pace.expected_used_percent),
            "deficit_percent": weekly_pace.as_ref().map(|pace| pace.deficit_percent).unwrap_or(0.0),
            "reserve_percent": weekly_pace.as_ref().map(|pace| pace.reserve_percent).unwrap_or(0.0),
            "eta_seconds": weekly_pace.as_ref().and_then(|pace| pace.eta_seconds),
            "will_last_to_reset": weekly_pace.as_ref().map(|pace| pace.will_last_to_reset).unwrap_or(false),
        });
        raw["weekly_deficit"] = json!(
            weekly_pace
                .as_ref()
                .map(|pace| pace.deficit_percent)
                .unwrap_or(0.0)
        );
    }
    if let Some(sonnet) = &sonnet {
        raw["sonnet"] = json!({
            "used_percent": sonnet.used_percent,
            "remaining_percent": sonnet.remaining_percent,
            "reset_at": sonnet.reset_at.as_ref().map(DateTime::<Utc>::to_rfc3339),
            "window_seconds": sonnet.window_seconds,
        });
    }
    if let Some(extra_usage) = extra_usage {
        raw["extra_usage"] = extra_usage;
    }

    Ok(Some(BudgetSnapshot {
        component: format!("agent:{}", agent.name),
        captured_at: Utc::now(),
        credits_remaining: session.as_ref().map(|window| window.remaining_percent),
        credits_total: session.as_ref().map(|_| 100.0),
        spent_usd: None,
        soft_limit_usd: None,
        hard_limit_usd: None,
        reset_at: session.as_ref().and_then(|window| window.reset_at),
        raw: Some(raw),
    }))
}

fn budget_http_client() -> Result<reqwest::Client, CoreError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| CoreError::Adapter(format!("unable to build budget http client: {error}")))
}

async fn fetch_json(
    client: &reqwest::Client,
    url: &str,
    bearer_token: &str,
    account_id: Option<&str>,
    headers: &[(&str, &str)],
) -> Result<Value, CoreError> {
    let mut request = client.get(url).bearer_auth(bearer_token);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(account_id) = account_id
        && !account_id.is_empty()
    {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    let response = request
        .send()
        .await
        .map_err(|error| CoreError::Adapter(format!("budget request failed: {error}")))?;
    let status = response.status();
    if status.as_u16() == 429 {
        let retry_after_ms = parse_retry_after(&response);
        let retry_after_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let body = response.text().await.unwrap_or_default();
        tracing::debug!(
            %url,
            ?retry_after_header,
            body_preview = &body[..body.len().min(200)],
            "budget endpoint returned 429"
        );
        return Err(CoreError::RateLimited(Box::new(
            polyphony_core::RateLimitSignal {
                component: format!("budget:{url}"),
                reason: "budget endpoint returned 429".into(),
                limited_at: Utc::now(),
                retry_after_ms,
                reset_at: None,
                status_code: Some(429),
                raw: serde_json::from_str(&body).ok(),
            },
        )));
    }
    let body = response
        .text()
        .await
        .map_err(|error| CoreError::Adapter(format!("budget response read failed: {error}")))?;
    if !status.is_success() {
        return Err(CoreError::Adapter(format!(
            "budget request to {} returned HTTP {}",
            url,
            status.as_u16()
        )));
    }
    serde_json::from_str(&body)
        .map_err(|error| CoreError::Adapter(format!("budget response decode failed: {error}")))
}

/// Minimum backoff for 429 responses when Retry-After is missing or zero (5 minutes).
const MIN_RETRY_AFTER_MS: u64 = 300_000;

/// Parse the `Retry-After` header from an HTTP response.
/// Supports both delay-seconds (e.g. `120`) and HTTP-date formats.
/// Returns at least `MIN_RETRY_AFTER_MS` to prevent tight retry loops.
fn parse_retry_after(response: &reqwest::Response) -> Option<u64> {
    let value = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok());
    let Some(value) = value else {
        return Some(MIN_RETRY_AFTER_MS);
    };
    // Try as integer seconds first
    if let Ok(seconds) = value.parse::<u64>() {
        return Some((seconds * 1000).max(MIN_RETRY_AFTER_MS));
    }
    // Try as float seconds
    if let Ok(seconds) = value.parse::<f64>() {
        return Some(((seconds * 1000.0) as u64).max(MIN_RETRY_AFTER_MS));
    }
    // Try as HTTP-date (RFC 2822 / RFC 7231)
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(value) {
        let delay_ms = date.signed_duration_since(Utc::now()).num_milliseconds();
        if delay_ms > 0 {
            return Some((delay_ms as u64).max(MIN_RETRY_AFTER_MS));
        }
    }
    Some(MIN_RETRY_AFTER_MS)
}

fn should_retry_codex_with_refresh(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::Adapter(message)
            if message.contains("HTTP 401") || message.contains("HTTP 403")
    )
}

fn should_retry_claude_with_refresh(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::Adapter(message)
            if message.contains("HTTP 401") || message.contains("HTTP 403")
    )
}

async fn load_codex_credentials(
    agent: &AgentDefinition,
) -> Result<Option<CodexCredentials>, CoreError> {
    let auth_path = codex_root(agent).join("auth.json");
    let Ok(data) = fs::read(&auth_path).await else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&data)
        .map_err(|error| CoreError::Adapter(format!("invalid codex auth.json: {error}")))?;

    let api_key = value
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(access_token) = api_key {
        return Ok(Some(CodexCredentials {
            access_token,
            refresh_token: None,
            id_token: None,
            account_id: None,
            auth_path,
            source_is_api_key: true,
        }));
    }

    let Some(tokens) = value.get("tokens") else {
        return Ok(None);
    };
    let Some(access_token) = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(None);
    };

    Ok(Some(CodexCredentials {
        access_token,
        refresh_token: tokens
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        id_token: tokens
            .get("id_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        account_id: tokens
            .get("account_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        auth_path,
        source_is_api_key: false,
    }))
}

async fn refresh_codex_credentials(
    agent: &AgentDefinition,
    credentials: &CodexCredentials,
    client: &reqwest::Client,
) -> Result<CodexCredentials, CoreError> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| CoreError::Adapter("codex refresh token missing".into()))?;
    let url = env_or_agent(agent, CODEX_TOKEN_URL_ENV)
        .unwrap_or_else(|| "https://auth.openai.com/oauth/token".to_string());
    let response = client
        .post(url)
        .json(&json!({
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "scope": "openid profile email",
        }))
        .send()
        .await
        .map_err(|error| CoreError::Adapter(format!("codex token refresh failed: {error}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| CoreError::Adapter(format!("codex token refresh read failed: {error}")))?;
    if !status.is_success() {
        return Err(CoreError::Adapter(format!(
            "codex token refresh returned HTTP {}",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_str(&body).map_err(|error| {
        CoreError::Adapter(format!("invalid codex token refresh payload: {error}"))
    })?;
    Ok(CodexCredentials {
        access_token: value
            .get("access_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| credentials.access_token.clone()),
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| credentials.refresh_token.clone()),
        id_token: value
            .get("id_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| credentials.id_token.clone()),
        account_id: credentials.account_id.clone(),
        auth_path: credentials.auth_path.clone(),
        source_is_api_key: false,
    })
}

async fn persist_codex_credentials(credentials: &CodexCredentials) -> Result<(), CoreError> {
    if credentials.source_is_api_key {
        return Ok(());
    }

    let Ok(data) = fs::read(&credentials.auth_path).await else {
        return Ok(());
    };
    let mut value: Value = serde_json::from_slice(&data)
        .map_err(|error| CoreError::Adapter(format!("invalid codex auth.json: {error}")))?;
    let Some(tokens) = value.get_mut("tokens").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    tokens.insert(
        "access_token".into(),
        Value::String(credentials.access_token.clone()),
    );
    if let Some(refresh_token) = &credentials.refresh_token {
        tokens.insert("refresh_token".into(), Value::String(refresh_token.clone()));
    }
    if let Some(id_token) = &credentials.id_token {
        tokens.insert("id_token".into(), Value::String(id_token.clone()));
    }
    value["last_refresh"] = Value::String(Utc::now().to_rfc3339());
    let payload = serde_json::to_vec_pretty(&value).map_err(|error| {
        CoreError::Adapter(format!("unable to encode codex auth.json: {error}"))
    })?;
    fs::write(&credentials.auth_path, payload)
        .await
        .map_err(|error| CoreError::Adapter(format!("unable to write codex auth.json: {error}")))
}

async fn resolve_codex_usage_url(agent: &AgentDefinition) -> Result<String, CoreError> {
    if let Some(base_url) = env_or_agent(agent, CODEX_USAGE_BASE_URL_ENV) {
        return Ok(normalize_codex_base_url(&base_url));
    }
    let config_path = codex_root(agent).join("config.toml");
    let base_url = match fs::read_to_string(config_path).await {
        Ok(config) => parse_chatgpt_base_url(&config)
            .unwrap_or_else(|| "https://chatgpt.com/backend-api".to_string()),
        Err(_) => "https://chatgpt.com/backend-api".to_string(),
    };
    Ok(normalize_codex_base_url(&base_url))
}

fn normalize_codex_base_url(base_url: &str) -> String {
    let mut normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        normalized = "https://chatgpt.com/backend-api".to_string();
    }
    if (normalized.starts_with("https://chatgpt.com")
        || normalized.starts_with("https://chat.openai.com"))
        && !normalized.contains("/backend-api")
    {
        normalized.push_str("/backend-api");
    }
    if normalized.contains("/backend-api") {
        format!("{normalized}/wham/usage")
    } else {
        format!("{normalized}/api/codex/usage")
    }
}

fn parse_chatgpt_base_url(config: &str) -> Option<String> {
    for raw_line in config.lines() {
        let line = raw_line.split('#').next()?.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '=');
        let key = parts.next()?.trim();
        let value = parts.next()?.trim();
        if key != "chatgpt_base_url" {
            continue;
        }
        return Some(
            value
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string(),
        );
    }
    None
}

fn codex_root(agent: &AgentDefinition) -> PathBuf {
    if let Some(root) = env_or_agent(agent, "CODEX_HOME")
        && !root.is_empty()
    {
        return PathBuf::from(root);
    }
    home_dir(agent).join(".codex")
}

async fn load_claude_credentials(
    agent: &AgentDefinition,
) -> Result<Option<ClaudeCredentials>, CoreError> {
    if let Some(token) = env_or_agent(agent, CLAUDE_OAUTH_TOKEN_ENV)
        && !token.is_empty()
    {
        return Ok(Some(ClaudeCredentials {
            access_token: token,
            refresh_token: None,
            expires_at: None,
            source: ClaudeCredentialSource::Environment,
        }));
    }

    let path = home_dir(agent).join(".claude").join(".credentials.json");
    if let Ok(data) = fs::read(&path).await {
        return parse_claude_credentials_blob(&data).map(|credentials| {
            Some(ClaudeCredentials {
                source: ClaudeCredentialSource::File(path),
                ..credentials
            })
        });
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(data) = read_claude_keychain_blob().await? {
            return parse_claude_credentials_blob(data.as_bytes()).map(|credentials| {
                Some(ClaudeCredentials {
                    source: ClaudeCredentialSource::Keychain,
                    ..credentials
                })
            });
        }
    }

    Ok(None)
}

fn parse_claude_credentials_blob(data: &[u8]) -> Result<ClaudeCredentials, CoreError> {
    let value: Value = serde_json::from_slice(data).map_err(|error| {
        CoreError::Adapter(format!("invalid claude credentials payload: {error}"))
    })?;
    let oauth = value
        .get("claudeAiOauth")
        .ok_or_else(|| CoreError::Adapter("claude credentials are missing claudeAiOauth".into()))?;
    let access_token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| CoreError::Adapter("claude credentials are missing accessToken".into()))?;
    Ok(ClaudeCredentials {
        access_token,
        refresh_token: oauth
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        expires_at: oauth
            .get("expiresAt")
            .and_then(json_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis),
        source: ClaudeCredentialSource::Environment,
    })
}

async fn refresh_claude_credentials(
    agent: &AgentDefinition,
    credentials: &ClaudeCredentials,
    client: &reqwest::Client,
) -> Result<ClaudeCredentials, CoreError> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| CoreError::Adapter("claude refresh token missing".into()))?;
    let url = env_or_agent(agent, CLAUDE_TOKEN_URL_ENV)
        .unwrap_or_else(|| "https://platform.claude.com/v1/oauth/token".to_string());
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", "9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
        ])
        .send()
        .await
        .map_err(|error| CoreError::Adapter(format!("claude token refresh failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        CoreError::Adapter(format!("claude token refresh read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(CoreError::Adapter(format!(
            "claude token refresh returned HTTP {}",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_str(&body).map_err(|error| {
        CoreError::Adapter(format!("invalid claude token refresh payload: {error}"))
    })?;
    Ok(ClaudeCredentials {
        access_token: value
            .get("access_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| credentials.access_token.clone()),
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| credentials.refresh_token.clone()),
        expires_at: value
            .get("expires_in")
            .and_then(json_i64)
            .map(|seconds| Utc::now() + chrono::TimeDelta::seconds(seconds)),
        source: credentials.source.clone(),
    })
}

async fn persist_claude_credentials(credentials: &ClaudeCredentials) -> Result<(), CoreError> {
    let ClaudeCredentialSource::File(path) = &credentials.source else {
        return Ok(());
    };
    let Ok(data) = fs::read(path).await else {
        return Ok(());
    };
    let mut value: Value = serde_json::from_slice(&data)
        .map_err(|error| CoreError::Adapter(format!("invalid claude credentials file: {error}")))?;
    let Some(oauth) = value
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    oauth.insert(
        "accessToken".into(),
        Value::String(credentials.access_token.clone()),
    );
    if let Some(refresh_token) = &credentials.refresh_token {
        oauth.insert("refreshToken".into(), Value::String(refresh_token.clone()));
    }
    if let Some(expires_at) = credentials.expires_at {
        oauth.insert(
            "expiresAt".into(),
            Value::Number(serde_json::Number::from(expires_at.timestamp_millis())),
        );
    }
    let payload = serde_json::to_vec_pretty(&value).map_err(|error| {
        CoreError::Adapter(format!("unable to encode claude credentials file: {error}"))
    })?;
    fs::write(path, payload).await.map_err(|error| {
        CoreError::Adapter(format!("unable to write claude credentials file: {error}"))
    })
}

#[cfg(target_os = "macos")]
async fn read_claude_keychain_blob() -> Result<Option<String>, CoreError> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-w",
            "-s",
            "Claude Code-credentials",
        ])
        .output()
        .await
        .map_err(|error| CoreError::Adapter(format!("claude keychain read failed: {error}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout).map_err(|error| {
        CoreError::Adapter(format!("claude keychain output was not utf-8: {error}"))
    })?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

fn parse_codex_window(value: Option<&Value>) -> Option<WindowSnapshot> {
    let value = value?;
    let used_percent = json_number(value.get("used_percent"))?;
    let remaining_percent = (100.0 - used_percent).clamp(0.0, 100.0);
    Some(WindowSnapshot {
        used_percent,
        remaining_percent,
        reset_at: value
            .get("reset_at")
            .and_then(json_i64)
            .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0)),
        window_seconds: value.get("limit_window_seconds").and_then(json_i64),
    })
}

fn parse_claude_window(
    value: Option<&Value>,
    default_window_seconds: i64,
) -> Option<WindowSnapshot> {
    let value = value?;
    let used_percent = json_number(value.get("utilization"))?;
    let remaining_percent = (100.0 - used_percent).clamp(0.0, 100.0);
    Some(WindowSnapshot {
        used_percent,
        remaining_percent,
        reset_at: value
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc),
        window_seconds: Some(default_window_seconds),
    })
}

fn pace_for_window(window: &WindowSnapshot, now: DateTime<Utc>) -> Option<PaceSnapshot> {
    let reset_at = window.reset_at?;
    let window_seconds = window.window_seconds?;
    if window_seconds <= 0 {
        return None;
    }
    let time_until_reset = (reset_at - now).num_seconds();
    if time_until_reset <= 0 || time_until_reset > window_seconds {
        return None;
    }
    let duration = window_seconds as f64;
    let elapsed = (duration - time_until_reset as f64).clamp(0.0, duration);
    let actual_used_percent = window.used_percent.clamp(0.0, 100.0);
    if elapsed == 0.0 && actual_used_percent > 0.0 {
        return None;
    }
    let expected_used_percent = (elapsed / duration * 100.0).clamp(0.0, 100.0);
    let delta = actual_used_percent - expected_used_percent;

    let (eta_seconds, will_last_to_reset) = if elapsed > 0.0 && actual_used_percent > 0.0 {
        let rate = actual_used_percent / elapsed;
        if rate > 0.0 {
            let remaining = (100.0 - actual_used_percent).max(0.0);
            let candidate = remaining / rate;
            if candidate >= time_until_reset as f64 {
                (None, true)
            } else {
                (Some(candidate.round() as i64), false)
            }
        } else {
            (None, false)
        }
    } else if elapsed > 0.0 && actual_used_percent == 0.0 {
        (None, true)
    } else {
        (None, false)
    };

    Some(PaceSnapshot {
        expected_used_percent,
        deficit_percent: delta.max(0.0),
        reserve_percent: (-delta).max(0.0),
        eta_seconds,
        will_last_to_reset,
    })
}

fn json_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(string)) => string.parse::<f64>().ok(),
        _ => None,
    }
}

fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(string) => string.parse::<i64>().ok(),
        _ => None,
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn env_or_agent(agent: &AgentDefinition, key: &str) -> Option<String> {
    agent
        .env
        .get(key)
        .cloned()
        .or_else(|| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn home_dir(agent: &AgentDefinition) -> PathBuf {
    if let Some(home) = env_or_agent(agent, "HOME") {
        return PathBuf::from(home);
    }
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

    use chrono::Utc;
    use polyphony_core::AgentDefinition;
    use serde_json::{Value, json};
    use tempfile::tempdir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Mutex,
    };

    use super::{
        fetch_budget_for_agent, normalize_codex_base_url, parse_chatgpt_base_url,
        parse_claude_credentials_blob,
    };

    async fn spawn_json_server(body: serde_json::Value) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_log = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let body = body.to_string();
                let request_log = Arc::clone(&request_log);
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; 4096];
                    let bytes_read = stream.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
                    request_log.lock().await.push(request);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (addr, requests)
    }

    #[tokio::test]
    async fn fetches_codex_budget_from_auth_file_and_usage_api() {
        let temp = tempdir().unwrap();
        let codex_root = temp.path().join(".codex");
        tokio::fs::create_dir_all(&codex_root).await.unwrap();
        tokio::fs::write(
            codex_root.join("auth.json"),
            r#"{
                "tokens": {
                    "access_token": "codex-access",
                    "refresh_token": "codex-refresh",
                    "account_id": "acct-123"
                }
            }"#,
        )
        .await
        .unwrap();

        let now = Utc::now();
        let session_reset = now + chrono::TimeDelta::hours(2);
        let weekly_reset = now + chrono::TimeDelta::days(6);
        let (addr, requests) = spawn_json_server(json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 7,
                    "reset_at": session_reset.timestamp(),
                    "limit_window_seconds": 18000
                },
                "secondary_window": {
                    "used_percent": 50,
                    "reset_at": weekly_reset.timestamp(),
                    "limit_window_seconds": 604800
                }
            },
            "credits": {
                "balance": "0"
            }
        }))
        .await;

        tokio::fs::write(
            codex_root.join("config.toml"),
            format!("chatgpt_base_url = \"http://{addr}\"\n"),
        )
        .await
        .unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), temp.path().display().to_string());
        env.insert("CODEX_HOME".into(), codex_root.display().to_string());
        let snapshot = fetch_budget_for_agent(&AgentDefinition {
            name: "router".into(),
            kind: "codex".into(),
            env,
            ..AgentDefinition::default()
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.credits_total, Some(100.0));
        assert_eq!(snapshot.credits_remaining, Some(93.0));
        assert_eq!(
            snapshot
                .raw
                .as_ref()
                .and_then(|value| value.get("provider"))
                .and_then(Value::as_str),
            Some("codex")
        );
        assert!(snapshot.has_credit_headroom());
        assert!(snapshot.has_weekly_credit_deficit());
        assert!(
            snapshot
                .raw
                .as_ref()
                .and_then(|value| value.pointer("/weekly/deficit_percent"))
                .and_then(Value::as_f64)
                .is_some_and(|value| value > 30.0)
        );

        let request = requests.lock().await.join("\n");
        assert!(request.contains("GET /api/codex/usage"), "{request}");
        assert!(
            request.contains("authorization: Bearer codex-access"),
            "{request}"
        );
        assert!(
            request.contains("chatgpt-account-id: acct-123"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn fetches_claude_budget_from_credentials_file_and_usage_api() {
        let temp = tempdir().unwrap();
        let claude_root = temp.path().join(".claude");
        tokio::fs::create_dir_all(&claude_root).await.unwrap();
        tokio::fs::write(
            claude_root.join(".credentials.json"),
            format!(
                r#"{{
                    "claudeAiOauth": {{
                        "accessToken": "claude-access",
                        "refreshToken": "claude-refresh",
                        "expiresAt": {}
                    }}
                }}"#,
                (Utc::now() + chrono::TimeDelta::hours(1)).timestamp_millis()
            ),
        )
        .await
        .unwrap();

        let now = Utc::now();
        let session_reset = now + chrono::TimeDelta::hours(2);
        let weekly_reset = now + chrono::TimeDelta::days(4);
        let (addr, requests) = spawn_json_server(json!({
            "five_hour": {
                "utilization": 89,
                "resets_at": session_reset.to_rfc3339()
            },
            "seven_day": {
                "utilization": 56,
                "resets_at": weekly_reset.to_rfc3339()
            }
        }))
        .await;

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), temp.path().display().to_string());
        env.insert(
            "POLYPHONY_CLAUDE_OAUTH_BASE_URL".into(),
            format!("http://{addr}"),
        );
        let snapshot = fetch_budget_for_agent(&AgentDefinition {
            name: "reviewer".into(),
            kind: "claude".into(),
            env,
            ..AgentDefinition::default()
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.credits_remaining, Some(11.0));
        assert!(snapshot.has_credit_headroom());
        assert!(snapshot.has_weekly_credit_deficit());
        assert!(
            snapshot
                .raw
                .as_ref()
                .and_then(|value| value.pointer("/weekly/deficit_percent"))
                .and_then(Value::as_f64)
                .is_some_and(|value| (12.0..=14.5).contains(&value))
        );

        let request = requests.lock().await.join("\n");
        assert!(request.contains("GET /api/oauth/usage"), "{request}");
        assert!(
            request.contains("authorization: Bearer claude-access"),
            "{request}"
        );
        assert!(
            request.contains("anthropic-beta: oauth-2025-04-20"),
            "{request}"
        );
    }

    #[test]
    fn parses_claude_keychain_credentials_blob() {
        let credentials = parse_claude_credentials_blob(
            br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat","refreshToken":"sk-ant-ort","expiresAt":1773699826082}}"#,
        )
        .unwrap();
        assert_eq!(credentials.access_token, "sk-ant-oat");
        assert_eq!(credentials.refresh_token.as_deref(), Some("sk-ant-ort"));
        assert!(credentials.expires_at.is_some());
    }

    #[test]
    fn parses_codex_config_base_url() {
        assert_eq!(
            parse_chatgpt_base_url("chatgpt_base_url = \"https://chat.openai.com\"\n"),
            Some("https://chat.openai.com".into())
        );
        assert_eq!(
            normalize_codex_base_url("https://chat.openai.com"),
            "https://chat.openai.com/backend-api/wham/usage"
        );
        assert_eq!(
            normalize_codex_base_url("https://api.openai.com"),
            "https://api.openai.com/api/codex/usage"
        );
    }
}
