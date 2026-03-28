use polyphony_core::TrackerKind;
use polyphony_workflow::{LoadedWorkflow, WebhookProviderConfig};

use crate::Error;

/// Result of auto-provisioning a webhook on a tracker.
pub(crate) struct ProvisionedWebhook {
    pub provider_name: String,
    pub config: WebhookProviderConfig,
}

/// Auto-provision a webhook on the configured tracker.
///
/// Returns the provider config to insert into `daemon.webhooks.providers`,
/// or `None` if the tracker doesn't support webhook provisioning.
pub(crate) async fn provision_webhook(
    workflow: &LoadedWorkflow,
    public_url: &str,
) -> Result<ProvisionedWebhook, Error> {
    let url = public_url.trim_end_matches('/');

    match workflow.config.tracker.kind {
        #[cfg(feature = "github")]
        TrackerKind::Github => provision_github(workflow, url).await,
        #[cfg(feature = "gitlab")]
        TrackerKind::Gitlab => provision_gitlab(workflow, url).await,
        #[cfg(feature = "linear")]
        TrackerKind::Linear => provision_linear(workflow, url).await,
        other => Err(Error::Config(format!(
            "webhook auto-provisioning is not supported for tracker: {other}"
        ))),
    }
}

/// List the tracker kinds that support webhook provisioning.
pub(crate) fn supported_trackers() -> &'static [TrackerKind] {
    &[
        #[cfg(feature = "github")]
        TrackerKind::Github,
        #[cfg(feature = "gitlab")]
        TrackerKind::Gitlab,
        #[cfg(feature = "linear")]
        TrackerKind::Linear,
    ]
}

fn generate_secret() -> String {
    use std::fmt::Write;
    let bytes: [u8; 32] = rand_bytes();
    let mut hex = String::with_capacity(64);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    // Use getrandom for cryptographically secure randomness.
    // If unavailable, fall back to timestamp-based entropy (not ideal but functional).
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut buf);
            return buf;
        }
    }
    // Fallback: hash the current timestamp
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seed = now.as_nanos();
    for (i, b) in buf.iter_mut().enumerate() {
        *b = ((seed >> (i % 16))
            ^ (seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(i as u128))) as u8;
    }
    buf
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

#[cfg(feature = "github")]
async fn provision_github(
    workflow: &LoadedWorkflow,
    public_url: &str,
) -> Result<ProvisionedWebhook, Error> {
    let repository = workflow
        .config
        .tracker
        .repository
        .as_deref()
        .ok_or_else(|| Error::Config("tracker.repository is required".into()))?;

    let (owner, repo) = repository
        .split_once('/')
        .ok_or_else(|| Error::Config("tracker.repository must be owner/repo".into()))?;

    let token = workflow
        .config
        .tracker
        .api_key
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .ok_or_else(|| {
            Error::Config(
                "GitHub API token required (set tracker.api_key or GITHUB_TOKEN env)".into(),
            )
        })?;

    let secret = generate_secret();
    let webhook_url = format!("{public_url}/webhooks/github");

    let crab = octocrab::Octocrab::builder()
        .personal_token(token)
        .build()
        .map_err(|e| Error::Config(format!("octocrab build: {e}")))?;

    let hook = octocrab::models::hooks::Hook {
        name: "web".into(),
        active: true,
        events: vec![
            octocrab::models::webhook_events::WebhookEventType::Issues,
            octocrab::models::webhook_events::WebhookEventType::IssueComment,
            octocrab::models::webhook_events::WebhookEventType::PullRequest,
            octocrab::models::webhook_events::WebhookEventType::PullRequestReview,
            octocrab::models::webhook_events::WebhookEventType::PullRequestReviewComment,
        ],
        config: octocrab::models::hooks::Config {
            url: webhook_url.clone(),
            content_type: Some(octocrab::models::hooks::ContentType::Json),
            secret: Some(secret.clone()),
            insecure_ssl: Some("0".into()),
        },
        ..Default::default()
    };

    crab.repos(owner, repo)
        .create_hook(hook)
        .await
        .map_err(|e| Error::Config(format!("GitHub webhook creation failed: {e}")))?;

    tracing::info!(provider = "github", url = %webhook_url, "webhook registered on {owner}/{repo}");

    Ok(ProvisionedWebhook {
        provider_name: "github".into(),
        config: WebhookProviderConfig {
            auth: "hmac_sha256".into(),
            secret,
            header: None,
        },
    })
}

// ---------------------------------------------------------------------------
// GitLab
// ---------------------------------------------------------------------------

#[cfg(feature = "gitlab")]
async fn provision_gitlab(
    workflow: &LoadedWorkflow,
    public_url: &str,
) -> Result<ProvisionedWebhook, Error> {
    let project_path = workflow
        .config
        .tracker
        .project_slug
        .as_deref()
        .or(workflow.config.tracker.repository.as_deref())
        .ok_or_else(|| {
            Error::Config("tracker.project_slug or repository is required for gitlab".into())
        })?;

    let endpoint = if workflow.config.tracker.endpoint.is_empty() {
        "https://gitlab.com"
    } else {
        workflow.config.tracker.endpoint.trim_end_matches('/')
    };

    let token = workflow
        .config
        .tracker
        .api_key
        .clone()
        .or_else(|| std::env::var("GITLAB_TOKEN").ok())
        .ok_or_else(|| {
            Error::Config(
                "GitLab API token required (set tracker.api_key or GITLAB_TOKEN env)".into(),
            )
        })?;

    let secret = generate_secret();
    let webhook_url = format!("{public_url}/webhooks/gitlab");
    let encoded_project = urlencoding::encode(project_path);
    let api_url = format!("{endpoint}/api/v4/projects/{encoded_project}/hooks");

    let body = serde_json::json!({
        "url": webhook_url,
        "token": secret,
        "push_events": false,
        "issues_events": true,
        "merge_requests_events": true,
        "note_events": true,
        "enable_ssl_verification": true,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(&api_url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Config(format!("GitLab webhook request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Config(format!(
            "GitLab webhook creation failed ({status}): {body}"
        )));
    }

    tracing::info!(provider = "gitlab", url = %webhook_url, "webhook registered on {project_path}");

    Ok(ProvisionedWebhook {
        provider_name: "gitlab".into(),
        config: WebhookProviderConfig {
            auth: "token_header".into(),
            secret,
            header: Some("X-Gitlab-Token".into()),
        },
    })
}

// ---------------------------------------------------------------------------
// Linear
// ---------------------------------------------------------------------------

#[cfg(feature = "linear")]
async fn provision_linear(
    workflow: &LoadedWorkflow,
    public_url: &str,
) -> Result<ProvisionedWebhook, Error> {
    let api_key = workflow
        .config
        .tracker
        .api_key
        .as_deref()
        .ok_or_else(|| Error::Config("tracker.api_key is required for Linear".into()))?;

    let endpoint = if workflow.config.tracker.endpoint.is_empty() {
        "https://api.linear.app/graphql"
    } else {
        workflow.config.tracker.endpoint.as_str()
    };

    let secret = generate_secret();
    let webhook_url = format!("{public_url}/webhooks/linear");

    // Linear uses a GraphQL mutation to create webhooks.
    // The webhook secret is set via the `secret` field on the createWebhook input.
    let query = r#"
        mutation CreateWebhook($input: WebhookCreateInput!) {
            webhookCreate(input: $input) {
                success
                webhook {
                    id
                    url
                    enabled
                }
            }
        }
    "#;

    // Linear webhook resource types we care about
    let variables = serde_json::json!({
        "input": {
            "url": webhook_url,
            "secret": secret,
            "resourceTypes": ["Issue", "Comment", "IssueLabel"],
            "enabled": true,
        }
    });

    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(endpoint)
        .header("Authorization", api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Config(format!("Linear webhook request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Config(format!(
            "Linear webhook creation failed ({status}): {text}"
        )));
    }

    let result: serde_json::Value = response
        .json()
        .await
        .map_err(|e| Error::Config(format!("Linear webhook response parse: {e}")))?;

    if let Some(errors) = result.get("errors") {
        return Err(Error::Config(format!(
            "Linear webhook creation failed: {errors}"
        )));
    }

    let success = result
        .pointer("/data/webhookCreate/success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !success {
        return Err(Error::Config(format!(
            "Linear webhook creation unsuccessful: {result}"
        )));
    }

    tracing::info!(provider = "linear", url = %webhook_url, "webhook registered on Linear");

    // Linear verifies webhooks by computing HMAC-SHA256 of the body with the
    // webhook secret and sending the raw hex signature in the `Linear-Signature`
    // header (no `sha256=` prefix). We use our `hmac_sha256_header` strategy.
    Ok(ProvisionedWebhook {
        provider_name: "linear".into(),
        config: WebhookProviderConfig {
            auth: "hmac_sha256_header".into(),
            secret,
            header: Some("Linear-Signature".into()),
        },
    })
}
