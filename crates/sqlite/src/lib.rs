use async_trait::async_trait;
use polyphony_core::{
    BudgetSnapshot, Error as CoreError, Movement, PersistedRunRecord, ReviewedPullRequestHead,
    RuntimeSnapshot, StateStore, StoreBootstrap, Task,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("sqlite migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
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
        let run_history = self.load_run_history().await?;
        let mut bootstrap = StoreBootstrap {
            snapshot: snapshot.clone(),
            retrying: Default::default(),
            throttles: Default::default(),
            budgets: Default::default(),
            saved_contexts: Default::default(),
            recent_events: Vec::new(),
            movements: Default::default(),
            tasks: Default::default(),
            reviewed_pull_request_heads: Default::default(),
            run_history,
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

        let movements = self.load_movements().await?;
        bootstrap.movements = movements.into_iter().map(|m| (m.id.clone(), m)).collect();

        for movement_id in bootstrap.movements.keys().cloned().collect::<Vec<_>>() {
            let tasks = self.load_tasks_for_movement(&movement_id).await?;
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

    async fn record_run(&self, run: &PersistedRunRecord) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(run).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            r#"
            insert into run_records (
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

    async fn save_movement(&self, movement: &Movement) -> Result<(), CoreError> {
        let payload =
            serde_json::to_string(movement).map_err(|error| CoreError::Store(error.to_string()))?;
        sqlx::query(
            r#"
            insert or replace into movements (
              movement_id, issue_id, status, created_at, updated_at, payload
            ) values (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(&movement.id)
        .bind(&movement.issue_id)
        .bind(movement.status.to_string())
        .bind(movement.created_at.to_rfc3339())
        .bind(movement.updated_at.to_rfc3339())
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
              task_id, movement_id, status, ordinal, created_at, updated_at, payload
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(&task.id)
        .bind(&task.movement_id)
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

    async fn load_movements(&self) -> Result<Vec<Movement>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from movements
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

    async fn load_tasks_for_movement(&self, movement_id: &str) -> Result<Vec<Task>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from tasks
            where movement_id = ?1
            order by ordinal asc
            "#,
        )
        .bind(movement_id)
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
    async fn load_run_history(&self) -> Result<Vec<PersistedRunRecord>, CoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            select payload from run_records
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
    use chrono::Utc;
    use polyphony_core::{
        AttemptStatus, CodexTotals, DispatchMode, LoadingState, Movement, MovementKind,
        MovementStatus, PersistedRunRecord, ReviewProviderKind, ReviewTarget,
        ReviewedPullRequestHead, RuntimeCadence, RuntimeSnapshot, SnapshotCounts, StateStore,
        TokenUsage, TrackerKind,
    };

    use super::SqliteStateStore;

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
            movement_id: Some("mov-1".into()),
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
    async fn bootstrap_restores_snapshot_run_history_and_terminal_movements() {
        let store = SqliteStateStore::connect("sqlite::memory:").await.unwrap();
        let snapshot = RuntimeSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            visible_issues: Vec::new(),
            visible_triggers: Vec::new(),
            approved_issue_keys: Vec::new(),
            running: Vec::new(),
            agent_history: Vec::new(),
            retrying: Vec::new(),
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: Vec::new(),
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            movements: Vec::new(),
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
        let run = PersistedRunRecord {
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
        let movement = Movement {
            id: "mov-1".into(),
            kind: MovementKind::IssueDelivery,
            issue_id: Some("issue-1".into()),
            issue_identifier: Some("GH-1".into()),
            title: "Deliver it".into(),
            status: MovementStatus::Delivered,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: None,
            workspace_path: None,
            review_target: None,
            deliverable: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        store.save_snapshot(&snapshot).await.unwrap();
        store.record_run(&run).await.unwrap();
        store.save_movement(&movement).await.unwrap();

        let bootstrap = store.bootstrap().await.unwrap();
        assert!(bootstrap.snapshot.is_some());
        assert_eq!(bootstrap.run_history.len(), 1);
        assert_eq!(bootstrap.movements.len(), 1);
        assert_eq!(
            bootstrap
                .run_history
                .first()
                .map(|entry| entry.agent_name.as_str()),
            Some("codex")
        );
        assert_eq!(
            bootstrap.movements.get("mov-1").map(|entry| entry.status),
            Some(MovementStatus::Delivered)
        );
    }
}
