use std::path::Path;

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

/// Return the default path for the repo registry file.
pub fn default_repo_registry_path() -> PathBuf {
    if let Some(path) = std::env::var_os("POLYPHONY_REPO_REGISTRY_PATH") {
        return PathBuf::from(path);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".polyphony")
        .join("repos.json")
}

/// Derive a `repo_id` from a git remote URL or local path.
pub fn derive_repo_id(source: &str) -> String {
    if let Some(slug) = extract_slug_from_url(source) {
        return slug;
    }
    Path::new(source)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source)
        .to_string()
}

/// Build a normalized repo registration from a remote URL or local path.
pub fn build_repo_registration(
    source: &str,
    branch: Option<&str>,
) -> Result<RepoRegistration, std::io::Error> {
    let repo_id = derive_repo_id(source);
    let is_url = source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("git@")
        || source.starts_with("ssh://");

    let (worktree_path, clone_url, tracker_kind) = if is_url {
        (
            default_managed_worktree_path(&repo_id),
            Some(source.to_string()),
            detect_tracker_kind(source),
        )
    } else {
        let path = Path::new(source).canonicalize().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("cannot resolve path '{source}': {error}"),
            )
        })?;
        if !path.join(".git").exists() && !path.join("WORKFLOW.md").exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "'{}' does not appear to be a git repository",
                    path.display()
                ),
            ));
        }
        (
            path.clone(),
            None,
            detect_tracker_kind_from_local_path(&path),
        )
    };

    Ok(RepoRegistration {
        repo_id: repo_id.clone(),
        label: repo_id,
        worktree_path,
        clone_url,
        default_branch: branch.unwrap_or("main").to_string(),
        tracker_kind,
        added_at: Utc::now(),
    })
}

fn extract_slug_from_url(url: &str) -> Option<String> {
    let url = url.strip_suffix(".git").unwrap_or(url);

    for host in ["github.com/", "gitlab.com/", "bitbucket.org/"] {
        if let Some(rest) = url.split_once(host).map(|(_, rest)| rest) {
            let parts: Vec<&str> = rest.splitn(3, '/').collect();
            if parts.len() >= 2 {
                return Some(format!("{}/{}", parts[0], parts[1]));
            }
        }
    }

    if let Some(rest) = url.split_once(':').map(|(_, rest)| rest)
        && url.contains('@')
    {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        return Some(rest.to_string());
    }

    None
}

fn default_managed_worktree_path(repo_id: &str) -> PathBuf {
    let sanitized = repo_id.replace('/', "_");
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".polyphony")
        .join("repos")
        .join(sanitized)
        .join("main")
}

fn detect_tracker_kind(source: &str) -> TrackerKind {
    let lower = source.to_ascii_lowercase();
    if lower.contains("github.com") {
        TrackerKind::Github
    } else if lower.contains("gitlab.com") || lower.contains("gitlab") {
        TrackerKind::Gitlab
    } else {
        TrackerKind::None
    }
}

fn detect_tracker_kind_from_local_path(path: &Path) -> TrackerKind {
    let git_config_path = path.join(".git").join("config");
    if let Ok(contents) = std::fs::read_to_string(git_config_path) {
        if contents.contains("github.com") {
            return TrackerKind::Github;
        }
        if contents.contains("gitlab.com") || contents.contains("gitlab") {
            return TrackerKind::Gitlab;
        }
    }
    TrackerKind::None
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
