use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use {
    polyphony_agent_common::wrap_command,
    polyphony_core::{
        CheckoutKind, Error as CoreError, SandboxConfig, Workspace, WorkspaceProvisioner,
        WorkspaceRequest, sanitize_workspace_key,
    },
    polyphony_workflow::HooksConfig,
    thiserror::Error,
    tokio::{io::AsyncReadExt, process::Command},
    tracing::{info, warn},
};

const HOOK_OUTPUT_LOG_LIMIT: usize = 2_000;

#[derive(Debug, Error)]
pub enum Error {
    #[error("core error: {0}")]
    Core(#[from] CoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("hook failure: {0}")]
    Hook(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HookLogOutput {
    text: Option<String>,
    truncated: bool,
    bytes: usize,
}

#[derive(Debug)]
struct HookCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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

    pub async fn list_workspaces(&self) -> Vec<(String, PathBuf)> {
        let mut entries = Vec::new();
        let mut read_dir = match tokio::fs::read_dir(&self.root).await {
            Ok(rd) => rd,
            Err(_) => return entries,
        };
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                entries.push((name.to_string(), path));
            }
        }
        entries
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
        self.ensure_workspace_with_ref(issue_identifier, branch_name, None, hooks)
            .await
    }

    pub async fn ensure_workspace_with_ref(
        &self,
        issue_identifier: &str,
        branch_name: Option<String>,
        checkout_ref: Option<String>,
        hooks: &HooksConfig,
    ) -> Result<Workspace, Error> {
        tokio::fs::create_dir_all(&self.root).await?;
        let request = self.workspace_request(issue_identifier, branch_name, checkout_ref)?;
        let started_at = Instant::now();
        info!(
            issue_identifier,
            workspace_path = %request.workspace_path.display(),
            checkout_kind = ?request.checkout_kind,
            sync_on_reuse = request.sync_on_reuse,
            branch_name = ?request.branch_name,
            checkout_ref = ?request.checkout_ref,
            "ensuring workspace"
        );
        let workspace = self.provisioner.ensure_workspace(request.clone()).await?;
        self.cleanup_transient_artifacts(&workspace.path).await?;
        if workspace.created_now
            && let Err(error) = self
                .run_hook(
                    "after_create",
                    hooks.after_create.as_deref(),
                    &workspace.path,
                    hooks.timeout_ms,
                    None,
                )
                .await
        {
            if let Err(cleanup_error) = self.provisioner.cleanup_workspace(request).await {
                warn!(%cleanup_error, path = %workspace.path.display(), "workspace rollback failed");
            }
            return Err(error);
        }
        info!(
            issue_identifier,
            workspace_path = %workspace.path.display(),
            created_now = workspace.created_now,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "workspace ready"
        );
        Ok(workspace)
    }

    pub async fn run_before_run(
        &self,
        hooks: &HooksConfig,
        workspace_path: &Path,
        sandbox: Option<&SandboxConfig>,
    ) -> Result<(), Error> {
        ensure_contained(&self.root, workspace_path)?;
        self.cleanup_transient_artifacts(workspace_path).await?;
        info!(workspace_path = %workspace_path.display(), "preparing workspace before run");
        self.run_hook(
            "before_run",
            hooks.before_run.as_deref(),
            workspace_path,
            hooks.timeout_ms,
            sandbox,
        )
        .await
    }

    pub async fn run_after_run_best_effort(
        &self,
        hooks: &HooksConfig,
        workspace_path: &Path,
        sandbox: Option<&SandboxConfig>,
    ) {
        if let Err(error) = self
            .run_hook(
                "after_run",
                hooks.after_run.as_deref(),
                workspace_path,
                hooks.timeout_ms,
                sandbox,
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
        let request = self.workspace_request(issue_identifier, branch_name, None)?;
        if tokio::fs::metadata(&request.workspace_path).await.is_err() {
            return Ok(());
        }
        info!(
            issue_identifier,
            workspace_path = %request.workspace_path.display(),
            "cleaning up workspace"
        );
        if let Err(error) = self
            .run_hook(
                "before_remove",
                hooks.before_remove.as_deref(),
                &request.workspace_path,
                hooks.timeout_ms,
                None,
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
        checkout_ref: Option<String>,
    ) -> Result<WorkspaceRequest, Error> {
        let (workspace_key, workspace_path) = self.workspace_path_for(issue_identifier)?;
        Ok(WorkspaceRequest {
            issue_identifier: issue_identifier.to_string(),
            workspace_root: self.root.clone(),
            workspace_path,
            workspace_key,
            branch_name,
            checkout_ref,
            checkout_kind: self.checkout_kind,
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
        sandbox: Option<&SandboxConfig>,
    ) -> Result<(), Error> {
        let Some(script) = script else {
            return Ok(());
        };
        ensure_contained(&self.root, cwd)?;
        info!(
            hook = hook_name,
            workspace_path = %cwd.display(),
            timeout_ms,
            "starting workspace hook"
        );
        let wrapped = wrap_command(sandbox, cwd, &BTreeMap::new(), script, false)?;
        let mut command = Command::new(&wrapped.program);
        command
            .args(&wrapped.args)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command.env_remove("CLAUDECODE");
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Hook(format!("{hook_name} stdout unavailable")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Hook(format!("{hook_name} stderr unavailable")))?;
        let stdout_handle = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut output = Vec::new();
            reader.read_to_end(&mut output).await?;
            Ok::<Vec<u8>, std::io::Error>(output)
        });
        let stderr_handle = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut output = Vec::new();
            reader.read_to_end(&mut output).await?;
            Ok::<Vec<u8>, std::io::Error>(output)
        });

        let (status, timed_out) =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
                Ok(status) => (status?, false),
                Err(_) => {
                    let _ = child.kill().await;
                    (child.wait().await?, true)
                },
            };
        let output = HookCommandOutput {
            status,
            stdout: join_hook_output(stdout_handle).await?,
            stderr: join_hook_output(stderr_handle).await?,
        };
        log_hook_output(hook_name, cwd, timeout_ms, &output, timed_out);
        if timed_out {
            return Err(Error::Hook(format!("{hook_name} timed out")));
        }
        if !output.status.success() {
            return Err(Error::Hook(format!(
                "{hook_name} exited with status {}",
                output.status
            )));
        }
        Ok(())
    }
}

async fn join_hook_output(
    handle: tokio::task::JoinHandle<Result<Vec<u8>, std::io::Error>>,
) -> Result<Vec<u8>, Error> {
    handle
        .await
        .map_err(|error| Error::Hook(format!("hook output task failed: {error}")))?
        .map_err(Error::Io)
}

fn log_hook_output(
    hook_name: &str,
    cwd: &Path,
    timeout_ms: u64,
    output: &HookCommandOutput,
    timed_out: bool,
) {
    let stdout = summarize_hook_output(&output.stdout);
    let stderr = summarize_hook_output(&output.stderr);
    if timed_out {
        warn!(
            hook = hook_name,
            workspace_path = %cwd.display(),
            timeout_ms,
            status = %output.status,
            stdout = %stdout.text.as_deref().unwrap_or(""),
            stdout_bytes = stdout.bytes,
            stdout_truncated = stdout.truncated,
            stderr = %stderr.text.as_deref().unwrap_or(""),
            stderr_bytes = stderr.bytes,
            stderr_truncated = stderr.truncated,
            "workspace hook timed out"
        );
        return;
    }
    if output.status.success() {
        info!(
            hook = hook_name,
            workspace_path = %cwd.display(),
            status = %output.status,
            stdout = %stdout.text.as_deref().unwrap_or(""),
            stdout_bytes = stdout.bytes,
            stdout_truncated = stdout.truncated,
            stderr = %stderr.text.as_deref().unwrap_or(""),
            stderr_bytes = stderr.bytes,
            stderr_truncated = stderr.truncated,
            "workspace hook completed"
        );
    } else {
        warn!(
            hook = hook_name,
            workspace_path = %cwd.display(),
            status = %output.status,
            stdout = %stdout.text.as_deref().unwrap_or(""),
            stdout_bytes = stdout.bytes,
            stdout_truncated = stdout.truncated,
            stderr = %stderr.text.as_deref().unwrap_or(""),
            stderr_bytes = stderr.bytes,
            stderr_truncated = stderr.truncated,
            "workspace hook failed"
        );
    }
}

fn summarize_hook_output(bytes: &[u8]) -> HookLogOutput {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    let bytes_len = text.len();
    if text.is_empty() {
        return HookLogOutput {
            text: None,
            truncated: false,
            bytes: bytes_len,
        };
    }
    let char_count = text.chars().count();
    if char_count <= HOOK_OUTPUT_LOG_LIMIT {
        return HookLogOutput {
            text: Some(text),
            truncated: false,
            bytes: bytes_len,
        };
    }
    let mut truncated = text.chars().take(HOOK_OUTPUT_LOG_LIMIT).collect::<String>();
    truncated.push_str("...");
    HookLogOutput {
        text: Some(truncated),
        truncated: true,
        bytes: bytes_len,
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
            vec!["tmp".into()],
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
        let mut hooks = hooks();
        hooks.before_run = Some("[ ! -e tmp ]".into());

        manager
            .run_before_run(&hooks, &workspace.path, None)
            .await
            .unwrap();

        assert!(!workspace.path.join("tmp").exists());
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

    #[test]
    fn summarize_hook_output_truncates_large_output() {
        let output = super::summarize_hook_output("x".repeat(2_100).as_bytes());

        assert_eq!(output.bytes, 2_100);
        assert!(output.truncated);
        assert_eq!(output.text.as_ref().unwrap().chars().count(), 2_003);
        assert!(output.text.as_ref().unwrap().ends_with("..."));
    }

    #[test]
    fn summarize_hook_output_trims_trailing_newlines() {
        let output = super::summarize_hook_output(b"hello\n");

        assert_eq!(output.bytes, 5);
        assert!(!output.truncated);
        assert_eq!(output.text.as_deref(), Some("hello"));
    }
}
