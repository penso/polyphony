use std::path::PathBuf;

use async_trait::async_trait;

use crate::{CachedSnapshot, Error, NetworkCache};

pub struct FileNetworkCache {
    path: PathBuf,
}

impl FileNetworkCache {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl NetworkCache for FileNetworkCache {
    async fn load(&self) -> Result<CachedSnapshot, Error> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let data = match std::fs::read_to_string(&path) {
                Ok(data) => data,
                Err(_) => return Ok(CachedSnapshot::default()),
            };
            Ok(serde_json::from_str(&data).unwrap_or_default())
        })
        .await
        .unwrap_or_else(|_| Ok(CachedSnapshot::default()))
    }

    async fn save(&self, snapshot: &CachedSnapshot) -> Result<(), Error> {
        let path = self.path.clone();
        let data =
            serde_json::to_string_pretty(snapshot).map_err(|e| Error::Store(e.to_string()))?;
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::Store(e.to_string()))?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &data).map_err(|e| Error::Store(e.to_string()))?;
            std::fs::rename(&tmp, &path).map_err(|e| Error::Store(e.to_string()))?;
            Ok(())
        })
        .await
        .unwrap_or_else(|e| Err(Error::Store(e.to_string())))
    }
}
