use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use factoryrs_core::{
    CheckoutKind, Error as CoreError, Workspace, WorkspaceProvisioner, WorkspaceRequest,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("missing repository source for workspace provisioning")]
    MissingRepositorySource,
}

#[derive(Debug, Default)]
pub struct GitWorkspaceProvisioner;

#[async_trait]
impl WorkspaceProvisioner for GitWorkspaceProvisioner {
    fn component_key(&self) -> String {
        "workspace:git".into()
    }

    async fn ensure_workspace(&self, request: WorkspaceRequest) -> Result<Workspace, CoreError> {
        tokio_wrap(move || ensure_workspace_sync(request)).await
    }

    async fn cleanup_workspace(&self, request: WorkspaceRequest) -> Result<(), CoreError> {
        tokio_wrap(move || cleanup_workspace_sync(request)).await
    }
}

fn ensure_workspace_sync(request: WorkspaceRequest) -> Result<Workspace, CoreError> {
    fs::create_dir_all(&request.workspace_root).map_err(map_io)?;
    if request.workspace_path.exists() {
        return Ok(Workspace {
            path: request.workspace_path,
            workspace_key: request.workspace_key,
            created_now: false,
            branch_name: request.branch_name,
        });
    }

    match request.checkout_kind {
        CheckoutKind::Directory => {
            fs::create_dir_all(&request.workspace_path).map_err(map_io)?;
        }
        CheckoutKind::LinkedWorktree => add_linked_worktree(&request)?,
        CheckoutKind::DiscreteClone => create_discrete_clone(&request)?,
    }

    Ok(Workspace {
        path: request.workspace_path,
        workspace_key: request.workspace_key,
        created_now: true,
        branch_name: request.branch_name,
    })
}

fn cleanup_workspace_sync(request: WorkspaceRequest) -> Result<(), CoreError> {
    match request.checkout_kind {
        CheckoutKind::LinkedWorktree => remove_linked_worktree(&request)?,
        CheckoutKind::DiscreteClone | CheckoutKind::Directory => {
            if request.workspace_path.exists() {
                fs::remove_dir_all(&request.workspace_path).map_err(map_io)?;
            }
        }
    }
    Ok(())
}

fn add_linked_worktree(request: &WorkspaceRequest) -> Result<(), CoreError> {
    let source_path = request
        .source_repo_path
        .as_deref()
        .ok_or_else(|| CoreError::Adapter(Error::MissingRepositorySource.to_string()))?;
    let repo = git2::Repository::open(source_path)
        .map_err(|error| CoreError::Adapter(format!("open source repo failed: {error}")))?;
    let branch_name = request
        .branch_name
        .clone()
        .unwrap_or_else(|| request.workspace_key.clone());
    let branch_ref = ensure_branch(&repo, &branch_name, request.default_branch.as_deref())?;
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&branch_ref));
    repo.worktree(&request.workspace_key, &request.workspace_path, Some(&opts))
        .map_err(|error| CoreError::Adapter(format!("add linked worktree failed: {error}")))?;
    Ok(())
}

fn create_discrete_clone(request: &WorkspaceRequest) -> Result<(), CoreError> {
    let source = request
        .clone_url
        .clone()
        .or_else(|| {
            request
                .source_repo_path
                .as_ref()
                .map(|path| path.display().to_string())
        })
        .ok_or_else(|| CoreError::Adapter(Error::MissingRepositorySource.to_string()))?;
    let repo = git2::Repository::clone(&source, &request.workspace_path)
        .map_err(|error| CoreError::Adapter(format!("clone failed: {error}")))?;
    if let Some(branch_name) = &request.branch_name {
        checkout_branch(&repo, branch_name, request.default_branch.as_deref())?;
    }
    Ok(())
}

fn remove_linked_worktree(request: &WorkspaceRequest) -> Result<(), CoreError> {
    let source_path = request
        .source_repo_path
        .as_deref()
        .ok_or_else(|| CoreError::Adapter(Error::MissingRepositorySource.to_string()))?;
    let repo = git2::Repository::open(source_path)
        .map_err(|error| CoreError::Adapter(format!("open source repo failed: {error}")))?;
    let worktrees = repo
        .worktrees()
        .map_err(|error| CoreError::Adapter(format!("list worktrees failed: {error}")))?;
    let target = canonicalize_if_possible(&request.workspace_path);
    for entry in &worktrees {
        let Some(name) = entry else {
            continue;
        };
        let wt = repo
            .find_worktree(name)
            .map_err(|error| CoreError::Adapter(format!("find worktree failed: {error}")))?;
        if canonicalize_if_possible(wt.path()) == target {
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).working_tree(true).locked(true);
            wt.prune(Some(&mut opts))
                .map_err(|error| CoreError::Adapter(format!("prune worktree failed: {error}")))?;
            break;
        }
    }
    if request.workspace_path.exists() {
        fs::remove_dir_all(&request.workspace_path).map_err(map_io)?;
    }
    Ok(())
}

fn ensure_branch<'a>(
    repo: &'a git2::Repository,
    branch_name: &str,
    default_branch: Option<&str>,
) -> Result<git2::Reference<'a>, CoreError> {
    if let Ok(branch) = repo.find_branch(branch_name, git2::BranchType::Local) {
        return branch
            .into_reference()
            .resolve()
            .map_err(|error| CoreError::Adapter(format!("resolve branch failed: {error}")));
    }
    let commit = resolve_base_commit(repo, default_branch)?;
    repo.branch(branch_name, &commit, false)
        .map_err(|error| CoreError::Adapter(format!("create branch failed: {error}")))?
        .into_reference()
        .resolve()
        .map_err(|error| CoreError::Adapter(format!("resolve branch failed: {error}")))
}

fn checkout_branch(
    repo: &git2::Repository,
    branch_name: &str,
    default_branch: Option<&str>,
) -> Result<(), CoreError> {
    let reference = ensure_branch(repo, branch_name, default_branch)?;
    let ref_name = reference
        .name()
        .ok_or_else(|| CoreError::Adapter("branch reference name missing".into()))?;
    repo.set_head(ref_name)
        .map_err(|error| CoreError::Adapter(format!("set head failed: {error}")))?;
    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::new()
            .safe()
            .allow_conflicts(true),
    ))
    .map_err(|error| CoreError::Adapter(format!("checkout head failed: {error}")))?;
    Ok(())
}

fn resolve_base_commit<'a>(
    repo: &'a git2::Repository,
    default_branch: Option<&str>,
) -> Result<git2::Commit<'a>, CoreError> {
    if let Some(default_branch) = default_branch {
        let ref_name = format!("refs/heads/{default_branch}");
        if let Ok(reference) = repo.find_reference(&ref_name) {
            return reference.peel_to_commit().map_err(|error| {
                CoreError::Adapter(format!("resolve default branch failed: {error}"))
            });
        }
    }
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| CoreError::Adapter(format!("resolve HEAD failed: {error}")))
}

fn canonicalize_if_possible(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn map_io(error: std::io::Error) -> CoreError {
    CoreError::Adapter(error.to_string())
}

async fn tokio_wrap<T, F>(func: F) -> Result<T, CoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(func)
        .await
        .map_err(|error| CoreError::Adapter(error.to_string()))?
}
