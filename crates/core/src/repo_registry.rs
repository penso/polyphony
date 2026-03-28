use crate::{prelude::*, *};

/// A registered repository managed by the Polyphony daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRegistration {
    pub repo_id: RepoId,
    pub label: String,
    pub worktree_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_url: Option<String>,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    pub tracker_kind: TrackerKind,
    pub added_at: DateTime<Utc>,
}

fn default_branch() -> String {
    "main".to_string()
}

/// Persistent registry of all repos managed by a single daemon instance.
/// Serialized to `~/.polyphony/repos.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoRegistry {
    pub repos: Vec<RepoRegistration>,
}

impl RepoRegistry {
    pub fn find(&self, repo_id: &str) -> Option<&RepoRegistration> {
        self.repos.iter().find(|r| r.repo_id == repo_id)
    }

    pub fn contains(&self, repo_id: &str) -> bool {
        self.repos.iter().any(|r| r.repo_id == repo_id)
    }

    pub fn add(&mut self, registration: RepoRegistration) {
        if !self.contains(&registration.repo_id) {
            self.repos.push(registration);
        }
    }

    pub fn remove(&mut self, repo_id: &str) -> Option<RepoRegistration> {
        if let Some(pos) = self.repos.iter().position(|r| r.repo_id == repo_id) {
            Some(self.repos.remove(pos))
        } else {
            None
        }
    }

    pub fn repo_ids(&self) -> Vec<String> {
        self.repos.iter().map(|r| r.repo_id.clone()).collect()
    }
}

/// Load the registry from the standard location.
pub fn load_repo_registry(path: &std::path::Path) -> Result<RepoRegistry, std::io::Error> {
    if !path.exists() {
        return Ok(RepoRegistry::default());
    }
    let contents = std::fs::read_to_string(path)?;
    serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Save the registry to the standard location.
pub fn save_repo_registry(
    path: &std::path::Path,
    registry: &RepoRegistry,
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(registry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, contents)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_repo() {
        let mut registry = RepoRegistry::default();
        let reg = RepoRegistration {
            repo_id: "owner/repo".into(),
            label: "owner/repo".into(),
            worktree_path: PathBuf::from("/tmp/test"),
            clone_url: Some("https://github.com/owner/repo.git".into()),
            default_branch: "main".into(),
            tracker_kind: TrackerKind::Github,
            added_at: Utc::now(),
        };
        registry.add(reg.clone());
        assert!(registry.contains("owner/repo"));
        assert_eq!(registry.repos.len(), 1);

        // Duplicate add is a no-op
        registry.add(reg);
        assert_eq!(registry.repos.len(), 1);

        let removed = registry.remove("owner/repo");
        assert!(removed.is_some());
        assert!(!registry.contains("owner/repo"));
    }

    #[test]
    fn round_trip_json() {
        let mut registry = RepoRegistry::default();
        registry.add(RepoRegistration {
            repo_id: "test/repo".into(),
            label: "test/repo".into(),
            worktree_path: PathBuf::from("/home/user/.polyphony/repos/test_repo/main"),
            clone_url: None,
            default_branch: "main".into(),
            tracker_kind: TrackerKind::None,
            added_at: Utc::now(),
        });
        let json = serde_json::to_string(&registry).unwrap();
        let loaded: RepoRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.repos.len(), 1);
        assert_eq!(loaded.repos[0].repo_id, "test/repo");
    }
}
