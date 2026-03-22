use std::env;

use polyphony_core::{Error as CoreError, IssueTracker, TrackerConnectionState, TrackerQuery};

#[cfg(feature = "github")]
use polyphony_github::GithubIssueTracker;
#[cfg(feature = "linear")]
use polyphony_linear::LinearTracker;
use serde::Deserialize;
#[cfg(feature = "linear")]
use serde_json::json;

#[cfg(feature = "linear")]
const LINEAR_ENDPOINT: &str = "https://api.linear.app/graphql";

fn required_env(test_name: &str, env_name: &str, help: &str) -> Option<String> {
    match env::var(env_name) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                println!("skipping {test_name}: {env_name} is set but empty ({help})");
                None
            } else {
                Some(trimmed.to_string())
            }
        },
        Err(_) => {
            println!("skipping {test_name}: missing {env_name} ({help})");
            None
        },
    }
}

#[cfg(feature = "github")]
struct GithubSmokeConfig {
    token: String,
    repository: String,
}

#[cfg(feature = "github")]
impl GithubSmokeConfig {
    fn from_env(test_name: &str) -> Option<Self> {
        let token = required_env(
            test_name,
            "GITHUB_TOKEN",
            "export a token with issue read access",
        )?;
        let repository = required_env(
            test_name,
            "GITHUB_TEST_REPOSITORY",
            "export an isolated owner/repo slug for the smoke test",
        )?;
        Some(Self { token, repository })
    }
}

#[cfg(feature = "linear")]
struct LinearSmokeConfig {
    api_key: String,
    project_slug: String,
}

#[cfg(feature = "linear")]
impl LinearSmokeConfig {
    fn from_env(test_name: &str) -> Option<Self> {
        let api_key = required_env(test_name, "LINEAR_API_KEY", "export a valid Linear API key")?;
        let project_slug = required_env(
            test_name,
            "LINEAR_PROJECT_SLUG",
            "export an isolated Linear project slug for the smoke test",
        )?;
        Some(Self {
            api_key,
            project_slug,
        })
    }
}

#[cfg(feature = "linear")]
#[derive(Debug, Deserialize)]
struct LinearBootstrapResponse {
    data: Option<LinearBootstrapData>,
}

#[cfg(feature = "linear")]
#[derive(Debug, Deserialize)]
struct LinearBootstrapData {
    issues: LinearBootstrapIssues,
}

#[cfg(feature = "linear")]
#[derive(Debug, Deserialize)]
struct LinearBootstrapIssues {
    nodes: Vec<LinearBootstrapIssue>,
}

#[cfg(feature = "linear")]
#[derive(Debug, Deserialize)]
struct LinearBootstrapIssue {
    id: String,
    identifier: String,
    state: LinearBootstrapState,
}

#[cfg(feature = "linear")]
#[derive(Debug, Deserialize)]
struct LinearBootstrapState {
    name: String,
}

#[cfg(feature = "linear")]
async fn bootstrap_linear_issue(
    tracker: &LinearTracker,
    project_slug: &str,
) -> Result<LinearBootstrapIssue, CoreError> {
    let payload = tracker
        .execute_raw_graphql(
            r#"
query LinearSmokeTestBootstrap($projectSlug: String!) {
  issues(first: 1, filter: { project: { slugId: { eq: $projectSlug } } }) {
    nodes {
      id
      identifier
      state {
        name
      }
    }
  }
}
"#,
            json!({ "projectSlug": project_slug }),
        )
        .await?;
    let response: LinearBootstrapResponse = serde_json::from_value(payload)
        .map_err(|error| CoreError::Adapter(format!("linear_smoke_bootstrap: {error}")))?;
    response
        .data
        .and_then(|data| data.issues.nodes.into_iter().next())
        .ok_or_else(|| {
            CoreError::Adapter(format!(
                "linear_smoke_bootstrap: no issues found in project `{project_slug}`"
            ))
        })
}

#[cfg(feature = "github")]
#[tokio::test]
#[ignore = "real integration smoke test, run with `just test-integration`"]
async fn github_tracker_real_smoke_test() -> Result<(), CoreError> {
    let test_name = "github_tracker_real_smoke_test";
    let Some(config) = GithubSmokeConfig::from_env(test_name) else {
        return Ok(());
    };

    let tracker = GithubIssueTracker::new(
        config.repository.clone(),
        Some(config.token),
        None,
        None,
        None,
    )?;

    if let Some(status) = tracker.fetch_connection_status().await? {
        assert_ne!(
            status.state,
            TrackerConnectionState::Disconnected,
            "expected GitHub connection status to be usable for `{}` but got {:?}",
            config.repository,
            status
        );
    }

    let open_states = vec!["Todo".to_string()];
    let search_query = TrackerQuery {
        project_slug: None,
        repository: Some(config.repository.clone()),
        active_states: open_states.clone(),
        terminal_states: vec!["Done".to_string()],
    };

    let candidate_issues = tracker.fetch_candidate_issues(&search_query).await?;
    assert!(
        !candidate_issues.is_empty(),
        "expected at least one searchable open GitHub issue in `{}`",
        config.repository
    );

    let listed_issues = tracker.fetch_issues_by_states(None, &open_states).await?;
    assert!(
        !listed_issues.is_empty(),
        "expected at least one listed open GitHub issue in `{}`",
        config.repository
    );

    let list_issue = listed_issues
        .iter()
        .find(|issue| {
            candidate_issues
                .iter()
                .any(|candidate| candidate.id == issue.id)
        })
        .unwrap_or(&listed_issues[0]);
    let fetched_issues = tracker
        .fetch_issues_by_ids(std::slice::from_ref(&list_issue.id))
        .await?;

    assert_eq!(
        fetched_issues.len(),
        1,
        "expected exactly one fetched GitHub issue for id `{}`",
        list_issue.id
    );
    let fetched_issue = &fetched_issues[0];
    assert_eq!(fetched_issue.id, list_issue.id);
    assert_eq!(fetched_issue.identifier, list_issue.identifier);

    Ok(())
}

#[cfg(feature = "linear")]
#[tokio::test]
#[ignore = "real integration smoke test, run with `just test-integration`"]
async fn linear_tracker_real_smoke_test() -> Result<(), CoreError> {
    let test_name = "linear_tracker_real_smoke_test";
    let Some(config) = LinearSmokeConfig::from_env(test_name) else {
        return Ok(());
    };

    let tracker = LinearTracker::new(LINEAR_ENDPOINT.to_string(), config.api_key, None)?;
    let bootstrap_issue = bootstrap_linear_issue(&tracker, &config.project_slug).await?;
    let state_name = bootstrap_issue.state.name.clone();
    let states = vec![state_name.clone()];
    let search_query = TrackerQuery {
        project_slug: Some(config.project_slug.clone()),
        repository: None,
        active_states: states.clone(),
        terminal_states: Vec::new(),
    };

    let candidate_issues = tracker.fetch_candidate_issues(&search_query).await?;
    assert!(
        candidate_issues
            .iter()
            .any(|issue| issue.id == bootstrap_issue.id),
        "expected Linear search to include bootstrap issue `{}` in project `{}`",
        bootstrap_issue.identifier,
        config.project_slug
    );

    let listed_issues = tracker
        .fetch_issues_by_states(Some(&config.project_slug), &states)
        .await?;
    assert!(
        listed_issues
            .iter()
            .any(|issue| issue.id == bootstrap_issue.id),
        "expected Linear list to include bootstrap issue `{}` in project `{}`",
        bootstrap_issue.identifier,
        config.project_slug
    );

    let fetched_issues = tracker
        .fetch_issues_by_ids(std::slice::from_ref(&bootstrap_issue.id))
        .await?;
    assert_eq!(
        fetched_issues.len(),
        1,
        "expected exactly one fetched Linear issue for id `{}`",
        bootstrap_issue.id
    );
    let fetched_issue = &fetched_issues[0];
    assert_eq!(fetched_issue.id, bootstrap_issue.id);
    assert_eq!(fetched_issue.identifier, bootstrap_issue.identifier);
    assert_eq!(fetched_issue.state, state_name);

    Ok(())
}
