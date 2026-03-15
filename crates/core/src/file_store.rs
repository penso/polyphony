use std::{collections::HashMap, path::PathBuf, sync::Arc};

use {
    async_trait::async_trait,
    serde::{Deserialize, Serialize},
    tokio::sync::Mutex,
};

use crate::{
    BudgetSnapshot, Error, Movement, PersistedRunRecord, ReviewedPullRequestHead, RuntimeSnapshot,
    StateStore, StoreBootstrap, Task,
};

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
    run_history: Vec<PersistedRunRecord>,
    #[serde(default)]
    movements: HashMap<String, Movement>,
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
            movements: data.movements,
            tasks: data.tasks,
            reviewed_pull_request_heads: data.reviewed_pull_request_heads,
            run_history: data.run_history,
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
        let snapshot = snapshot.clone();
        self.update_data(move |data| {
            data.snapshot = Some(snapshot.clone());
        })
        .await
    }

    async fn record_run(&self, run: &PersistedRunRecord) -> Result<(), Error> {
        let run = run.clone();
        self.update_data(move |data| {
            data.run_history.insert(0, run.clone());
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

    async fn save_movement(&self, movement: &Movement) -> Result<(), Error> {
        let movement = movement.clone();
        self.update_data(move |data| {
            data.movements.insert(movement.id.clone(), movement.clone());
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

    async fn load_movements(&self) -> Result<Vec<Movement>, Error> {
        Ok(self
            .load_data()
            .await?
            .movements
            .into_values()
            .collect::<Vec<_>>())
    }

    async fn load_tasks_for_movement(&self, movement_id: &str) -> Result<Vec<Task>, Error> {
        Ok(self
            .load_data()
            .await?
            .tasks
            .into_values()
            .filter(|task| task.movement_id == movement_id)
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
    serde_json::from_str(&data).map_err(|error| Error::Store(error.to_string()))
}

fn save_data_blocking(path: &PathBuf, data: &JsonStateStoreData) -> Result<(), Error> {
    let serialized =
        serde_json::to_string_pretty(data).map_err(|error| Error::Store(error.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::Store(error.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serialized).map_err(|error| Error::Store(error.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|error| Error::Store(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tempfile::tempdir;

    use crate::{
        AgentContextSnapshot, AgentHistoryRow, AgentModelCatalog, AttemptStatus, BudgetSnapshot,
        CodexTotals, DispatchMode, LoadingState, RuntimeCadence, RuntimeEvent, RuntimeSnapshot,
        SnapshotCounts, StateStore, TokenUsage, TrackerKind,
    };

    use super::JsonStateStore;

    #[tokio::test]
    async fn json_state_store_bootstraps_snapshot_and_runs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = JsonStateStore::new(path);
        let snapshot = RuntimeSnapshot {
            generated_at: chrono::Utc::now(),
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            visible_issues: Vec::new(),
            visible_triggers: Vec::new(),
            running: Vec::new(),
            agent_history: vec![AgentHistoryRow {
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
            movements: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::default(),
            tracker_kind: TrackerKind::None,
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
        };
        let run = crate::PersistedRunRecord {
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
        store.record_run(&run).await.unwrap();

        let bootstrap = store.bootstrap().await.unwrap();
        assert!(bootstrap.snapshot.is_some());
        assert_eq!(bootstrap.run_history.len(), 1);
        assert_eq!(bootstrap.budgets.len(), 1);
        assert_eq!(bootstrap.saved_contexts.len(), 1);
    }
}
