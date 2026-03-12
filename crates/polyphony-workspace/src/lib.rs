use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use {
    polyphony_core::{
        CheckoutKind, Error as CoreError, Workspace, WorkspaceProvisioner, WorkspaceRequest,
        sanitize_workspace_key,
    },
    polyphony_workflow::HooksConfig,
    thiserror::Error,
    tokio::process::Command,
    tracing::warn,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("core error: {0}")]
    Core(#[from] CoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("hook failure: {0}")]
    Hook(String),
}

pub struct WorkspaceManager {
    root: PathBuf,
    provisioner: Arc<dyn WorkspaceProvisioner>,
    checkout_kind: CheckoutKind,
    sync_on_reuse: bool,
    transient_paths: Vec<String>,
    source_repo_path: Option<PathBuf>,
    clone_url: Option<String>,
    default_branch: Option<String>,
}

impl WorkspaceManager {
    pub fn new(
        root: PathBuf,
        provisioner: Arc<dyn WorkspaceProvisioner>,
        checkout_kind: CheckoutKind,
        sync_on_reuse: bool,
        transient_paths: Vec<String>,
        source_repo_path: Option<PathBuf>,
        clone_url: Option<String>,
        default_branch: Option<String>,
    ) -> Self {
        Self {
            root,
            provisioner,
            checkout_kind,
            sync_on_reuse,
            transient_paths,
            source_repo_path,
            clone_url,
            default_branch,
        }
    }

    pub fn workspace_path_for(&self, issue_identifier: &str) -> Result<(String, PathBuf), Error> {
        let workspace_key = sanitize_workspace_key(issue_identifier);
        let workspace_path = self.root.join(&workspace_key);
        ensure_contained(&self.root, &workspace_path)?;
        Ok((workspace_key, workspace_path))
    }

    pub async fn ensure_workspace(
        &self,
        issue_identifier: &str,
        branch_name: Option<String>,
        hooks: &HooksConfig,
    ) -> Result<Workspace, Error> {
        tokio::fs::create_dir_all(&self.root).await?;
        let request = self.workspace_request(issue_identifier, branch_name)?;
        let workspace = self.provisioner.ensure_workspace(request.clone()).await?;
        self.cleanup_transient_artifacts(&workspace.path).await?;
        if workspace.created_now
            && let Err(error) = self
                .run_hook(
                    "after_create",
                    hooks.after_create.as_deref(),
                    &workspace.path,
                    hooks.timeout_ms,
                )
                .await
        {
            if let Err(cleanup_error) = self.provisioner.cleanup_workspace(request).await {
                warn!(%cleanup_error, path = %workspace.path.display(), "workspace rollback failed");
            }
            return Err(error);
        }
        Ok(workspace)
    }

    pub async fn run_before_run(
        &self,
        hooks: &HooksConfig,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        ensure_contained(&self.root, workspace_path)?;
        self.cleanup_transient_artifacts(workspace_path).await?;
        self.run_hook(
            "before_run",
            hooks.before_run.as_deref(),
            workspace_path,
            hooks.timeout_ms,
        )
        .await
    }

    pub async fn run_after_run_best_effort(&self, hooks: &HooksConfig, workspace_path: &Path) {
        if let Err(error) = self
            .run_hook(
                "after_run",
                hooks.after_run.as_deref(),
                workspace_path,
                hooks.timeout_ms,
            )
            .await
        {
            warn!(%error, "after_run hook failed");
        }
    }

    pub async fn cleanup_workspace(
        &self,
        issue_identifier: &str,
        branch_name: Option<String>,
        hooks: &HooksConfig,
    ) -> Result<(), Error> {
        let request = self.workspace_request(issue_identifier, branch_name)?;
        if tokio::fs::metadata(&request.workspace_path).await.is_err() {
            return Ok(());
        }
        if let Err(error) = self
            .run_hook(
                "before_remove",
                hooks.before_remove.as_deref(),
                &request.workspace_path,
                hooks.timeout_ms,
            )
            .await
        {
            warn!(%error, "before_remove hook failed");
        }
        self.provisioner.cleanup_workspace(request).await?;
        Ok(())
    }

    fn workspace_request(
        &self,
        issue_identifier: &str,
        branch_name: Option<String>,
    ) -> Result<WorkspaceRequest, Error> {
        let (workspace_key, workspace_path) = self.workspace_path_for(issue_identifier)?;
        Ok(WorkspaceRequest {
            issue_identifier: issue_identifier.to_string(),
            workspace_root: self.root.clone(),
            workspace_path,
            workspace_key,
            branch_name,
            checkout_kind: self.checkout_kind.clone(),
            sync_on_reuse: self.sync_on_reuse,
            source_repo_path: self.source_repo_path.clone(),
            clone_url: self.clone_url.clone(),
            default_branch: self.default_branch.clone(),
        })
    }

    async fn cleanup_transient_artifacts(&self, workspace_path: &Path) -> Result<(), Error> {
        ensure_contained(&self.root, workspace_path)?;
        for artifact in &self.transient_paths {
            let artifact_path = workspace_path.join(artifact);
            match tokio::fs::symlink_metadata(&artifact_path).await {
                Ok(metadata) if metadata.is_dir() => {
                    tokio::fs::remove_dir_all(&artifact_path).await?;
                },
                Ok(_) => {
                    tokio::fs::remove_file(&artifact_path).await?;
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(Error::Io(error)),
            }
        }
        Ok(())
    }

    async fn run_hook(
        &self,
        hook_name: &str,
        script: Option<&str>,
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<(), Error> {
        let Some(script) = script else {
            return Ok(());
        };
        ensure_contained(&self.root, cwd)?;
        let mut command = Command::new("bash");
        command.arg("-lc").arg(script).current_dir(cwd);
        let status = tokio::time::timeout(Duration::from_millis(timeout_ms), command.status())
            .await
            .map_err(|_| Error::Hook(format!("{hook_name} timed out")))??;
        if !status.success() {
            return Err(Error::Hook(format!(
                "{hook_name} exited with status {status}"
            )));
        }
        Ok(())
    }
}

fn ensure_contained(root: &Path, workspace: &Path) -> Result<(), Error> {
    let root = absolute_path(root);
    let workspace = absolute_path(workspace);
    if !workspace.starts_with(&root) {
        return Err(Error::Hook(format!(
            "workspace path escapes root: {}",
            workspace.display()
        )));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use {
        async_trait::async_trait,
        polyphony_core::{
            CheckoutKind, Error as CoreError, Workspace, WorkspaceProvisioner, WorkspaceRequest,
        },
        polyphony_workflow::HooksConfig,
        tempfile::tempdir,
    };

    use super::WorkspaceManager;

    #[derive(Default)]
    struct RecordingProvisioner {
        ensured: Arc<Mutex<Vec<WorkspaceRequest>>>,
        cleaned: Arc<Mutex<Vec<WorkspaceRequest>>>,
    }

    #[async_trait]
    impl WorkspaceProvisioner for RecordingProvisioner {
        fn component_key(&self) -> String {
            "workspace:test".into()
        }

        async fn ensure_workspace(
            &self,
            request: WorkspaceRequest,
        ) -> Result<Workspace, CoreError> {
            std::fs::create_dir_all(&request.workspace_path)
                .map_err(|error| CoreError::Adapter(error.to_string()))?;
            let created_now = self.ensured.lock().unwrap().is_empty();
            self.ensured.lock().unwrap().push(request.clone());
            Ok(Workspace {
                path: request.workspace_path,
                workspace_key: request.workspace_key,
                created_now,
                branch_name: request.branch_name,
            })
        }

        async fn cleanup_workspace(&self, request: WorkspaceRequest) -> Result<(), CoreError> {
            self.cleaned.lock().unwrap().push(request.clone());
            if request.workspace_path.exists() {
                std::fs::remove_dir_all(&request.workspace_path)
                    .map_err(|error| CoreError::Adapter(error.to_string()))?;
            }
            Ok(())
        }
    }

    fn manager(root: PathBuf, provisioner: Arc<dyn WorkspaceProvisioner>) -> WorkspaceManager {
        WorkspaceManager::new(
            root,
            provisioner,
            CheckoutKind::Directory,
            true,
            vec!["tmp".into(), ".elixir_ls".into()],
            None,
            None,
            None,
        )
    }

    fn hooks() -> HooksConfig {
        HooksConfig {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 5_000,
        }
    }

    #[tokio::test]
    async fn after_create_runs_only_for_new_workspace() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("workspaces");
        let provisioner = Arc::new(RecordingProvisioner::default());
        let manager = manager(root, provisioner);
        let mut hooks = hooks();
        hooks.after_create = Some("printf x >> .after_create_count".into());

        let workspace = manager
            .ensure_workspace("FAC-1", Some("task/fac-1".into()), &hooks)
            .await
            .unwrap();
        manager
            .ensure_workspace("FAC-1", Some("task/fac-1".into()), &hooks)
            .await
            .unwrap();

        let count_file = workspace.path.join(".after_create_count");
        assert_eq!(std::fs::read_to_string(count_file).unwrap(), "x");
    }

    #[tokio::test]
    async fn failed_after_create_rolls_back_workspace() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("workspaces");
        let provisioner = Arc::new(RecordingProvisioner::default());
        let manager = manager(root.clone(), provisioner);
        let mut hooks = hooks();
        hooks.after_create = Some("exit 7".into());

        let error = manager
            .ensure_workspace("FAC-2", Some("task/fac-2".into()), &hooks)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("after_create"));
        assert!(!root.join("FAC-2").exists());
    }

    #[tokio::test]
    async fn before_run_cleans_transient_artifacts() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("workspaces");
        let provisioner = Arc::new(RecordingProvisioner::default());
        let manager = manager(root.clone(), provisioner);
        let workspace = manager
            .ensure_workspace("FAC-3", Some("task/fac-3".into()), &hooks())
            .await
            .unwrap();
        std::fs::create_dir_all(workspace.path.join("tmp")).unwrap();
        std::fs::create_dir_all(workspace.path.join(".elixir_ls")).unwrap();
        let mut hooks = hooks();
        hooks.before_run = Some("[ ! -e tmp ] && [ ! -e .elixir_ls ]".into());

        manager
            .run_before_run(&hooks, &workspace.path)
            .await
            .unwrap();

        assert!(!workspace.path.join("tmp").exists());
        assert!(!workspace.path.join(".elixir_ls").exists());
    }

    #[tokio::test]
    async fn cleanup_runs_before_remove_and_deletes_workspace() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("workspaces");
        let provisioner = Arc::new(RecordingProvisioner::default());
        let manager = manager(root.clone(), provisioner);
        let workspace = manager
            .ensure_workspace("FAC-4", Some("task/fac-4".into()), &hooks())
            .await
            .unwrap();
        let mut hooks = hooks();
        hooks.before_remove = Some("touch ../before_remove_ran".into());

        manager
            .cleanup_workspace("FAC-4", Some("task/fac-4".into()), &hooks)
            .await
            .unwrap();

        assert!(root.join("before_remove_ran").exists());
        assert!(!workspace.path.exists());
    }
}
