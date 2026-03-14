use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use polyphony_core::{
    BlockerRef, Error as CoreError, Issue, IssueAuthor, IssueStateUpdate, IssueTracker,
    TrackerQuery,
};
use serde::Deserialize;
use tokio::process::Command;
use tracing::debug;

/// A beads JSON record from `bd list --json`.
#[derive(Debug, Deserialize)]
struct BeadsIssue {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    dependency_count: u32,
}

/// Extended record from `bd show <id> --long --json`.
#[derive(Debug, Deserialize)]
struct BeadsIssueDetail {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    issue_type: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    dependencies: Vec<BeadsDependency>,
}

#[derive(Debug, Deserialize)]
struct BeadsDependency {
    id: String,
    #[allow(dead_code)]
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

pub struct BeadsTracker {
    repo_root: PathBuf,
}

impl BeadsTracker {
    pub fn new(repo_root: PathBuf) -> Result<Self, CoreError> {
        if !repo_root.join(".beads").is_dir() {
            return Err(CoreError::Adapter(format!(
                "no .beads/ directory found in {}",
                repo_root.display()
            )));
        }
        Ok(Self { repo_root })
    }

    async fn run_bd(&self, args: &[&str]) -> Result<String, CoreError> {
        let output = Command::new("bd")
            .args(args)
            .current_dir(&self.repo_root)
            .output()
            .await
            .map_err(|e| {
                CoreError::Adapter(format!(
                    "failed to run `bd {}`: {e} — is beads installed?",
                    args.join(" ")
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::Adapter(format!(
                "`bd {}` exited with {}: {stderr}",
                args.join(" "),
                output.status
            )));
        }

        String::from_utf8(output.stdout).map_err(|e| {
            CoreError::Adapter(format!("invalid UTF-8 in bd output: {e}"))
        })
    }

    async fn list_all(&self) -> Result<Vec<BeadsIssue>, CoreError> {
        let json = self.run_bd(&["list", "--json", "--all", "--limit", "0"]).await?;
        serde_json::from_str(&json).map_err(|e| {
            CoreError::Adapter(format!("failed to parse bd list JSON: {e}"))
        })
    }

    async fn show_issue(&self, id: &str) -> Result<Vec<BeadsIssueDetail>, CoreError> {
        let json = self.run_bd(&["show", id, "--long", "--json"]).await?;
        serde_json::from_str(&json).map_err(|e| {
            CoreError::Adapter(format!("failed to parse bd show JSON for {id}: {e}"))
        })
    }
}

fn normalize_status(status: &str) -> String {
    match status {
        "open" => "Open".to_string(),
        "in_progress" => "In Progress".to_string(),
        "blocked" => "Blocked".to_string(),
        "closed" => "Closed".to_string(),
        "deferred" => "Deferred".to_string(),
        other => {
            // Capitalize first letter for unknown statuses.
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    format!("{upper}{rest}", rest = chars.as_str())
                }
                None => String::new(),
            }
        }
    }
}

/// Strip the project prefix from a beads ID: `polyphony-oio.2` → `oio.2`.
fn shorten_beads_id(id: &str) -> String {
    id.split_once('-')
        .map_or_else(|| id.to_string(), |(_, rest)| rest.to_string())
}

fn beads_to_issue(b: &BeadsIssue) -> Issue {
    let state = normalize_status(&b.status);
    let mut labels = Vec::new();
    if let Some(ref t) = b.issue_type {
        labels.push(t.clone());
    }
    let identifier = shorten_beads_id(&b.id);
    Issue {
        id: b.id.clone(),
        identifier,
        title: b.title.clone(),
        description: b.description.clone(),
        priority: b.priority,
        state,
        branch_name: Some(format!("beads-{}", b.id)),
        url: None,
        author: Some(IssueAuthor {
            id: None,
            username: b.owner.clone(),
            display_name: b.created_by.clone(),
            role: None,
            trust_level: None,
            url: None,
        }),
        labels,
        comments: Vec::new(),
        blocked_by: Vec::new(),
        parent_id: b.parent.clone(),
        created_at: b.created_at,
        updated_at: b.updated_at,
    }
}

fn detail_to_issue(d: &BeadsIssueDetail) -> Issue {
    let state = normalize_status(&d.status);
    let mut labels = Vec::new();
    if let Some(ref t) = d.issue_type {
        labels.push(t.clone());
    }
    let blocked_by: Vec<BlockerRef> = d
        .dependencies
        .iter()
        .map(|dep| BlockerRef {
            id: Some(dep.id.clone()),
            identifier: Some(dep.id.clone()),
            state: dep.status.as_deref().map(normalize_status),
        })
        .collect();
    let identifier = shorten_beads_id(&d.id);
    Issue {
        id: d.id.clone(),
        identifier,
        title: d.title.clone(),
        description: d.description.clone(),
        priority: d.priority,
        state,
        branch_name: Some(format!("beads-{}", d.id)),
        url: None,
        author: Some(IssueAuthor {
            id: None,
            username: d.owner.clone(),
            display_name: d.created_by.clone(),
            role: None,
            trust_level: None,
            url: None,
        }),
        labels,
        comments: Vec::new(),
        blocked_by,
        parent_id: d.parent.clone(),
        created_at: d.created_at,
        updated_at: d.updated_at,
    }
}

#[async_trait]
impl IssueTracker for BeadsTracker {
    fn component_key(&self) -> String {
        "tracker:beads".to_string()
    }

    async fn fetch_candidate_issues(
        &self,
        query: &TrackerQuery,
    ) -> Result<Vec<Issue>, CoreError> {
        let all = self.list_all().await?;
        debug!(total = all.len(), "fetched beads issues");

        let issues: Vec<Issue> = all
            .iter()
            .filter(|b| {
                let state = normalize_status(&b.status);
                query.active_states.iter().any(|s| s.eq_ignore_ascii_case(&state))
            })
            .map(beads_to_issue)
            .collect();

        // For issues with dependencies, fetch detail to get blocker info.
        let mut result = Vec::with_capacity(issues.len());
        for issue in &issues {
            let orig = all.iter().find(|b| b.id == issue.id);
            if orig.is_some_and(|b| b.dependency_count > 0) {
                match self.show_issue(&issue.id).await {
                    Ok(details) if !details.is_empty() => {
                        result.push(detail_to_issue(&details[0]));
                    }
                    _ => result.push(issue.clone()),
                }
            } else {
                result.push(issue.clone());
            }
        }

        debug!(filtered = result.len(), "beads candidate issues");
        Ok(result)
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        let all = self.list_all().await?;
        Ok(all
            .iter()
            .filter(|b| {
                let state = normalize_status(&b.status);
                states.iter().any(|s| s.eq_ignore_ascii_case(&state))
            })
            .map(beads_to_issue)
            .collect())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        let mut issues = Vec::with_capacity(issue_ids.len());
        for id in issue_ids {
            match self.show_issue(id).await {
                Ok(details) if !details.is_empty() => {
                    issues.push(detail_to_issue(&details[0]));
                }
                Ok(_) => {
                    debug!(id, "beads show returned empty array, skipping");
                }
                Err(e) => {
                    debug!(id, error = %e, "failed to fetch beads issue, skipping");
                }
            }
        }
        Ok(issues)
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        let all = self.list_all().await?;
        Ok(all
            .iter()
            .filter(|b| issue_ids.contains(&b.id))
            .map(|b| IssueStateUpdate {
                id: b.id.clone(),
                identifier: b.id.clone(),
                state: normalize_status(&b.status),
                updated_at: b.updated_at,
            })
            .collect())
    }
}
