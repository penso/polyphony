use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use async_trait::async_trait;
use polyphony_core::{
    BudgetSnapshot, Error as CoreError, PersistedAgentRunRecord, ReviewedPullRequestHead, Run,
    RuntimeSnapshot, StateStore, StoreBootstrap, Task,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("sqlite migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("sqlite configuration error: {0}")]
    Configuration(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    pool: sqlx::SqlitePool,
}

impl SqliteStateStore {
    pub async fn connect(database_url: &str) -> Result<Self, Error> {
        let max_connections = if database_url.starts_with("sqlite::memory:") {
            1
        } else {
            5
        };
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;
        run_migrations(&pool).await?;
        let store = Self { pool };
        Ok(store)
    }
}

pub async fn run_migrations(pool: &sqlx::SqlitePool) -> Result<(), Error> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(Error::from)
}

pub fn reset_database(database_url: &str) -> Result<Vec<PathBuf>, Error> {
    let Some(path) = sqlite_database_path(database_url)? else {
        return Ok(Vec::new());
    };

    let mut removed_paths = Vec::new();
    for candidate in sqlite_sidecar_paths(&path) {
        match std::fs::remove_file(&candidate) {
            Ok(()) => removed_paths.push(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(Error::Io(error)),
        }
    }

    Ok(removed_paths)
}

fn sqlite_database_path(database_url: &str) -> Result<Option<PathBuf>, Error> {
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(database_url)
        .map_err(|error| Error::Configuration(error.to_string()))?;
    let filename = options.get_filename();
    if is_in_memory_database(database_url, filename) {
        return Ok(None);
    }
    Ok(Some(filename.to_path_buf()))
}

fn is_in_memory_database(database_url: &str, filename: &Path) -> bool {
    filename == Path::new(":memory:")
        || filename
            .to_string_lossy()
            .starts_with("file:sqlx-in-memory-")
        || database_url.trim_start().starts_with("sqlite::memory:")
        || database_url.contains("mode=memory")
}

fn sqlite_sidecar_paths(path: &Path) -> [PathBuf; 3] {
    let base = path.to_path_buf();
    [
        base.clone(),
        PathBuf::from(format!("{}-shm", base.display())),
        PathBuf::from(format!("{}-wal", base.display())),
    ]
}

#[async_trait]
impl StateStore for SqliteStateStore {
    async fn bootstrap(&self) -> Result<StoreBootstrap, CoreError> {
        let snapshot: Option<RuntimeSnapshot> = sqlx::query_as(
            "select payload from runtime_snapshots order by generated_at desc limit 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?
        .map(|(payload,): (String,)| {
            serde_json::from_str(&payload).map_err(|error| CoreError::Store(error.to_string()))
        })
        .transpose()?;
        let agent_run_history = self.load_agent_run_history().await?;
        let mut bootstrap = StoreBootstrap {
            snapshot: snapshot.clone(),
            retrying: Default::default(),
            throttles: Default::default(),
            budgets: Default::default(),
            saved_contexts: Default::default(),
            recent_events: Vec::new(),
            runs: Default::default(),
            tasks: Default::default(),
            reviewed_pull_request_heads: Default::default(),
            agent_run_history,
        };

        if let Some(snapshot) = snapshot {
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

        let runs = self.load_runs().await?;
        bootstrap.runs = runs.into_iter().map(|m| (m.id.clone(), m)).collect();

        for run_id in bootstrap.runs.keys().cloned().collect::<Vec<_>>() {
            let tasks = self.load_tasks_for_run(&run_id).await?;
            for task in tasks {
                bootstrap.tasks.insert(task.id.clone(), task);
            }
        }
        let reviewed_heads = self.load_reviewed_pull_request_heads().await?;
        bootstrap.reviewed_pull_request_heads = reviewed_heads
            .into_iter()
            .map(|head| (head.key.clone(), head))
            .collect();

        Ok(bootstrap)
    }

    async fn save_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(snapshot).map_err(|error| CoreError::Store(error.to_string()))?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query("delete from runtime_snapshots")
            .execute(&mut *transaction)
            .await
            .map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query("insert into runtime_snapshots (generated_at, payload) values (?1, ?2)")
            .bind(snapshot.generated_at.to_rfc3339())
            .bind(payload)
            .execute(&mut *transaction)
            .await
            .map_err(|error| CoreError::Store(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(())
    }

    async fn record_agent_run(&self, run: &PersistedAgentRunRecord) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(run).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            r#"
            insert into agent_run_records (
              issue_id,
              issue_identifier,
              session_id,
              status,
              attempt,
              started_at,
              finished_at,
              payload
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&run.issue_id)
        .bind(&run.issue_identifier)
        .bind(&run.session_id)
        .bind(run.status.to_string())
        .bind(run.attempt.map(i64::from))
        .bind(run.started_at.to_rfc3339())
        .bind(run.finished_at.map(|value| value.to_rfc3339()))
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(())
    }

    async fn record_budget(&self, snapshot: &BudgetSnapshot) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(snapshot).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            "insert into budget_snapshots (component, captured_at, payload) values (?1, ?2, ?3)",
        )
        .bind(&snapshot.component)
        .bind(snapshot.captured_at.to_rfc3339())
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(())
    }

    async fn save_run(&self, run: &Run) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(run).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            r#"
            insert or replace into runs (
              run_id, issue_id, status, created_at, updated_at, payload
            ) values (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(&run.id)
        .bind(&run.issue_id)
        .bind(run.status.to_string())
        .bind(run.created_at.to_rfc3339())
        .bind(run.updated_at.to_rfc3339())
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(())
    }

    async fn save_task(&self, task: &Task) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(task).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            r#"
            insert or replace into tasks (
              task_id, run_id, status, ordinal, created_at, updated_at, payload
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(&task.id)
        .bind(&task.run_id)
        .bind(task.status.to_string())
        .bind(task.ordinal)
        .bind(task.created_at.to_rfc3339())
        .bind(task.updated_at.to_rfc3339())
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(())
    }

    async fn load_runs(&self) -> Result<Vec<Run>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from runs
            order by created_at asc
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;

        rows.iter()
            .map(|(payload,)| {
                serde_json::from_str(payload).map_err(|error| CoreError::Store(error.to_string()))
            })
            .collect()
    }

    async fn load_tasks_for_run(&self, run_id: &str) -> Result<Vec<Task>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from tasks
            where run_id = ?1
            order by ordinal asc
            "#,
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;

        rows.iter()
            .map(|(payload,)| {
                serde_json::from_str(payload).map_err(|error| CoreError::Store(error.to_string()))
            })
            .collect()
    }

    async fn save_reviewed_pull_request_head(
        &self,
        head: &ReviewedPullRequestHead,
    ) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(head).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            r#"
            insert or replace into reviewed_pull_request_heads (
              review_key, reviewed_at, payload
            ) values (?1, ?2, ?3)
            "#,
        )
        .bind(&head.key)
        .bind(head.reviewed_at.to_rfc3339())
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(())
    }

    async fn load_reviewed_pull_request_heads(
        &self,
    ) -> Result<Vec<ReviewedPullRequestHead>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from reviewed_pull_request_heads
            order by reviewed_at asc
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;

        rows.iter()
            .map(|(payload,)| {
                serde_json::from_str(payload).map_err(|error| CoreError::Store(error.to_string()))
            })
            .collect()
    }
}

impl SqliteStateStore {
    async fn load_agent_run_history(&self) -> Result<Vec<PersistedAgentRunRecord>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from agent_run_records
            order by id desc
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| CoreError::Store(error.to_string()))?;

        rows.iter()
            .map(|(payload,)| {
                serde_json::from_str(payload).map_err(|error| CoreError::Store(error.to_string()))
            })
            .collect()
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use polyphony_core::{
        AttemptStatus, CodexTotals, DispatchMode, LoadingState, PersistedAgentRunRecord,
        ReviewProviderKind, ReviewTarget, ReviewedPullRequestHead, Run, RunKind, RunStatus,
        RuntimeCadence, RuntimeSnapshot, SnapshotCounts, StateStore, TokenUsage, TrackerKind,
    };
    use tempfile::tempdir;

    use super::{SqliteStateStore, reset_database, sqlite_database_path};

    #[tokio::test]
    async fn persists_reviewed_pull_request_heads() {
        let store = SqliteStateStore::connect("sqlite::memory:").await.unwrap();
        let reviewed = ReviewedPullRequestHead {
            key: "pr_review:github:penso/polyphony:42:abc123".into(),
            target: ReviewTarget {
                provider: ReviewProviderKind::Github,
                repository: "penso/polyphony".into(),
                number: 42,
                url: Some("https://github.com/penso/polyphony/pull/42".into()),
                base_branch: "main".into(),
                head_branch: "feature/review".into(),
                head_sha: "abc123".into(),
                checkout_ref: Some("refs/pull/42/head".into()),
            },
            reviewed_at: Utc::now(),
            run_id: Some("run-1".into()),
        };

        store
            .save_reviewed_pull_request_head(&reviewed)
            .await
            .unwrap();
        let loaded = store.load_reviewed_pull_request_heads().await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].key, reviewed.key);
        assert_eq!(loaded[0].target.repository, "penso/polyphony");
    }

    #[tokio::test]
    async fn bootstrap_restores_snapshot_run_history_and_terminal_runs() {
        let store = SqliteStateStore::connect("sqlite::memory:").await.unwrap();
        let snapshot = RuntimeSnapshot {
            repo_ids: Vec::new(), repo_registrations: Vec::new(),
            generated_at: Utc::now(),
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
            saved_contexts: Vec::new(),
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            runs: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::default(),
            tracker_kind: TrackerKind::Github,
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
        };
        let persisted_run = PersistedAgentRunRecord {
            repo_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "GH-1".into(),
            agent_name: "codex".into(),
            model: Some("gpt-5".into()),
            session_id: Some("sess-1".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 4,
            turn_count: 2,
            last_event: Some("Outcome".into()),
            last_message: Some("done".into()),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            workspace_path: None,
            error: None,
            saved_context: None,
        };
        let run = Run {
            id: "run-1".into(),
            kind: RunKind::IssueDelivery,
            issue_id: Some("issue-1".into()),
            issue_identifier: Some("GH-1".into()),
            title: "Deliver it".into(),
            status: RunStatus::Delivered,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: None,
            workspace_path: None,
            review_target: None,
            deliverable: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
        };

        store.save_snapshot(&snapshot).await.unwrap();
        store.record_agent_run(&persisted_run).await.unwrap();
        store.save_run(&run).await.unwrap();

        let bootstrap = store.bootstrap().await.unwrap();
        assert!(bootstrap.snapshot.is_some());
        assert_eq!(bootstrap.agent_run_history.len(), 1);
        assert_eq!(bootstrap.runs.len(), 1);
        assert_eq!(
            bootstrap
                .agent_run_history
                .first()
                .map(|entry| entry.agent_name.as_str()),
            Some("codex")
        );
        assert_eq!(
            bootstrap.runs.get("run-1").map(|entry| entry.status),
            Some(RunStatus::Delivered)
        );
    }

    #[test]
    fn sqlite_database_path_skips_in_memory_urls() {
        assert_eq!(sqlite_database_path("sqlite::memory:").unwrap(), None);
        assert_eq!(sqlite_database_path("sqlite://?mode=memory").unwrap(), None);
    }

    #[test]
    fn sqlite_database_path_returns_file_path() {
        assert_eq!(
            sqlite_database_path("sqlite://.polyphony/polyphony.db?mode=rwc").unwrap(),
            Some(PathBuf::from(".polyphony/polyphony.db"))
        );
    }

    #[tokio::test]
    async fn reset_database_removes_sqlite_file_and_sidecars() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("polyphony.db");
        let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
        let store = SqliteStateStore::connect(&database_url).await.unwrap();

        let snapshot = RuntimeSnapshot {
            repo_ids: Vec::new(), repo_registrations: Vec::new(),
            generated_at: Utc::now(),
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
            saved_contexts: Vec::new(),
            recent_events: Vec::new(),
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
        };

        store.save_snapshot(&snapshot).await.unwrap();
        drop(store);

        std::fs::write(format!("{}-wal", db_path.display()), "").unwrap();
        std::fs::write(format!("{}-shm", db_path.display()), "").unwrap();

        let removed = reset_database(&database_url).unwrap();
        assert_eq!(removed.len(), 3);
        assert!(!db_path.exists());
        assert!(!PathBuf::from(format!("{}-wal", db_path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", db_path.display())).exists());

        let reopened = SqliteStateStore::connect(&database_url).await.unwrap();
        let bootstrap = reopened.bootstrap().await.unwrap();
        assert!(bootstrap.snapshot.is_none());
        assert!(bootstrap.runs.is_empty());
        assert!(bootstrap.agent_run_history.is_empty());
    }
}
