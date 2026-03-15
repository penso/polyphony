use crate::{prelude::*, *};

pub(crate) fn ensure_bootstrapped_workflow<F>(
    workflow_path: &Path,
    no_tui: bool,
    prompt_create_workflow: F,
) -> Result<WorkflowBootstrap, Error>
where
    F: FnOnce(&Path) -> Result<bool, Error>,
{
    if workflow_path.exists() {
        if workflow_path.is_file() {
            return Ok(WorkflowBootstrap::Ready);
        }
        return Err(Error::Config(format!(
            "workflow path `{}` exists but is not a file",
            workflow_path.display()
        )));
    }

    let should_create = if no_tui {
        true
    } else {
        prompt_create_workflow(workflow_path)?
    };
    if !should_create {
        return Ok(WorkflowBootstrap::Canceled);
    }

    if ensure_workflow_file(workflow_path)? {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            "created default workflow file"
        );
    }
    Ok(WorkflowBootstrap::Ready)
}

#[cfg(test)]
pub(crate) fn maybe_seed_repo_config_file(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<Option<PathBuf>, Error> {
    let repo_config_path = repo_config_path(workflow_path)?;
    if repo_config_path.exists() {
        if repo_config_path.is_file() {
            return Ok(Some(repo_config_path));
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            repo_config_path.display()
        )));
    }

    let workflow = load_workflow_with_user_config(workflow_path, user_config_path)?;
    let workflow_root = workflow_root_dir(workflow_path)?;
    if !should_seed_repo_config(&workflow.config, &workflow_root) {
        return Ok(None);
    }

    let source_repo_path = workflow_root
        .canonicalize()
        .unwrap_or_else(|_| workflow_root.clone());
    if ensure_repo_config_file(&repo_config_path, &source_repo_path)? {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            repo_config_path = %repo_config_path.display(),
            "created default repo-local config file"
        );
    }
    Ok(Some(repo_config_path))
}

/// Seed the repo config file, auto-detecting GitHub remotes.
///
/// Returns `(repo_config_path, first_run_no_github)`. When `first_run_no_github`
/// is `true`, no GitHub remote was found and a default config with `kind = "none"`
/// was written — the caller should exit with instructions.
pub(crate) fn maybe_seed_repo_config_with_github_detection(
    workflow_path: &Path,
    user_config_path: Option<&Path>,
) -> Result<(Option<PathBuf>, bool), Error> {
    let rcp = repo_config_path(workflow_path)?;
    if rcp.exists() {
        if rcp.is_file() {
            return Ok((Some(rcp), false));
        }
        return Err(Error::Config(format!(
            "repo config path `{}` exists but is not a file",
            rcp.display()
        )));
    }

    let workflow = load_workflow_with_user_config(workflow_path, user_config_path)?;
    let workflow_root = workflow_root_dir(workflow_path)?;
    if !should_seed_repo_config(&workflow.config, &workflow_root) {
        return Ok((None, false));
    }

    let source_repo_path = workflow_root
        .canonicalize()
        .unwrap_or_else(|_| workflow_root.clone());

    // Try beads first (local tracker, highest priority).
    if workflow_root.join(".beads").is_dir() {
        if polyphony_workflow::seed_repo_config_with_beads(&rcp, &source_repo_path)? {
            eprintln!("Detected beads issue tracker — tracker configured automatically.");
            tracing::info!(
                workflow_path = %workflow_path.display(),
                repo_config_path = %rcp.display(),
                "created repo-local config with beads tracker"
            );
        }
        return Ok((Some(rcp), false));
    }

    // Try to detect a GitHub remote and pre-configure the tracker.
    if let Some(github_repo) = polyphony_git::detect_github_remote(&workflow_root) {
        if seed_repo_config_with_github(&rcp, &source_repo_path, &github_repo)? {
            eprintln!(
                "Detected GitHub repository: {github_repo} — tracker configured automatically."
            );
            tracing::info!(
                workflow_path = %workflow_path.display(),
                repo_config_path = %rcp.display(),
                github_repo = %github_repo,
                "created repo-local config with GitHub tracker"
            );
        }
        return Ok((Some(rcp), false));
    }

    // Fallback: seed with kind = "none" and signal that the user should configure manually.
    if ensure_repo_config_file(&rcp, &source_repo_path)? {
        tracing::info!(
            workflow_path = %workflow_path.display(),
            repo_config_path = %rcp.display(),
            "created default repo-local config file (no GitHub remote detected)"
        );
    }
    Ok((Some(rcp), true))
}

fn should_seed_repo_config(config: &ServiceConfig, workflow_root: &Path) -> bool {
    workflow_root.join(".git").exists()
        && (config.tracker.kind == TrackerKind::None
            || (config.workspace.checkout_kind == CheckoutKind::Directory
                && config.workspace.source_repo_path.is_none()
                && config.workspace.clone_url.is_none()))
}

pub(crate) fn workflow_root_dir(workflow_path: &Path) -> Result<PathBuf, Error> {
    let parent = workflow_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    match parent {
        Some(parent) => Ok(parent.to_path_buf()),
        None => std::env::current_dir().map_err(Error::Io),
    }
}
