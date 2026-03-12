#[cfg(feature = "sqlite")]
use async_trait::async_trait;
#[cfg(feature = "sqlite")]
use factoryrs_core::{
    BudgetSnapshot, Error as CoreError, PersistedRunRecord, RuntimeSnapshot, StateStore,
    StoreBootstrap,
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
        let Some((payload,)) = row else {
            return Ok(StoreBootstrap {
                retrying: Default::default(),
                throttles: Default::default(),
                budgets: Default::default(),
                recent_events: Vec::new(),
            });
        };
        let snapshot: RuntimeSnapshot =
            serde_json::from_str(&payload).map_err(|error| CoreError::Store(error.to_string()))?;
        Ok(StoreBootstrap {
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
            recent_events: snapshot.recent_events,
        })
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
        .bind(&run.status)
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
