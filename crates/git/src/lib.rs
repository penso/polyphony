use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use {
    async_trait::async_trait,
    polyphony_core::{
        CheckoutKind, Error as CoreError, Workspace, WorkspaceCommitRequest, WorkspaceCommitResult,
        WorkspaceCommitter, WorkspaceProvisioner, WorkspaceRequest,
    },
    thiserror::Error,
    tracing::{debug, info, warn},
};

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

#[derive(Debug, Default)]
pub struct GitWorkspaceCommitter;

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

#[async_trait]
impl WorkspaceCommitter for GitWorkspaceCommitter {
    fn component_key(&self) -> String {
        "workspace:git-commit".into()
    }

    async fn commit_and_push(
        &self,
        request: &WorkspaceCommitRequest,
    ) -> Result<Option<WorkspaceCommitResult>, CoreError> {
        let request = request.clone();
        tokio_wrap(move || commit_and_push_sync(&request)).await
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
        debug!(
            workspace_path = %request.workspace_path.display(),
            checkout_kind = ?request.checkout_kind,
            sync_on_reuse = request.sync_on_reuse,
            "reusing existing workspace"
        );
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

    info!(
        workspace_path = %request.workspace_path.display(),
        checkout_kind = ?request.checkout_kind,
        branch_name = ?request.branch_name,
        "created workspace"
    );
    Ok(Workspace {
        path: request.workspace_path,
        workspace_key: request.workspace_key,
        created_now: true,
        branch_name: request.branch_name,
    })
}

fn cleanup_workspace_sync(request: WorkspaceRequest) -> Result<(), CoreError> {
    info!(
        workspace_path = %request.workspace_path.display(),
        checkout_kind = ?request.checkout_kind,
        "removing workspace"
    );
    match request.checkout_kind {
        CheckoutKind::LinkedWorktree => remove_linked_worktree(&request)?,
        CheckoutKind::DiscreteClone | CheckoutKind::Directory => {
            if request.workspace_path.exists() {
                fs::remove_dir_all(&request.workspace_path).map_err(map_io)?;
            }
        },
    }
    Ok(())
}

fn commit_and_push_sync(
    request: &WorkspaceCommitRequest,
) -> Result<Option<WorkspaceCommitResult>, CoreError> {
    let repo = git2::Repository::open(&request.workspace_path)
        .map_err(|error| CoreError::Adapter(format!("open workspace repo failed: {error}")))?;
    checkout_branch(&repo, &request.branch_name, None)?;

    let statuses = repo
        .statuses(Some(
            git2::StatusOptions::new()
                .include_untracked(true)
                .recurse_untracked_dirs(true)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true)
                .include_ignored(false),
        ))
        .map_err(|error| CoreError::Adapter(format!("git status failed: {error}")))?;
    let changed_files = statuses.iter().count();
    if changed_files > 0 {
        let mut index = repo
            .index()
            .map_err(|error| CoreError::Adapter(format!("open git index failed: {error}")))?;
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .map_err(|error| CoreError::Adapter(format!("stage changes failed: {error}")))?;
        let tree_id = index
            .write_tree()
            .map_err(|error| CoreError::Adapter(format!("write git tree failed: {error}")))?;
        index
            .write()
            .map_err(|error| CoreError::Adapter(format!("persist git index failed: {error}")))?;
        let tree = repo
            .find_tree(tree_id)
            .map_err(|error| CoreError::Adapter(format!("find git tree failed: {error}")))?;
        let signature = resolve_signature(
            &repo,
            request.author_name.as_deref(),
            request.author_email.as_deref(),
        )?;
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .map(|commit| vec![commit])
            .unwrap_or_default();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &request.commit_message,
            &tree,
            &parent_refs,
        )
        .map_err(|error| CoreError::Adapter(format!("create git commit failed: {error}")))?;
    }

    let head_commit = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| CoreError::Adapter(format!("resolve pushed head failed: {error}")))?;
    let head_sha = head_commit.id().to_string();
    let has_deliverable = if changed_files > 0 {
        true
    } else {
        branch_is_ahead_of_base(&repo, head_commit.id(), request.base_branch.as_deref())?
    };
    if !has_deliverable {
        debug!(
            workspace_path = %request.workspace_path.display(),
            branch_name = %request.branch_name,
            base_branch = request.base_branch.as_deref().unwrap_or("<none>"),
            "workspace handoff skipped because branch matches base and there are no changes"
        );
        return Ok(None);
    }

    let mut remote = repo
        .find_remote(&request.remote_name)
        .map_err(|error| CoreError::Adapter(format!("find remote failed: {error}")))?;
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed| {
        resolve_remote_credentials(
            &repo,
            url,
            username_from_url,
            allowed,
            request.auth_token.as_deref(),
        )
    });
    let mut push_options = git2::PushOptions::new();
    push_options.remote_callbacks(callbacks);
    let refspec = format!(
        "refs/heads/{}:refs/heads/{}",
        request.branch_name, request.branch_name
    );
    remote
        .push(&[refspec], Some(&mut push_options))
        .map_err(|error| CoreError::Adapter(format!("push branch failed: {error}")))?;

    info!(
        workspace_path = %request.workspace_path.display(),
        branch_name = %request.branch_name,
        changed_files,
        head_sha = %head_sha,
        "committed and pushed workspace changes"
    );
    Ok(Some(WorkspaceCommitResult {
        branch_name: request.branch_name.clone(),
        head_sha,
        changed_files,
    }))
}

fn branch_is_ahead_of_base(
    repo: &git2::Repository,
    head_oid: git2::Oid,
    base_branch: Option<&str>,
) -> Result<bool, CoreError> {
    let base_commit = resolve_base_commit(repo, base_branch)?;
    if head_oid == base_commit.id() {
        return Ok(false);
    }
    let mut revwalk = repo
        .revwalk()
        .map_err(|error| CoreError::Adapter(format!("create revwalk failed: {error}")))?;
    revwalk
        .push(head_oid)
        .map_err(|error| CoreError::Adapter(format!("walk branch head failed: {error}")))?;
    revwalk
        .hide(base_commit.id())
        .map_err(|error| CoreError::Adapter(format!("hide base branch failed: {error}")))?;
    Ok(revwalk
        .next()
        .transpose()
        .map_err(|error| CoreError::Adapter(format!("walk branch commits failed: {error}")))?
        .is_some())
}

fn sync_existing_workspace(request: &WorkspaceRequest) -> Result<(), CoreError> {
    if !request.sync_on_reuse {
        return Ok(());
    }
    info!(
        workspace_path = %request.workspace_path.display(),
        checkout_kind = ?request.checkout_kind,
        branch_name = ?request.branch_name,
        checkout_ref = ?request.checkout_ref,
        "syncing existing workspace before reuse"
    );
    match request.checkout_kind {
        CheckoutKind::Directory => Ok(()),
        CheckoutKind::LinkedWorktree => {
            if let Some(source_repo_path) = request.source_repo_path.as_deref()
                && let Err(error) =
                    fetch_origin_with_timeout(source_repo_path, request.checkout_ref.as_deref())
            {
                warn!(%error, "fetch origin failed during workspace sync, continuing without remote update");
            }
            sync_existing_repo_checkout(
                &request.workspace_path,
                request.branch_name.as_deref(),
                request.checkout_ref.as_deref(),
                request.default_branch.as_deref(),
            )
        },
        CheckoutKind::DiscreteClone => {
            if let Err(error) =
                fetch_origin_with_timeout(&request.workspace_path, request.checkout_ref.as_deref())
            {
                warn!(%error, "fetch origin failed during workspace sync, continuing without remote update");
            }
            sync_existing_repo_checkout(
                &request.workspace_path,
                request.branch_name.as_deref(),
                request.checkout_ref.as_deref(),
                request.default_branch.as_deref(),
            )
        },
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
    info!(
        source_path = %source_path.display(),
        workspace_path = %request.workspace_path.display(),
        branch_name = %branch_name,
        checkout_ref = ?request.checkout_ref,
        "creating linked worktree"
    );
    let branch_ref = if let Some(checkout_ref) = request.checkout_ref.as_deref() {
        fetch_checkout_ref(&repo, checkout_ref)?;
        ensure_branch_from_ref(&repo, &branch_name, checkout_ref)?
    } else {
        ensure_branch(&repo, &branch_name, request.default_branch.as_deref())?
    };
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
    info!(
        source = %source,
        workspace_path = %request.workspace_path.display(),
        branch_name = ?request.branch_name,
        checkout_ref = ?request.checkout_ref,
        "creating discrete clone workspace"
    );
    let repo = git2::Repository::clone(&source, &request.workspace_path)
        .map_err(|error| CoreError::Adapter(format!("clone failed: {error}")))?;
    if let Some(checkout_ref) = request.checkout_ref.as_deref() {
        fetch_checkout_ref(&repo, checkout_ref)?;
        let branch_name = request
            .branch_name
            .as_deref()
            .unwrap_or(&request.workspace_key);
        checkout_branch_from_ref(&repo, branch_name, checkout_ref)?;
    } else if let Some(branch_name) = &request.branch_name {
        checkout_branch(&repo, branch_name, request.default_branch.as_deref())?;
    }
    Ok(())
}

fn sync_existing_repo_checkout(
    repo_path: &Path,
    branch_name: Option<&str>,
    checkout_ref: Option<&str>,
    default_branch: Option<&str>,
) -> Result<(), CoreError> {
    let repo = git2::Repository::open(repo_path)
        .map_err(|error| CoreError::Adapter(format!("open workspace repo failed: {error}")))?;
    if let Some(checkout_ref) = checkout_ref {
        let branch_name = branch_name.unwrap_or("polyphony-review");
        checkout_branch_from_ref(&repo, branch_name, checkout_ref)?;
    } else if let Some(branch_name) = branch_name {
        checkout_branch(&repo, branch_name, default_branch)?;
    }
    Ok(())
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Run `fetch_origin` in a separate thread with a timeout so a hanging SSH
/// agent (e.g. YubiKey not tapped) doesn't block the orchestrator.
fn fetch_origin_with_timeout(
    repo_path: &Path,
    checkout_ref: Option<&str>,
) -> Result<(), CoreError> {
    let path = repo_path.to_path_buf();
    let checkout_ref = checkout_ref.map(str::to_owned);
    info!(
        repo_path = %path.display(),
        timeout_secs = FETCH_TIMEOUT.as_secs(),
        checkout_ref = ?checkout_ref,
        "fetching origin for workspace reuse"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let repo = git2::Repository::open(&path)
                .map_err(|e| CoreError::Adapter(format!("open repo for fetch failed: {e}")))?;
            fetch_origin(&repo, checkout_ref.as_deref())
        })();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(FETCH_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            warn!("fetch_origin timed out after {FETCH_TIMEOUT:?}");
            Err(CoreError::Adapter("fetch origin timed out".into()))
        },
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(CoreError::Adapter("fetch origin thread panicked".into()))
        },
    }
}

fn fetch_origin(repo: &git2::Repository, checkout_ref: Option<&str>) -> Result<(), CoreError> {
    let Ok(mut remote) = repo.find_remote("origin") else {
        return Ok(());
    };
    debug!("fetching origin for existing workspace");
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed| {
        resolve_remote_credentials(repo, url, username_from_url, allowed, None)
    });
    let mut fetch_options = git2::FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);
    let mut refspecs = vec!["refs/heads/*:refs/remotes/origin/*".to_string()];
    if let Some(checkout_ref) = checkout_ref {
        refspecs.push(format!(
            "{checkout_ref}:{}",
            local_checkout_ref_name(checkout_ref)
        ));
    }
    remote
        .fetch(
            &refspecs.iter().map(String::as_str).collect::<Vec<_>>(),
            Some(&mut fetch_options),
            None,
        )
        .map_err(|error| CoreError::Adapter(format!("fetch origin failed: {error}")))?;
    Ok(())
}

fn fetch_checkout_ref(repo: &git2::Repository, checkout_ref: &str) -> Result<(), CoreError> {
    fetch_origin(repo, Some(checkout_ref))
}

fn resolve_signature(
    repo: &git2::Repository,
    author_name: Option<&str>,
    author_email: Option<&str>,
) -> Result<git2::Signature<'static>, CoreError> {
    let config = repo.config().ok();
    let name = author_name
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            config
                .as_ref()
                .and_then(|config| config.get_string("user.name").ok())
        })
        .unwrap_or_else(|| "polyphony".into());
    let email = author_email
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            config
                .as_ref()
                .and_then(|config| config.get_string("user.email").ok())
        })
        .unwrap_or_else(|| "polyphony@example.com".into());
    git2::Signature::now(&name, &email)
        .map_err(|error| CoreError::Adapter(format!("build commit signature failed: {error}")))
}

fn resolve_remote_credentials(
    repo: &git2::Repository,
    url: &str,
    username_from_url: Option<&str>,
    allowed: git2::CredentialType,
    auth_token: Option<&str>,
) -> Result<git2::Cred, git2::Error> {
    if allowed.contains(git2::CredentialType::SSH_KEY) {
        return git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"));
    }
    if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT)
        && let Some(token) = auth_token
    {
        return git2::Cred::userpass_plaintext("x-access-token", token);
    }
    if allowed.contains(git2::CredentialType::DEFAULT) {
        return git2::Cred::default();
    }
    match repo.config() {
        Ok(config) => git2::Cred::credential_helper(&config, url, username_from_url),
        Err(_) => git2::Cred::default(),
    }
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

fn ensure_branch_from_ref<'a>(
    repo: &'a git2::Repository,
    branch_name: &str,
    checkout_ref: &str,
) -> Result<git2::Reference<'a>, CoreError> {
    let commit = resolve_checkout_ref_commit(repo, checkout_ref)?;
    if let Ok(mut branch) = repo.find_branch(branch_name, git2::BranchType::Local) {
        branch
            .get_mut()
            .set_target(commit.id(), "update review branch")
            .map_err(|error| CoreError::Adapter(format!("update branch failed: {error}")))?;
        return branch
            .into_reference()
            .resolve()
            .map_err(|error| CoreError::Adapter(format!("resolve branch failed: {error}")));
    }
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

fn checkout_branch_from_ref(
    repo: &git2::Repository,
    branch_name: &str,
    checkout_ref: &str,
) -> Result<(), CoreError> {
    let reference = ensure_branch_from_ref(repo, branch_name, checkout_ref)?;
    let ref_name = reference
        .name()
        .ok_or_else(|| CoreError::Adapter("branch reference name missing".into()))?;
    repo.set_head(ref_name)
        .map_err(|error| CoreError::Adapter(format!("set head failed: {error}")))?;
    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::new()
            .force()
            .remove_untracked(true),
    ))
    .map_err(|error| CoreError::Adapter(format!("checkout head failed: {error}")))?;
    Ok(())
}

fn resolve_checkout_ref_commit<'a>(
    repo: &'a git2::Repository,
    checkout_ref: &str,
) -> Result<git2::Commit<'a>, CoreError> {
    let local_ref = local_checkout_ref_name(checkout_ref);
    repo.find_reference(&local_ref)
        .or_else(|_| repo.find_reference(checkout_ref))
        .map_err(|error| CoreError::Adapter(format!("resolve checkout ref failed: {error}")))?
        .peel_to_commit()
        .map_err(|error| CoreError::Adapter(format!("resolve checkout commit failed: {error}")))
}

fn local_checkout_ref_name(checkout_ref: &str) -> String {
    let trimmed = checkout_ref.strip_prefix("refs/").unwrap_or(checkout_ref);
    format!("refs/remotes/origin/{trimmed}")
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

/// Detect a GitHub remote from the `origin` remote of a repository.
///
/// Returns `Some("owner/repo")` when `origin` points to GitHub
/// (SSH or HTTPS format). Returns `None` otherwise.
pub fn detect_github_remote(repo_root: &Path) -> Option<String> {
    let repo = git2::Repository::open(repo_root).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?;
    parse_github_owner_repo(url)
}

fn parse_github_owner_repo(url: &str) -> Option<String> {
    // SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let slug = rest.trim_end_matches(".git");
        if slug.contains('/') && !slug.is_empty() {
            return Some(slug.to_string());
        }
    }
    // HTTPS: https://github.com/owner/repo or https://github.com/owner/repo.git
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        let slug = rest.trim_end_matches(".git").trim_end_matches('/');
        if slug.contains('/') && !slug.is_empty() {
            return Some(slug.to_string());
        }
    }
    None
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::path::{Path, PathBuf};

    use {
        polyphony_core::{
            CheckoutKind, WorkspaceCommitRequest, WorkspaceCommitter, WorkspaceProvisioner,
            WorkspaceRequest,
        },
        tempfile::tempdir,
    };

    use super::{
        GitWorkspaceCommitter, GitWorkspaceProvisioner, checkout_branch, detect_github_remote,
        parse_github_owner_repo,
    };

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
            checkout_ref: None,
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

    fn commit_on_branch(repo: &git2::Repository, branch_name: &str, file_name: &str, body: &str) {
        if repo
            .find_branch(branch_name, git2::BranchType::Local)
            .is_err()
        {
            let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch(branch_name, &head_commit, false).unwrap();
        }
        repo.set_head(&format!("refs/heads/{branch_name}")).unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        std::fs::write(repo.workdir().unwrap().join(file_name), format!("{body}\n")).unwrap();
        commit_all(repo, &format!("update {branch_name}"));
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

    #[tokio::test]
    async fn linked_worktree_checkout_ref_tracks_pull_request_head() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("source");
        let source_repo = init_repo(&source_path);
        commit_on_branch(&source_repo, "feature/review", "review.txt", "first");
        let feature_commit = source_repo.head().unwrap().peel_to_commit().unwrap();
        source_repo
            .reference(
                "refs/pull/42/head",
                feature_commit.id(),
                true,
                "test pr ref",
            )
            .unwrap();

        let root = temp.path().join("workspaces");
        let provisioner = GitWorkspaceProvisioner;
        let mut request = make_request(
            &root,
            "FAC-105",
            CheckoutKind::LinkedWorktree,
            Some(source_path.clone()),
        );
        request.branch_name = Some("pr-review/42".into());
        request.checkout_ref = Some("refs/pull/42/head".into());

        let workspace = provisioner.ensure_workspace(request.clone()).await.unwrap();
        let repo = git2::Repository::open(&workspace.path).unwrap();
        assert_eq!(current_branch(&repo), "pr-review/42");
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            feature_commit.id()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("review.txt")).unwrap(),
            "first\n"
        );

        commit_on_branch(&source_repo, "feature/review", "review.txt", "second");
        let next_commit = source_repo.head().unwrap().peel_to_commit().unwrap();
        source_repo
            .reference("refs/pull/42/head", next_commit.id(), true, "update pr ref")
            .unwrap();

        let workspace = provisioner.ensure_workspace(request).await.unwrap();
        let repo = git2::Repository::open(&workspace.path).unwrap();
        assert!(!workspace.created_now);
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().id(),
            next_commit.id()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("review.txt")).unwrap(),
            "second\n"
        );
    }

    #[tokio::test]
    async fn committer_creates_commit_and_pushes_branch() {
        let temp = tempdir().unwrap();
        let remote_path = temp.path().join("remote.git");
        git2::Repository::init_bare(&remote_path).unwrap();

        let repo_path = temp.path().join("repo");
        let repo = init_repo(&repo_path);
        repo.remote("origin", &remote_path.display().to_string())
            .unwrap();
        std::fs::write(repo_path.join("README.md"), "updated\n").unwrap();

        let committer = GitWorkspaceCommitter;
        let result = committer
            .commit_and_push(&WorkspaceCommitRequest {
                workspace_path: repo_path.clone(),
                branch_name: "main".into(),
                base_branch: Some("main".into()),
                commit_message: "test handoff".into(),
                remote_name: "origin".into(),
                auth_token: None,
                author_name: Some("polyphony".into()),
                author_email: Some("polyphony@example.com".into()),
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.branch_name, "main");
        assert_eq!(result.changed_files, 1);

        let remote = git2::Repository::open_bare(&remote_path).unwrap();
        let remote_head = remote
            .find_reference("refs/heads/main")
            .unwrap()
            .target()
            .unwrap()
            .to_string();
        assert_eq!(remote_head, result.head_sha);
    }

    #[tokio::test]
    async fn committer_skips_clean_branch_when_it_matches_base() {
        let temp = tempdir().unwrap();
        let remote_path = temp.path().join("remote.git");
        git2::Repository::init_bare(&remote_path).unwrap();
        let repo_path = temp.path().join("repo");
        let repo = init_repo(&repo_path);
        repo.remote("origin", &remote_path.display().to_string())
            .unwrap();
        std::fs::write(repo_path.join("README.md"), "updated\n").unwrap();

        let committer = GitWorkspaceCommitter;
        let request = WorkspaceCommitRequest {
            workspace_path: repo_path,
            branch_name: "main".into(),
            base_branch: Some("main".into()),
            commit_message: "test handoff".into(),
            remote_name: "origin".into(),
            auth_token: None,
            author_name: Some("polyphony".into()),
            author_email: Some("polyphony@example.com".into()),
        };
        let first_result = committer.commit_and_push(&request).await.unwrap().unwrap();
        assert_eq!(first_result.changed_files, 1);

        let result = committer.commit_and_push(&request).await.unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn committer_pushes_clean_branch_that_is_ahead_of_base() {
        let temp = tempdir().unwrap();
        let remote_path = temp.path().join("remote.git");
        git2::Repository::init_bare(&remote_path).unwrap();

        let repo_path = temp.path().join("repo");
        let repo = init_repo(&repo_path);
        repo.remote("origin", &remote_path.display().to_string())
            .unwrap();
        checkout_branch(&repo, "issue-2", Some("main")).unwrap();
        std::fs::write(repo_path.join("foobar.txt"), "dogfood ok\n").unwrap();
        let committer = GitWorkspaceCommitter;
        let request = WorkspaceCommitRequest {
            workspace_path: repo_path.clone(),
            branch_name: "issue-2".into(),
            base_branch: Some("main".into()),
            commit_message: "docs(dogfood): add foobar.txt for issue #2".into(),
            remote_name: "origin".into(),
            auth_token: None,
            author_name: Some("polyphony".into()),
            author_email: Some("polyphony@example.com".into()),
        };
        let first_result = committer.commit_and_push(&request).await.unwrap().unwrap();

        let result = committer.commit_and_push(&request).await.unwrap().unwrap();

        assert_eq!(result.branch_name, "issue-2");
        assert_eq!(result.changed_files, 0);
        assert_eq!(result.head_sha, first_result.head_sha);

        let remote = git2::Repository::open_bare(&remote_path).unwrap();
        let remote_head = remote
            .find_reference("refs/heads/issue-2")
            .unwrap()
            .target()
            .unwrap()
            .to_string();
        assert_eq!(remote_head, result.head_sha);
    }

    #[test]
    fn parse_github_ssh_url() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:openai/symphony.git"),
            Some("openai/symphony".into())
        );
    }

    #[test]
    fn parse_github_ssh_url_without_dot_git() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:openai/symphony"),
            Some("openai/symphony".into())
        );
    }

    #[test]
    fn parse_github_https_url() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/penso/polyphony.git"),
            Some("penso/polyphony".into())
        );
    }

    #[test]
    fn parse_github_https_url_without_dot_git() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/penso/polyphony"),
            Some("penso/polyphony".into())
        );
    }

    #[test]
    fn parse_github_https_with_trailing_slash() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/penso/polyphony/"),
            Some("penso/polyphony".into())
        );
    }

    #[test]
    fn parse_non_github_url_returns_none() {
        assert_eq!(
            parse_github_owner_repo("git@gitlab.com:owner/repo.git"),
            None
        );
    }

    #[test]
    fn detect_github_remote_from_repo_with_github_origin() {
        let temp = tempdir().unwrap();
        let repo_path = temp.path().join("gh-repo");
        let repo = init_repo(&repo_path);
        repo.remote("origin", "git@github.com:penso/polyphony.git")
            .unwrap();

        assert_eq!(
            detect_github_remote(&repo_path),
            Some("penso/polyphony".into())
        );
    }

    #[test]
    fn detect_github_remote_returns_none_for_non_github() {
        let temp = tempdir().unwrap();
        let repo_path = temp.path().join("gl-repo");
        let repo = init_repo(&repo_path);
        repo.remote("origin", "git@gitlab.com:owner/repo.git")
            .unwrap();

        assert_eq!(detect_github_remote(&repo_path), None);
    }

    #[test]
    fn detect_github_remote_returns_none_without_origin() {
        let temp = tempdir().unwrap();
        let repo_path = temp.path().join("no-origin");
        init_repo(&repo_path);

        assert_eq!(detect_github_remote(&repo_path), None);
    }
}
