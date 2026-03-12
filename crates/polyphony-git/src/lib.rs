use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use polyphony_core::{
    CheckoutKind, Error as CoreError, Workspace, WorkspaceProvisioner, WorkspaceRequest,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("missing repository source for workspace provisioning")]
    MissingRepositorySource,
    #[error("workspace path exists and is not a directory: {0}")]
    WorkspacePathCollision(String),
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
    if let Ok(metadata) = fs::symlink_metadata(&request.workspace_path) {
        if !metadata.is_dir() {
            return Err(CoreError::Adapter(
                Error::WorkspacePathCollision(request.workspace_path.display().to_string())
                    .to_string(),
            ));
        }
        sync_existing_workspace(&request)?;
        return Ok(Workspace {
            path: request.workspace_path,
            workspace_key: request.workspace_key,
            created_now: false,
            branch_name: request.branch_name,
        });
    }

    let create_result = match request.checkout_kind {
        CheckoutKind::Directory => fs::create_dir_all(&request.workspace_path).map_err(map_io),
        CheckoutKind::LinkedWorktree => add_linked_worktree(&request),
        CheckoutKind::DiscreteClone => create_discrete_clone(&request),
    };
    if let Err(error) = create_result {
        cleanup_partial_workspace(&request.workspace_path);
        return Err(error);
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

fn sync_existing_workspace(request: &WorkspaceRequest) -> Result<(), CoreError> {
    if !request.sync_on_reuse {
        return Ok(());
    }
    match request.checkout_kind {
        CheckoutKind::Directory => Ok(()),
        CheckoutKind::LinkedWorktree => {
            if let Some(source_repo_path) = request.source_repo_path.as_deref() {
                let source_repo = git2::Repository::open(source_repo_path).map_err(|error| {
                    CoreError::Adapter(format!("open source repo failed: {error}"))
                })?;
                fetch_origin(&source_repo)?;
            }
            sync_existing_repo_checkout(
                &request.workspace_path,
                request.branch_name.as_deref(),
                request.default_branch.as_deref(),
            )
        }
        CheckoutKind::DiscreteClone => {
            let repo = git2::Repository::open(&request.workspace_path).map_err(|error| {
                CoreError::Adapter(format!("open existing clone failed: {error}"))
            })?;
            fetch_origin(&repo)?;
            sync_existing_repo_checkout(
                &request.workspace_path,
                request.branch_name.as_deref(),
                request.default_branch.as_deref(),
            )
        }
    }
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

fn sync_existing_repo_checkout(
    repo_path: &Path,
    branch_name: Option<&str>,
    default_branch: Option<&str>,
) -> Result<(), CoreError> {
    let repo = git2::Repository::open(repo_path)
        .map_err(|error| CoreError::Adapter(format!("open workspace repo failed: {error}")))?;
    if let Some(branch_name) = branch_name {
        checkout_branch(&repo, branch_name, default_branch)?;
    }
    Ok(())
}

fn fetch_origin(repo: &git2::Repository) -> Result<(), CoreError> {
    let Ok(mut remote) = repo.find_remote("origin") else {
        return Ok(());
    };
    remote
        .fetch(&["refs/heads/*:refs/remotes/origin/*"], None, None)
        .map_err(|error| CoreError::Adapter(format!("fetch origin failed: {error}")))?;
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
        let remote_ref_name = format!("refs/remotes/origin/{default_branch}");
        if let Ok(reference) = repo.find_reference(&remote_ref_name) {
            return reference.peel_to_commit().map_err(|error| {
                CoreError::Adapter(format!("resolve remote default branch failed: {error}"))
            });
        }
    }
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| CoreError::Adapter(format!("resolve HEAD failed: {error}")))
}

fn cleanup_partial_workspace(path: &Path) {
    if path.exists() {
        let _ = fs::remove_dir_all(path);
    }
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use polyphony_core::{CheckoutKind, WorkspaceProvisioner, WorkspaceRequest};
    use tempfile::tempdir;

    use super::GitWorkspaceProvisioner;

    fn make_request(
        root: &Path,
        workspace_name: &str,
        checkout_kind: CheckoutKind,
        source_repo_path: Option<PathBuf>,
    ) -> WorkspaceRequest {
        WorkspaceRequest {
            issue_identifier: workspace_name.to_string(),
            workspace_root: root.to_path_buf(),
            workspace_path: root.join(workspace_name),
            workspace_key: workspace_name.to_string(),
            branch_name: Some(format!("task/{workspace_name}")),
            checkout_kind,
            sync_on_reuse: true,
            source_repo_path,
            clone_url: None,
            default_branch: Some("main".into()),
        }
    }

    fn init_repo(path: &Path) -> git2::Repository {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(path, &opts).unwrap();
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "initial");
        repo
    }

    fn commit_all(repo: &git2::Repository, message: &str) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("polyphony", "polyphony@example.com").unwrap();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .map(|commit| vec![commit])
            .unwrap_or_default();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap();
    }

    fn current_branch(repo: &git2::Repository) -> String {
        repo.head().unwrap().shorthand().unwrap().to_string()
    }

    #[tokio::test]
    async fn linked_worktree_lifecycle_works() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        init_repo(&source_path);
        let root = temp.path().join("workspaces");
        let provisioner = GitWorkspaceProvisioner;
        let request = make_request(
            &root,
            "FAC-101",
            CheckoutKind::LinkedWorktree,
            Some(source_path.clone()),
        );

        let workspace = provisioner.ensure_workspace(request.clone()).await.unwrap();
        assert!(workspace.created_now);
        assert!(workspace.path.exists());
        assert_eq!(
            current_branch(&git2::Repository::open(&workspace.path).unwrap()),
            "task/FAC-101"
        );

        provisioner.cleanup_workspace(request).await.unwrap();
        assert!(!root.join("FAC-101").exists());
    }

    #[tokio::test]
    async fn existing_non_directory_path_is_rejected() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        init_repo(&source_path);
        let root = temp.path().join("workspaces");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("FAC-102"), "collision").unwrap();
        let provisioner = GitWorkspaceProvisioner;
        let request = make_request(
            &root,
            "FAC-102",
            CheckoutKind::DiscreteClone,
            Some(source_path),
        );

        let error = provisioner.ensure_workspace(request).await.unwrap_err();
        assert!(error.to_string().contains("not a directory"));
    }

    #[tokio::test]
    async fn existing_clone_rechecks_out_requested_branch() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        init_repo(&source_path);
        let root = temp.path().join("workspaces");
        let provisioner = GitWorkspaceProvisioner;
        let request = make_request(
            &root,
            "FAC-103",
            CheckoutKind::DiscreteClone,
            Some(source_path),
        );

        let workspace = provisioner.ensure_workspace(request.clone()).await.unwrap();
        let repo = git2::Repository::open(&workspace.path).unwrap();
        let main_ref = repo.find_reference("refs/heads/main").unwrap();
        let main_name = main_ref.name().unwrap().to_string();
        repo.set_head(&main_name).unwrap();
        repo.checkout_head(None).unwrap();
        assert_eq!(current_branch(&repo), "main");

        let workspace = provisioner.ensure_workspace(request).await.unwrap();
        let repo = git2::Repository::open(&workspace.path).unwrap();
        assert!(!workspace.created_now);
        assert_eq!(current_branch(&repo), "task/FAC-103");
    }

    #[tokio::test]
    async fn existing_clone_preserves_branch_when_sync_on_reuse_is_disabled() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        init_repo(&source_path);
        let root = temp.path().join("workspaces");
        let provisioner = GitWorkspaceProvisioner;
        let mut request = make_request(
            &root,
            "FAC-104",
            CheckoutKind::DiscreteClone,
            Some(source_path),
        );

        let workspace = provisioner.ensure_workspace(request.clone()).await.unwrap();
        let repo = git2::Repository::open(&workspace.path).unwrap();
        let main_ref = repo.find_reference("refs/heads/main").unwrap();
        let main_name = main_ref.name().unwrap().to_string();
        repo.set_head(&main_name).unwrap();
        repo.checkout_head(None).unwrap();
        assert_eq!(current_branch(&repo), "main");

        request.sync_on_reuse = false;
        let workspace = provisioner.ensure_workspace(request).await.unwrap();
        let repo = git2::Repository::open(&workspace.path).unwrap();
        assert!(!workspace.created_now);
        assert_eq!(current_branch(&repo), "main");
    }
}
