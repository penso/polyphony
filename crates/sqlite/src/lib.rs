#[cfg(feature = "sqlite")]
use async_trait::async_trait;
#[cfg(feature = "sqlite")]
use polyphony_core::{
    BudgetSnapshot, Error as CoreError, Movement, PersistedRunRecord, ReviewedPullRequestHead,
    RuntimeSnapshot, StateStore, StoreBootstrap, Task,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[cfg(feature = "sqlite")]
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("sqlite feature is disabled")]
    Disabled,
}

#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteStateStore {
    pub async fn connect(database_url: &str) -> Result<Self, Error> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), Error> {
        sqlx::query(
            r#"
            create table if not exists runtime_snapshots (
              id integer primary key autoincrement,
              generated_at text not null,
              payload text not null
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            create table if not exists run_records (
              id integer primary key autoincrement,
              issue_id text not null,
              issue_identifier text not null,
              session_id text,
              status text not null,
              attempt integer,
              started_at text not null,
              finished_at text,
              payload text not null
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            create table if not exists budget_snapshots (
              id integer primary key autoincrement,
              component text not null,
              captured_at text not null,
              payload text not null
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            create table if not exists movements (
              id integer primary key autoincrement,
              movement_id text not null unique,
              issue_id text,
              status text not null,
              created_at text not null,
              updated_at text not null,
              payload text not null
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            create table if not exists tasks (
              id integer primary key autoincrement,
              task_id text not null unique,
              movement_id text not null,
              status text not null,
              ordinal integer not null,
              created_at text not null,
              updated_at text not null,
              payload text not null
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            create table if not exists reviewed_pull_request_heads (
              id integer primary key autoincrement,
              review_key text not null unique,
              reviewed_at text not null,
              payload text not null
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl StateStore for SqliteStateStore {
    async fn bootstrap(&self) -> Result<StoreBootstrap, CoreError> {
        let row: Option<(String,)> =
            sqlx::query_as("select payload from runtime_snapshots order by id desc limit 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| CoreError::Store(error.to_string()))?;
        let mut bootstrap = match row {
            None => StoreBootstrap {
                retrying: Default::default(),
                throttles: Default::default(),
                budgets: Default::default(),
                saved_contexts: Default::default(),
                recent_events: Vec::new(),
                movements: Default::default(),
                tasks: Default::default(),
                reviewed_pull_request_heads: Default::default(),
            },
            Some((payload,)) => {
                let snapshot: RuntimeSnapshot = serde_json::from_str(&payload)
                    .map_err(|error| CoreError::Store(error.to_string()))?;
                StoreBootstrap {
                    retrying: snapshot
                        .retrying
                        .into_iter()
                        .map(|row| (row.issue_id.clone(), row))
                        .collect(),
                    throttles: snapshot
                        .throttles
                        .into_iter()
                        .map(|window| (window.component.clone(), window))
                        .collect(),
                    budgets: snapshot
                        .budgets
                        .into_iter()
                        .map(|budget| (budget.component.clone(), budget))
                        .collect(),
                    saved_contexts: snapshot
                        .saved_contexts
                        .into_iter()
                        .map(|context| (context.issue_id.clone(), context))
                        .collect(),
                    recent_events: snapshot.recent_events,
                    movements: Default::default(),
                    tasks: Default::default(),
                    reviewed_pull_request_heads: Default::default(),
                }
            },
        };

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
        sqlx::query("insert into runtime_snapshots (generated_at, payload) values (?1, ?2)")
            .bind(snapshot.generated_at.to_rfc3339())
            .bind(payload)
            .execute(&self.pool)
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
            where status not in ('delivered', 'failed', 'cancelled')
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

#[cfg(not(feature = "sqlite"))]
#[derive(Debug, Clone, Default)]
pub struct SqliteStateStore;

#[cfg(not(feature = "sqlite"))]
impl SqliteStateStore {
    pub async fn connect(_database_url: &str) -> Result<Self, Error> {
        Err(Error::Disabled)
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::SqliteStateStore,
        chrono::Utc,
        polyphony_core::{ReviewProviderKind, ReviewTarget, ReviewedPullRequestHead, StateStore},
    };

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
}
