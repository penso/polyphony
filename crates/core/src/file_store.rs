use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    BudgetSnapshot, Error, PersistedAgentRunRecord, ReviewedPullRequestHead, Run, RuntimeSnapshot,
    StateStore, StoreBootstrap, Task,
};

const MAX_RECENT_EVENTS: usize = 256;
const MAX_RECENT_EVENT_MESSAGE_CHARS: usize = 512;
const MAX_RUN_HISTORY: usize = 256;
const MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES: usize = 0;
const MAX_SAVED_CONTEXT_MESSAGE_CHARS: usize = 2_048;

#[derive(Debug, Clone)]
pub struct JsonStateStore {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl JsonStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    async fn load_data(&self) -> Result<JsonStateStoreData, Error> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || load_data_blocking(&path))
            .await
            .unwrap_or_else(|error| Err(Error::Store(error.to_string())))
    }

    async fn update_data(
        &self,
        update: impl FnOnce(&mut JsonStateStoreData) + Send + 'static,
    ) -> Result<(), Error> {
        let _guard = self.write_lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut data = load_data_blocking(&path)?;
            update(&mut data);
            save_data_blocking(&path, &data)
        })
        .await
        .unwrap_or_else(|error| Err(Error::Store(error.to_string())))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct JsonStateStoreData {
    #[serde(default)]
    snapshot: Option<RuntimeSnapshot>,
    #[serde(default)]
    agent_run_history: Vec<PersistedAgentRunRecord>,
    #[serde(default)]
    runs: HashMap<String, Run>,
    #[serde(default)]
    tasks: HashMap<String, Task>,
    #[serde(default)]
    reviewed_pull_request_heads: HashMap<String, ReviewedPullRequestHead>,
}

#[async_trait]
impl StateStore for JsonStateStore {
    async fn bootstrap(&self) -> Result<StoreBootstrap, Error> {
        let data = self.load_data().await?;
        let mut bootstrap = StoreBootstrap {
            snapshot: data.snapshot.clone(),
            retrying: HashMap::new(),
            throttles: HashMap::new(),
            budgets: HashMap::new(),
            saved_contexts: HashMap::new(),
            recent_events: Vec::new(),
            runs: data.runs,
            tasks: data.tasks,
            reviewed_pull_request_heads: data.reviewed_pull_request_heads,
            agent_run_history: data.agent_run_history,
        };

        if let Some(snapshot) = data.snapshot {
            bootstrap.retrying = snapshot
                .retrying
                .into_iter()
                .map(|row| (row.issue_id.clone(), row))
                .collect();
            bootstrap.throttles = snapshot
                .throttles
                .into_iter()
                .map(|window| (window.component.clone(), window))
                .collect();
            bootstrap.budgets = snapshot
                .budgets
                .into_iter()
                .map(|budget| (budget.component.clone(), budget))
                .collect();
            bootstrap.saved_contexts = snapshot
                .saved_contexts
                .into_iter()
                .map(|context| (context.issue_id.clone(), context))
                .collect();
            bootstrap.recent_events = snapshot.recent_events;
        }

        Ok(bootstrap)
    }

    async fn save_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), Error> {
        let snapshot = compact_snapshot_for_store(snapshot.clone());
        self.update_data(move |data| {
            data.snapshot = Some(snapshot.clone());
        })
        .await
    }

    async fn record_agent_run(&self, run: &PersistedAgentRunRecord) -> Result<(), Error> {
        let run = run.clone();
        self.update_data(move |data| {
            data.agent_run_history.insert(0, run.clone());
        })
        .await
    }

    async fn record_budget(&self, snapshot: &BudgetSnapshot) -> Result<(), Error> {
        let snapshot = snapshot.clone();
        self.update_data(move |data| {
            if let Some(existing) = &mut data.snapshot {
                existing
                    .budgets
                    .retain(|budget| budget.component != snapshot.component);
                existing.budgets.push(snapshot.clone());
            }
        })
        .await
    }

    async fn save_run(&self, run: &Run) -> Result<(), Error> {
        let run = run.clone();
        self.update_data(move |data| {
            data.runs.insert(run.id.clone(), run.clone());
        })
        .await
    }

    async fn save_task(&self, task: &Task) -> Result<(), Error> {
        let task = task.clone();
        self.update_data(move |data| {
            data.tasks.insert(task.id.clone(), task.clone());
        })
        .await
    }

    async fn load_runs(&self) -> Result<Vec<Run>, Error> {
        Ok(self
            .load_data()
            .await?
            .runs
            .into_values()
            .collect::<Vec<_>>())
    }

    async fn load_tasks_for_run(&self, run_id: &str) -> Result<Vec<Task>, Error> {
        Ok(self
            .load_data()
            .await?
            .tasks
            .into_values()
            .filter(|task| task.run_id == run_id)
            .collect::<Vec<_>>())
    }

    async fn save_reviewed_pull_request_head(
        &self,
        head: &ReviewedPullRequestHead,
    ) -> Result<(), Error> {
        let head = head.clone();
        self.update_data(move |data| {
            data.reviewed_pull_request_heads
                .insert(head.key.clone(), head.clone());
        })
        .await
    }

    async fn load_reviewed_pull_request_heads(
        &self,
    ) -> Result<Vec<ReviewedPullRequestHead>, Error> {
        Ok(self
            .load_data()
            .await?
            .reviewed_pull_request_heads
            .into_values()
            .collect::<Vec<_>>())
    }
}

fn load_data_blocking(path: &PathBuf) -> Result<JsonStateStoreData, Error> {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JsonStateStoreData::default());
        },
        Err(error) => return Err(Error::Store(error.to_string())),
    };
    let mut data: JsonStateStoreData =
        serde_json::from_str(&data).map_err(|error| Error::Store(error.to_string()))?;
    compact_state_store_data(&mut data);
    Ok(data)
}

fn compact_snapshot_for_store(mut snapshot: RuntimeSnapshot) -> RuntimeSnapshot {
    snapshot.running.clear();
    snapshot.agent_run_history.clear();
    snapshot.runs.clear();
    snapshot.tasks.clear();
    snapshot.recent_events = compact_recent_events(snapshot.recent_events);
    snapshot.saved_contexts = snapshot
        .saved_contexts
        .into_iter()
        .map(|context| compact_saved_context(context, MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES))
        .collect();
    snapshot.pending_user_interactions.clear();
    snapshot.loading = crate::LoadingState::default();
    snapshot.from_cache = false;
    snapshot.cached_at = None;
    snapshot
}

fn save_data_blocking(path: &PathBuf, data: &JsonStateStoreData) -> Result<(), Error> {
    let mut data = data.clone();
    compact_state_store_data(&mut data);
    let serialized =
        serde_json::to_string_pretty(&data).map_err(|error| Error::Store(error.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::Store(error.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serialized).map_err(|error| Error::Store(error.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|error| Error::Store(error.to_string()))?;
    Ok(())
}

fn compact_state_store_data(data: &mut JsonStateStoreData) {
    if let Some(snapshot) = data.snapshot.take() {
        data.snapshot = Some(compact_snapshot_for_store(snapshot));
    }
    data.agent_run_history = data
        .agent_run_history
        .drain(..)
        .take(MAX_RUN_HISTORY)
        .map(compact_persisted_agent_run_record)
        .collect();
}

fn compact_persisted_agent_run_record(mut run: PersistedAgentRunRecord) -> PersistedAgentRunRecord {
    run.last_message = run
        .last_message
        .as_deref()
        .map(|message| truncate_chars(message, MAX_RECENT_EVENT_MESSAGE_CHARS));
    run.error = run
        .error
        .as_deref()
        .map(|message| truncate_chars(message, MAX_SAVED_CONTEXT_MESSAGE_CHARS));
    run.saved_context = None;
    run
}

fn compact_saved_context(
    mut context: crate::AgentContextSnapshot,
    max_entries: usize,
) -> crate::AgentContextSnapshot {
    for entry in &mut context.transcript {
        entry.message = truncate_chars(&entry.message, MAX_SAVED_CONTEXT_MESSAGE_CHARS);
    }
    if context.transcript.len() > max_entries {
        let drain = context.transcript.len().saturating_sub(max_entries);
        context.transcript.drain(..drain);
    }
    context
}

fn compact_recent_events(events: Vec<crate::RuntimeEvent>) -> Vec<crate::RuntimeEvent> {
    events
        .into_iter()
        .take(MAX_RECENT_EVENTS)
        .map(|mut event| {
            event.message = truncate_chars(&event.message, MAX_RECENT_EVENT_MESSAGE_CHARS);
            event
        })
        .collect()
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;

    use tempfile::tempdir;

    use super::JsonStateStore;
    use crate::{
        AgentContextSnapshot, AgentModelCatalog, AgentRunHistoryRow, AttemptStatus, BudgetSnapshot,
        CodexTotals, DispatchMode, LoadingState, RuntimeCadence, RuntimeEvent, RuntimeSnapshot,
        SnapshotCounts, StateStore, TokenUsage, TrackerKind,
    };

    #[tokio::test]
    async fn json_state_store_bootstraps_snapshot_and_runs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = JsonStateStore::new(path);
        let snapshot = RuntimeSnapshot {
            repo_ids: Vec::new(),
            repo_registrations: Vec::new(),
            generated_at: chrono::Utc::now(),
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            tracker_issues: Vec::new(),
            inbox_items: Vec::new(),
            approved_inbox_keys: Vec::new(),
            running: Vec::new(),
            agent_run_history: vec![AgentRunHistoryRow {
                repo_id: String::new(),
                issue_id: "1".into(),
                issue_identifier: "ISSUE-1".into(),
                agent_name: "codex".into(),
                model: Some("gpt-5".into()),
                status: AttemptStatus::Succeeded,
                attempt: Some(1),
                max_turns: 4,
                turn_count: 2,
                session_id: Some("sess".into()),
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                last_event: Some("completed".into()),
                last_message: Some("done".into()),
                started_at: chrono::Utc::now(),
                finished_at: Some(chrono::Utc::now()),
                last_event_at: Some(chrono::Utc::now()),
                tokens: TokenUsage::default(),
                workspace_path: None,
                error: None,
                saved_context: None,
            }],
            retrying: Vec::new(),
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: vec![BudgetSnapshot {
                component: "agent:codex".into(),
                captured_at: chrono::Utc::now(),
                credits_remaining: Some(10.0),
                credits_total: Some(20.0),
                spent_usd: None,
                soft_limit_usd: None,
                hard_limit_usd: None,
                reset_at: None,
                raw: None,
            }],
            agent_catalogs: vec![AgentModelCatalog::default()],
            saved_contexts: vec![AgentContextSnapshot {
                repo_id: String::new(),
                issue_id: "1".into(),
                issue_identifier: "ISSUE-1".into(),
                updated_at: chrono::Utc::now(),
                agent_name: "codex".into(),
                model: None,
                session_id: None,
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                status: Some(AttemptStatus::Succeeded),
                error: None,
                usage: TokenUsage::default(),
                transcript: Vec::new(),
            }],
            recent_events: vec![RuntimeEvent {
                at: chrono::Utc::now(),
                scope: crate::EventScope::Worker,
                message: "done".into(),
            }],
            pending_user_interactions: Vec::new(),
            runs: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::default(),
            tracker_kind: TrackerKind::None,
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
            heartbeat: crate::HeartbeatStatus::default(),
        };
        let run = crate::PersistedAgentRunRecord {
            repo_id: String::new(),
            issue_id: "1".into(),
            issue_identifier: "ISSUE-1".into(),
            agent_name: "codex".into(),
            model: Some("gpt-5".into()),
            session_id: Some("sess".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 4,
            turn_count: 2,
            last_event: Some("completed".into()),
            last_message: Some("done".into()),
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            last_event_at: Some(chrono::Utc::now()),
            tokens: TokenUsage::default(),
            workspace_path: None,
            error: None,
            saved_context: None,
        };

        store.save_snapshot(&snapshot).await.unwrap();
        store.record_agent_run(&run).await.unwrap();

        let stored = super::load_data_blocking(&dir.path().join("state.json")).unwrap();
        let stored_snapshot = stored.snapshot.unwrap();
        assert!(stored_snapshot.running.is_empty());
        assert!(stored_snapshot.agent_run_history.is_empty());
        assert!(stored_snapshot.runs.is_empty());
        assert!(stored_snapshot.tasks.is_empty());

        let bootstrap = store.bootstrap().await.unwrap();
        assert!(bootstrap.snapshot.is_some());
        assert_eq!(bootstrap.agent_run_history.len(), 1);
        assert_eq!(bootstrap.budgets.len(), 1);
        assert_eq!(bootstrap.saved_contexts.len(), 1);
    }

    #[test]
    fn load_data_compacts_oversized_snapshot_and_run_history() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let now = chrono::Utc::now();
        let context = AgentContextSnapshot {
            repo_id: String::new(),
            issue_id: "1".into(),
            issue_identifier: "ISSUE-1".into(),
            updated_at: now,
            agent_name: "router".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Succeeded),
            error: None,
            usage: TokenUsage::default(),
            transcript: (0..40)
                .map(|index| crate::AgentContextEntry {
                    at: now,
                    kind: crate::AgentEventKind::Notification,
                    message: format!("{index}-{}", "x".repeat(3_000)),
                })
                .collect(),
        };
        let data = super::JsonStateStoreData {
            snapshot: Some(RuntimeSnapshot {
                repo_ids: Vec::new(),
                repo_registrations: Vec::new(),
                generated_at: now,
                counts: SnapshotCounts::default(),
                cadence: RuntimeCadence::default(),
                tracker_issues: Vec::new(),
                inbox_items: Vec::new(),
                approved_inbox_keys: Vec::new(),
                running: Vec::new(),
                agent_run_history: Vec::new(),
                retrying: Vec::new(),
                codex_totals: CodexTotals::default(),
                rate_limits: None,
                throttles: Vec::new(),
                budgets: Vec::new(),
                agent_catalogs: Vec::new(),
                saved_contexts: vec![context.clone()],
                recent_events: (0..300)
                    .map(|index| RuntimeEvent {
                        at: now,
                        scope: crate::EventScope::Agent,
                        message: format!("{index}-{}", "y".repeat(1_000)),
                    })
                    .collect(),
                pending_user_interactions: Vec::new(),
                runs: Vec::new(),
                tasks: Vec::new(),
                loading: LoadingState::default(),
                dispatch_mode: DispatchMode::default(),
                tracker_kind: TrackerKind::None,
                tracker_connection: None,
                from_cache: false,
                cached_at: None,
                agent_profile_names: Vec::new(),
                agent_profiles: Vec::new(),
                heartbeat: crate::HeartbeatStatus::default(),
            }),
            agent_run_history: (0..300)
                .map(|index| crate::PersistedAgentRunRecord {
                    repo_id: String::new(),
                    issue_id: index.to_string(),
                    issue_identifier: format!("ISSUE-{index}"),
                    agent_name: "codex".into(),
                    model: None,
                    session_id: None,
                    thread_id: None,
                    turn_id: None,
                    codex_app_server_pid: None,
                    status: AttemptStatus::Succeeded,
                    attempt: Some(1),
                    max_turns: 1,
                    turn_count: 1,
                    last_event: None,
                    last_message: Some("z".repeat(1_000)),
                    started_at: now,
                    finished_at: Some(now),
                    last_event_at: Some(now),
                    tokens: TokenUsage::default(),
                    workspace_path: None,
                    error: None,
                    saved_context: Some(context.clone()),
                })
                .collect(),
            runs: HashMap::new(),
            tasks: HashMap::new(),
            reviewed_pull_request_heads: HashMap::new(),
        };

        super::save_data_blocking(&path, &data).unwrap();
        let loaded = super::load_data_blocking(&path).unwrap();

        assert_eq!(loaded.agent_run_history.len(), super::MAX_RUN_HISTORY);
        assert_eq!(
            loaded.snapshot.as_ref().unwrap().recent_events.len(),
            super::MAX_RECENT_EVENTS
        );
        assert_eq!(
            loaded.snapshot.as_ref().unwrap().saved_contexts[0]
                .transcript
                .len(),
            super::MAX_SAVED_CONTEXT_TRANSCRIPT_ENTRIES
        );
        assert!(loaded.agent_run_history[0].saved_context.is_none());
    }
}
