use std::path::{Path, PathBuf};

use chrono::Utc;
use polyphony_core::{
    RepoRegistration, RepoRegistry, TrackerKind, load_repo_registry, save_repo_registry,
};

use crate::Error;

/// Derive a `repo_id` from a git remote URL or local path.
///
/// For URLs like `https://github.com/owner/repo.git`, returns `owner/repo`.
/// For local paths, returns the directory name.
pub(crate) fn derive_repo_id(source: &str) -> String {
    if let Some(slug) = extract_slug_from_url(source) {
        return slug;
    }
    // Local path: use the directory name
    Path::new(source)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source)
        .to_string()
}

fn extract_slug_from_url(url: &str) -> Option<String> {
    // Handle https://github.com/owner/repo.git or https://github.com/owner/repo
    let url = url.strip_suffix(".git").unwrap_or(url);

    // Try splitting on github.com/, gitlab.com/, etc.
    for host in &["github.com/", "gitlab.com/", "bitbucket.org/"] {
        if let Some(rest) = url.split_once(host).map(|(_, rest)| rest) {
            let parts: Vec<&str> = rest.splitn(3, '/').collect();
            if parts.len() >= 2 {
                return Some(format!("{}/{}", parts[0], parts[1]));
            }
        }
    }

    // SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.split_once(':').map(|(_, rest)| rest)
        && url.contains('@')
    {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        return Some(rest.to_string());
    }

    None
}

/// Return the default path for the repo registry file.
pub(crate) fn default_registry_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".polyphony")
        .join("repos.json")
}

/// Return the default worktree path for a given repo_id.
fn default_worktree_path(repo_id: &str) -> PathBuf {
    let sanitized = repo_id.replace('/', "_");
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".polyphony")
        .join("repos")
        .join(sanitized)
        .join("main")
}

/// Detect the tracker kind from a URL or local git remote.
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

pub(crate) fn add_repo(source: &str, branch: Option<&str>) -> Result<RepoRegistration, Error> {
    let registry_path = default_registry_path();
    let mut registry = load_repo_registry(&registry_path)
        .map_err(|e| Error::Config(format!("loading repo registry: {e}")))?;

    let repo_id = derive_repo_id(source);
    if registry.contains(&repo_id) {
        return Err(Error::Config(format!(
            "repository '{}' is already registered",
            repo_id
        )));
    }

    let is_url = source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("git@")
        || source.starts_with("ssh://");

    let worktree_path;
    let clone_url;
    let tracker_kind;

    if is_url {
        worktree_path = default_worktree_path(&repo_id);
        clone_url = Some(source.to_string());
        tracker_kind = detect_tracker_kind(source);
    } else {
        // Local path
        let path = Path::new(source)
            .canonicalize()
            .map_err(|e| Error::Config(format!("cannot resolve path '{}': {e}", source)))?;
        if !path.join(".git").exists() && !path.join("WORKFLOW.md").exists() {
            return Err(Error::Config(format!(
                "'{}' does not appear to be a git repository",
                path.display()
            )));
        }
        worktree_path = path;
        clone_url = None;
        // Try to detect tracker from git remote
        tracker_kind = detect_tracker_kind_from_local(&worktree_path);
    }

    let default_branch = branch.unwrap_or("main").to_string();

    let registration = RepoRegistration {
        repo_id: repo_id.clone(),
        label: repo_id,
        worktree_path,
        clone_url,
        default_branch,
        tracker_kind,
        added_at: Utc::now(),
    };

    registry.add(registration.clone());
    save_repo_registry(&registry_path, &registry)
        .map_err(|e| Error::Config(format!("saving repo registry: {e}")))?;

    Ok(registration)
}

pub(crate) fn remove_repo(repo_id: &str) -> Result<RepoRegistration, Error> {
    let registry_path = default_registry_path();
    let mut registry = load_repo_registry(&registry_path)
        .map_err(|e| Error::Config(format!("loading repo registry: {e}")))?;

    let removed = registry
        .remove(repo_id)
        .ok_or_else(|| Error::Config(format!("repository '{}' is not registered", repo_id)))?;

    save_repo_registry(&registry_path, &registry)
        .map_err(|e| Error::Config(format!("saving repo registry: {e}")))?;

    Ok(removed)
}

pub(crate) fn list_repos() -> Result<RepoRegistry, Error> {
    let registry_path = default_registry_path();
    load_repo_registry(&registry_path)
        .map_err(|e| Error::Config(format!("loading repo registry: {e}")))
}

fn detect_tracker_kind_from_local(path: &Path) -> TrackerKind {
    // Try reading .git/config to detect remote URL
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn derive_repo_id_from_github_url() {
        assert_eq!(
            derive_repo_id("https://github.com/penso/polyphony.git"),
            "penso/polyphony"
        );
        assert_eq!(
            derive_repo_id("https://github.com/penso/polyphony"),
            "penso/polyphony"
        );
    }

    #[test]
    fn derive_repo_id_from_ssh_url() {
        assert_eq!(
            derive_repo_id("git@github.com:penso/polyphony.git"),
            "penso/polyphony"
        );
    }

    #[test]
    fn derive_repo_id_from_local_path() {
        assert_eq!(derive_repo_id("/home/user/code/polyphony"), "polyphony");
        assert_eq!(derive_repo_id("."), ".");
    }

    #[test]
    fn detect_tracker_from_url() {
        assert_eq!(
            detect_tracker_kind("https://github.com/owner/repo"),
            TrackerKind::Github
        );
        assert_eq!(
            detect_tracker_kind("https://gitlab.com/owner/repo"),
            TrackerKind::Gitlab
        );
        assert_eq!(
            detect_tracker_kind("https://example.com/repo"),
            TrackerKind::None
        );
    }
}
