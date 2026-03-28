use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use polyphony_core::StateStore;
use serde::Serialize;

use crate::{Error, bootstrap_support::workflow_root_dir};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResetStateReport {
    pub backend: &'static str,
    pub workflow_root: PathBuf,
    pub removed_paths: Vec<PathBuf>,
}

pub(crate) fn state_store_path(workflow_path: &Path) -> Result<PathBuf, Error> {
    Ok(workflow_root_dir(workflow_path)?
        .join(".polyphony")
        .join("state.json"))
}

pub(crate) async fn build_store(
    workflow_path: &Path,
    sqlite_url: Option<&str>,
) -> Result<Option<Arc<dyn StateStore>>, Error> {
    #[cfg(feature = "sqlite")]
    if let Some(url) = sqlite_url {
        let store = polyphony_sqlite::SqliteStateStore::connect(url)
            .await
            .map_err(|error| Error::Config(error.to_string()))?;
        return Ok(Some(Arc::new(store)));
    }

    #[cfg(not(feature = "sqlite"))]
    if sqlite_url.is_some() {
        return Err(Error::Config(
            "sqlite support is disabled for this build".into(),
        ));
    }

    let state_path = state_store_path(workflow_path)?;
    Ok(Some(Arc::new(
        polyphony_core::file_store::JsonStateStore::new(state_path),
    )))
}

pub(crate) async fn reset_repository_state(
    workflow_path: &Path,
    sqlite_url: Option<&str>,
) -> Result<ResetStateReport, Error> {
    #[cfg(unix)]
    {
        let status = crate::daemon::request_status(workflow_path).await?;
        if status.running {
            let pid = status
                .pid
                .map(|value| format!(" (pid {value})"))
                .unwrap_or_default();
            return Err(Error::Config(format!(
                "daemon is running{pid}; stop it before resetting repository state"
            )));
        }
    }

    let workflow_root = workflow_root_dir(workflow_path)?;

    #[cfg(feature = "sqlite")]
    if let Some(url) = sqlite_url {
        let removed_paths = polyphony_sqlite::reset_database(url)
            .map_err(|error| Error::Config(error.to_string()))?;
        return Ok(ResetStateReport {
            backend: "sqlite",
            workflow_root,
            removed_paths,
        });
    }

    #[cfg(not(feature = "sqlite"))]
    if sqlite_url.is_some() {
        return Err(Error::Config(
            "sqlite support is disabled for this build".into(),
        ));
    }

    let state_path = state_store_path(workflow_path)?;
    let mut removed_paths = Vec::new();
    if remove_file_if_exists(&state_path)? {
        removed_paths.push(state_path);
    }

    Ok(ResetStateReport {
        backend: "json",
        workflow_root,
        removed_paths,
    })
}

fn remove_file_if_exists(path: &Path) -> Result<bool, Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Io(error)),
    }
}
