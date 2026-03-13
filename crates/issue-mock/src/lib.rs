use std::{collections::HashMap, sync::Arc, time::Duration};

use {
    async_trait::async_trait,
    chrono::Utc,
    polyphony_core::{
        AgentDefinition, AgentEvent, AgentEventKind, AgentRunResult, AgentRunSpec, AgentRuntime,
        AttemptStatus, BudgetSnapshot, Error as CoreError, Issue, IssueAuthor, IssueComment,
        IssueStateUpdate, IssueTracker, TokenUsage, TrackerQuery,
    },
    thiserror::Error,
    tokio::sync::{RwLock, mpsc},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("mock issue adapter error: {0}")]
    Mock(String),
}

#[cfg(feature = "mock")]
#[derive(Debug, Clone)]
pub struct MockTracker {
    issues: Arc<RwLock<HashMap<String, Issue>>>,
}

#[cfg(feature = "mock")]
impl MockTracker {
    pub fn seeded_demo() -> Self {
        let issues = [
            demo_issue("FAC-101", "Build workflow loader", Some(1), "Todo"),
            demo_issue(
                "FAC-102",
                "Add orchestrator retries",
                Some(1),
                "In Progress",
            ),
            demo_issue("FAC-103", "Stream live task view", Some(2), "Todo"),
        ]
        .into_iter()
        .map(|issue| (issue.id.clone(), issue))
        .collect();
        Self {
            issues: Arc::new(RwLock::new(issues)),
        }
    }

    pub async fn set_state(&self, issue_id: &str, state: &str) {
        if let Some(issue) = self.issues.write().await.get_mut(issue_id) {
            issue.state = state.to_string();
            issue.updated_at = Some(Utc::now());
        }
    }
}

#[cfg(feature = "mock")]
#[async_trait]
impl IssueTracker for MockTracker {
    fn component_key(&self) -> String {
        "tracker:mock".into()
    }

    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, CoreError> {
        let active = query
            .active_states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let issues = self.issues.read().await;
        Ok(issues
            .values()
            .filter(|issue| active.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, CoreError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let states = states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let issues = self.issues.read().await;
        Ok(issues
            .values()
            .filter(|issue| states.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>, CoreError> {
        let issues = self.issues.read().await;
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, CoreError> {
        let issues = self.issues.read().await;
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .map(|issue| IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }

    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, CoreError> {
        Ok(Some(BudgetSnapshot {
            component: self.component_key(),
            captured_at: Utc::now(),
            credits_remaining: None,
            credits_total: None,
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: None,
        }))
    }
}

#[cfg(feature = "mock")]
#[derive(Debug, Clone)]
pub struct MockAgentRuntime {
    tracker: MockTracker,
}

#[cfg(feature = "mock")]
impl MockAgentRuntime {
    pub fn new(tracker: MockTracker) -> Self {
        Self { tracker }
    }
}

#[cfg(feature = "mock")]
#[async_trait]
impl AgentRuntime for MockAgentRuntime {
    fn component_key(&self) -> String {
        "provider:mock".into()
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, CoreError> {
        let session_id = format!(
            "{}-attempt-{}",
            spec.issue.identifier,
            spec.attempt.unwrap_or(0)
        );
        let _ = event_tx.send(AgentEvent {
            issue_id: spec.issue.id.clone(),
            issue_identifier: spec.issue.identifier.clone(),
            agent_name: spec.agent.name.clone(),
            session_id: Some(session_id.clone()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            kind: AgentEventKind::SessionStarted,
            at: Utc::now(),
            message: Some("session started".into()),
            usage: None,
            rate_limits: None,
            raw: None,
        });

        for turn in 1..=spec.max_turns.min(3) {
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                agent_name: spec.agent.name.clone(),
                session_id: Some(session_id.clone()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                kind: AgentEventKind::TurnStarted,
                at: Utc::now(),
                message: Some(format!("turn {turn} started")),
                usage: None,
                rate_limits: None,
                raw: None,
            });
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                agent_name: spec.agent.name.clone(),
                session_id: Some(session_id.clone()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                kind: AgentEventKind::Notification,
                at: Utc::now(),
                message: Some(format!("processing {}", spec.issue.title)),
                usage: None,
                rate_limits: None,
                raw: None,
            });
            tokio::time::sleep(Duration::from_millis(400)).await;
            let usage = TokenUsage {
                input_tokens: 600 * u64::from(turn),
                output_tokens: 280 * u64::from(turn),
                total_tokens: 880 * u64::from(turn),
            };
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                agent_name: spec.agent.name.clone(),
                session_id: Some(session_id.clone()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                kind: AgentEventKind::UsageUpdated,
                at: Utc::now(),
                message: Some(format!("turn {turn} usage updated")),
                usage: Some(usage),
                rate_limits: None,
                raw: None,
            });
            let _ = event_tx.send(AgentEvent {
                issue_id: spec.issue.id.clone(),
                issue_identifier: spec.issue.identifier.clone(),
                agent_name: spec.agent.name.clone(),
                session_id: Some(session_id.clone()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                kind: AgentEventKind::TurnCompleted,
                at: Utc::now(),
                message: Some(format!("turn {turn} completed")),
                usage: None,
                rate_limits: None,
                raw: None,
            });
        }

        self.tracker.set_state(&spec.issue.id, "Human Review").await;
        Ok(AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: spec.max_turns.min(3),
            error: None,
            final_issue_state: Some("Human Review".into()),
        })
    }

    async fn fetch_budgets(
        &self,
        agents: &[AgentDefinition],
    ) -> Result<Vec<BudgetSnapshot>, CoreError> {
        let names = if agents.is_empty() {
            vec!["mock".to_string()]
        } else {
            agents.iter().map(|agent| agent.name.clone()).collect()
        };
        Ok(names
            .into_iter()
            .map(|name| BudgetSnapshot {
                component: format!("agent:{name}"),
                captured_at: Utc::now(),
                credits_remaining: Some(100.0),
                credits_total: Some(100.0),
                spent_usd: Some(0.0),
                soft_limit_usd: Some(10.0),
                hard_limit_usd: Some(25.0),
                reset_at: None,
                raw: None,
            })
            .collect())
    }
}

#[cfg(feature = "mock")]
fn demo_issue(identifier: &str, title: &str, priority: Option<i32>, state: &str) -> Issue {
    Issue {
        id: identifier.to_string(),
        identifier: identifier.to_string(),
        title: title.to_string(),
        description: Some(format!("Implement {title}")),
        priority,
        state: state.to_string(),
        branch_name: Some(format!("feat/{}", identifier.to_ascii_lowercase())),
        url: None,
        author: Some(IssueAuthor {
            id: Some("mock-user".into()),
            username: Some("factory-bot".into()),
            display_name: Some("Factory Bot".into()),
            role: Some("member".into()),
            trust_level: Some("internal_member".into()),
            url: None,
        }),
        labels: vec!["automation".into(), "rust".into()],
        comments: vec![IssueComment {
            id: format!("{identifier}-comment-1"),
            body: "Please include tests and note any follow-up risks.".into(),
            author: Some(IssueAuthor {
                id: Some("mock-reviewer".into()),
                username: Some("maintainer".into()),
                display_name: Some("Maintainer".into()),
                role: Some("admin".into()),
                trust_level: Some("internal_admin".into()),
                url: None,
            }),
            url: None,
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }],
        blocked_by: Vec::new(),
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    }
}
