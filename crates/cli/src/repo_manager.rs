use std::path::PathBuf;

use polyphony_core::{
    RepoRegistration, RepoRegistry, build_repo_registration, default_repo_registry_path,
    load_repo_registry, save_repo_registry,
};

use crate::Error;

/// Return the default path for the repo registry file.
pub(crate) fn default_registry_path() -> PathBuf {
    default_repo_registry_path()
}

pub(crate) fn add_repo(source: &str, branch: Option<&str>) -> Result<RepoRegistration, Error> {
    let registry_path = default_registry_path();
    let mut registry = load_repo_registry(&registry_path)
        .map_err(|e| Error::Config(format!("loading repo registry: {e}")))?;

    let registration = build_repo_registration(source, branch)
        .map_err(|e| Error::Config(format!("building repo registration: {e}")))?;
    let repo_id = registration.repo_id.clone();
    if registry.contains(&repo_id) {
        return Err(Error::Config(format!(
            "repository '{}' is already registered",
            repo_id
        )));
    }

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn derive_repo_id_from_github_url() {
        assert_eq!(
            polyphony_core::derive_repo_id("https://github.com/penso/polyphony.git"),
            "penso/polyphony"
        );
        assert_eq!(
            polyphony_core::derive_repo_id("https://github.com/penso/polyphony"),
            "penso/polyphony"
        );
    }

    #[test]
    fn derive_repo_id_from_ssh_url() {
        assert_eq!(
            polyphony_core::derive_repo_id("git@github.com:penso/polyphony.git"),
            "penso/polyphony"
        );
    }

    #[test]
    fn derive_repo_id_from_local_path() {
        assert_eq!(
            polyphony_core::derive_repo_id("/home/user/code/polyphony"),
            "polyphony"
        );
        assert_eq!(polyphony_core::derive_repo_id("."), ".");
    }
}
