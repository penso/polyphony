#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    collections::VecDeque,
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use polyphony_core::{
    AgentSession, Deliverable, DeliverableDecision, DeliverableKind, DeliverableStatus,
    DispatchMode, IssueAuthor, IssueComment, IssueStateUpdate, PullRequestRef, StoreBootstrap,
    UpdateIssueRequest, Workspace, WorkspaceCommitResult, WorkspaceRequest,
};
use polyphony_workflow::load_workflow;
use serde_json::json;
use tokio::{
    sync::{Notify, watch},
    time::timeout,
};

use crate::{helpers::*, prelude::*, *};

#[derive(Clone)]
struct TestTracker {
    issues: Arc<Mutex<HashMap<String, Issue>>>,
    workflow_updates: Arc<Mutex<Vec<String>>>,
    fetch_by_ids_calls: Arc<Mutex<u32>>,
    issue_updates: Arc<Mutex<Vec<UpdateIssueRequest>>>,
    acknowledged_issues: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct DelayedCleanupTracker {
    issues: Arc<Vec<Issue>>,
    cleanup_gate: Arc<Notify>,
}

#[async_trait]
impl IssueTracker for DelayedCleanupTracker {
    fn component_key(&self) -> String {
        "tracker:delayed-cleanup".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self.issues.as_ref().clone())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        self.cleanup_gate.notified().await;
        Ok(Vec::new())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self
            .issues
            .iter()
            .filter(|issue| issue_ids.contains(&issue.id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        Ok(self
            .issues
            .iter()
            .filter(|issue| issue_ids.contains(&issue.id))
            .map(|issue| IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }
}

impl TestTracker {
    fn new(issues: Vec<Issue>) -> Self {
        Self {
            issues: Arc::new(Mutex::new(
                issues
                    .into_iter()
                    .map(|issue| (issue.id.clone(), issue))
                    .collect(),
            )),
            workflow_updates: Arc::new(Mutex::new(Vec::new())),
            fetch_by_ids_calls: Arc::new(Mutex::new(0)),
            issue_updates: Arc::new(Mutex::new(Vec::new())),
            acknowledged_issues: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_workflow_updates(&self) -> Vec<String> {
        self.workflow_updates.lock().unwrap().clone()
    }

    fn fetch_by_ids_calls(&self) -> u32 {
        *self.fetch_by_ids_calls.lock().unwrap()
    }

    fn recorded_issue_updates(&self) -> Vec<UpdateIssueRequest> {
        self.issue_updates.lock().unwrap().clone()
    }

    fn acknowledged_issues(&self) -> Vec<String> {
        self.acknowledged_issues.lock().unwrap().clone()
    }
}

#[async_trait]
impl IssueTracker for TestTracker {
    fn component_key(&self) -> String {
        "tracker:test".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self.issues.lock().unwrap().values().cloned().collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        let normalized = states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        Ok(self
            .issues
            .lock()
            .unwrap()
            .values()
            .filter(|issue| normalized.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        *self.fetch_by_ids_calls.lock().unwrap() += 1;
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<polyphony_core::IssueStateUpdate>, polyphony_core::Error> {
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .map(|issue| polyphony_core::IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }

    async fn update_issue_workflow_status(
        &self,
        _issue: &Issue,
        status: &str,
    ) -> Result<(), polyphony_core::Error> {
        self.workflow_updates
            .lock()
            .unwrap()
            .push(status.to_string());
        Ok(())
    }

    async fn update_issue(
        &self,
        request: &UpdateIssueRequest,
    ) -> Result<Issue, polyphony_core::Error> {
        self.issue_updates.lock().unwrap().push(request.clone());
        let mut issues = self.issues.lock().unwrap();
        let issue = issues.get_mut(&request.id).ok_or_else(|| {
            polyphony_core::Error::Adapter(format!("issue {} not found", request.id))
        })?;
        if let Some(state) = &request.state {
            issue.state = state.clone();
        }
        if let Some(title) = &request.title {
            issue.title = title.clone();
        }
        if let Some(description) = &request.description {
            issue.description = Some(description.clone());
        }
        if let Some(priority) = request.priority {
            issue.priority = Some(priority);
        }
        issue.updated_at = Some(Utc::now());
        Ok(issue.clone())
    }

    async fn acknowledge_issue(&self, issue: &Issue) -> Result<(), polyphony_core::Error> {
        self.acknowledged_issues
            .lock()
            .unwrap()
            .push(issue.id.clone());
        Ok(())
    }
}

struct NoopAgent;

#[async_trait]
impl AgentRuntime for NoopAgent {
    fn component_key(&self) -> String {
        "provider:test".into()
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Ok(AgentRunResult::succeeded(1))
    }
}

#[derive(Clone, Default)]
struct RecordingPullRequestCommenter {
    comments: Arc<Mutex<Vec<(PullRequestRef, String)>>>,
    reviews: Arc<
        Mutex<
            Vec<(
                PullRequestRef,
                String,
                Vec<PullRequestReviewComment>,
                String,
            )>,
        >,
    >,
}

impl RecordingPullRequestCommenter {
    fn comment_bodies(&self) -> Vec<String> {
        self.comments
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body)| body.clone())
            .collect()
    }

    fn reviews(
        &self,
    ) -> Vec<(
        PullRequestRef,
        String,
        Vec<PullRequestReviewComment>,
        String,
    )> {
        self.reviews.lock().unwrap().clone()
    }
}

#[async_trait]
impl PullRequestCommenter for RecordingPullRequestCommenter {
    fn component_key(&self) -> String {
        "github:test-comments".into()
    }

    async fn comment_on_pull_request(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
    ) -> Result<(), polyphony_core::Error> {
        self.comments
            .lock()
            .unwrap()
            .push((pull_request.clone(), body.to_string()));
        Ok(())
    }

    async fn sync_pull_request_comment(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
    ) -> Result<(), polyphony_core::Error> {
        let mut comments = self.comments.lock().unwrap();
        if let Some((_, existing_body)) = comments
            .iter_mut()
            .find(|(_, existing_body)| existing_body.contains(marker))
        {
            *existing_body = body.to_string();
        } else {
            comments.push((pull_request.clone(), body.to_string()));
        }
        Ok(())
    }

    async fn sync_pull_request_review(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
        comments: &[PullRequestReviewComment],
        commit_sha: &str,
        _verdict: polyphony_core::ReviewVerdict,
    ) -> Result<(), polyphony_core::Error> {
        let mut reviews = self.reviews.lock().unwrap();
        if reviews.iter().any(|review| review.1.contains(marker)) {
            return Ok(());
        }
        reviews.push((
            pull_request.clone(),
            body.to_string(),
            comments.to_vec(),
            commit_sha.to_string(),
        ));
        Ok(())
    }
}

#[derive(Clone)]
struct NamedTracker {
    component: String,
    issues: Arc<Mutex<HashMap<String, Issue>>>,
}

impl NamedTracker {
    fn new(component: impl Into<String>, issues: Vec<Issue>) -> Self {
        Self {
            component: component.into(),
            issues: Arc::new(Mutex::new(
                issues
                    .into_iter()
                    .map(|issue| (issue.id.clone(), issue))
                    .collect(),
            )),
        }
    }
}

#[async_trait]
impl IssueTracker for NamedTracker {
    fn component_key(&self) -> String {
        self.component.clone()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(self.issues.lock().unwrap().values().cloned().collect())
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        let normalized = states
            .iter()
            .map(|state| state.to_ascii_lowercase())
            .collect::<Vec<_>>();
        Ok(self
            .issues
            .lock()
            .unwrap()
            .values()
            .filter(|issue| normalized.contains(&issue.state.to_ascii_lowercase()))
            .cloned()
            .collect())
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, polyphony_core::Error> {
        let issues = self.issues.lock().unwrap();
        Ok(issue_ids
            .iter()
            .filter_map(|issue_id| issues.get(issue_id))
            .map(|issue| IssueStateUpdate {
                id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                state: issue.state.clone(),
                updated_at: issue.updated_at,
            })
            .collect())
    }
}

#[derive(Clone)]
struct NamedAgent {
    component: String,
}

impl NamedAgent {
    fn new(component: impl Into<String>) -> Self {
        Self {
            component: component.into(),
        }
    }
}

#[async_trait]
impl AgentRuntime for NamedAgent {
    fn component_key(&self) -> String {
        self.component.clone()
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Ok(AgentRunResult::succeeded(1))
    }
}

#[derive(Clone, Default)]
struct RecordingSessionAgent {
    prompts: Arc<Mutex<Vec<String>>>,
    session_starts: Arc<Mutex<u32>>,
    stops: Arc<Mutex<u32>>,
}

impl RecordingSessionAgent {
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }

    fn session_starts(&self) -> u32 {
        *self.session_starts.lock().unwrap()
    }

    fn stops(&self) -> u32 {
        *self.stops.lock().unwrap()
    }
}

struct RecordingSession {
    prompts: Arc<Mutex<Vec<String>>>,
    stops: Arc<Mutex<u32>>,
}

#[async_trait]
impl AgentSession for RecordingSession {
    async fn run_turn(&mut self, prompt: String) -> Result<AgentRunResult, polyphony_core::Error> {
        self.prompts.lock().unwrap().push(prompt);
        Ok(AgentRunResult::succeeded(1))
    }

    async fn stop(&mut self) -> Result<(), polyphony_core::Error> {
        *self.stops.lock().unwrap() += 1;
        Ok(())
    }
}

#[async_trait]
impl AgentRuntime for RecordingSessionAgent {
    fn component_key(&self) -> String {
        "provider:session-test".into()
    }

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, polyphony_core::Error> {
        *self.session_starts.lock().unwrap() += 1;
        Ok(Some(Box::new(RecordingSession {
            prompts: self.prompts.clone(),
            stops: self.stops.clone(),
        })))
    }

    async fn run(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(
            "run() should not be used when live sessions are available".into(),
        ))
    }
}

#[derive(Clone)]
struct SequencedPullRequestTriggerSource {
    batches: Arc<Mutex<VecDeque<Vec<PullRequestTrigger>>>>,
}

impl SequencedPullRequestTriggerSource {
    fn new(batches: Vec<Vec<PullRequestTrigger>>) -> Self {
        Self {
            batches: Arc::new(Mutex::new(batches.into())),
        }
    }
}

#[async_trait]
impl PullRequestTriggerSource for SequencedPullRequestTriggerSource {
    fn component_key(&self) -> String {
        "github:test-pr-triggers".into()
    }

    async fn fetch_triggers(&self) -> Result<Vec<PullRequestTrigger>, polyphony_core::Error> {
        Ok(self.batches.lock().unwrap().pop_front().unwrap_or_default())
    }
}

struct SequencedStateTracker {
    issue: Issue,
    states: Arc<Mutex<VecDeque<String>>>,
}

impl SequencedStateTracker {
    fn new(issue: Issue, states: Vec<&str>) -> Self {
        Self {
            issue,
            states: Arc::new(Mutex::new(states.into_iter().map(str::to_string).collect())),
        }
    }
}

#[async_trait]
impl IssueTracker for SequencedStateTracker {
    fn component_key(&self) -> String {
        "tracker:sequence".into()
    }

    async fn fetch_candidate_issues(
        &self,
        _query: &polyphony_core::TrackerQuery,
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(vec![self.issue.clone()])
    }

    async fn fetch_issues_by_states(
        &self,
        _project_slug: Option<&str>,
        _states: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        Ok(vec![self.issue.clone()])
    }

    async fn fetch_issues_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<Issue>, polyphony_core::Error> {
        if issue_ids.iter().any(|issue_id| issue_id == &self.issue.id) {
            Ok(vec![self.issue.clone()])
        } else {
            Ok(Vec::new())
        }
    }

    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, polyphony_core::Error> {
        if !issue_ids.iter().any(|issue_id| issue_id == &self.issue.id) {
            return Ok(Vec::new());
        }
        let state = self
            .states
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.issue.state.clone());
        Ok(vec![IssueStateUpdate {
            id: self.issue.id.clone(),
            identifier: self.issue.identifier.clone(),
            state,
            updated_at: self.issue.updated_at,
        }])
    }
}

#[derive(Clone, Default)]
struct RecordingProvisioner {
    cleaned: Arc<Mutex<Vec<String>>>,
}

impl RecordingProvisioner {
    fn cleaned_issue_identifiers(&self) -> Vec<String> {
        self.cleaned.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkspaceProvisioner for RecordingProvisioner {
    fn component_key(&self) -> String {
        "workspace:test".into()
    }

    async fn ensure_workspace(
        &self,
        request: WorkspaceRequest,
    ) -> Result<Workspace, polyphony_core::Error> {
        tokio::fs::create_dir_all(&request.workspace_path)
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        Ok(Workspace {
            path: request.workspace_path,
            workspace_key: request.workspace_key,
            created_now: false,
            branch_name: request.branch_name,
        })
    }

    async fn cleanup_workspace(
        &self,
        request: WorkspaceRequest,
    ) -> Result<(), polyphony_core::Error> {
        self.cleaned
            .lock()
            .unwrap()
            .push(request.issue_identifier.clone());
        if tokio::fs::try_exists(&request.workspace_path)
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?
        {
            tokio::fs::remove_dir_all(&request.workspace_path)
                .await
                .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FailingProvisioner {
    message: String,
}

#[async_trait]
impl WorkspaceProvisioner for FailingProvisioner {
    fn component_key(&self) -> String {
        "workspace:failing".into()
    }

    async fn ensure_workspace(
        &self,
        _request: WorkspaceRequest,
    ) -> Result<Workspace, polyphony_core::Error> {
        Err(polyphony_core::Error::Adapter(self.message.clone()))
    }

    async fn cleanup_workspace(
        &self,
        _request: WorkspaceRequest,
    ) -> Result<(), polyphony_core::Error> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct ScriptedPipelineAgent {
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl ScriptedPipelineAgent {
    fn recorded_agent_names(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(agent_name, _)| agent_name.clone())
            .collect()
    }

    fn recorded_calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentRuntime for ScriptedPipelineAgent {
    fn component_key(&self) -> String {
        "provider:scripted-pipeline".into()
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, polyphony_core::Error> {
        self.calls
            .lock()
            .unwrap()
            .push((spec.agent.name.clone(), spec.prompt.clone()));
        let polyphony_dir = spec.workspace_path.join(".polyphony");
        tokio::fs::create_dir_all(&polyphony_dir)
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;

        if spec.agent.name == "router" {
            let plan = json!({
                "tasks": [{
                    "title": "Create the missing file",
                    "category": "coding",
                    "description": "Add the repository marker file requested by the issue.",
                    "agent": "implementer"
                }]
            });
            tokio::fs::write(polyphony_dir.join("plan.json"), plan.to_string())
                .await
                .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
            return Ok(AgentRunResult::succeeded(1));
        }

        if spec.prompt.contains("Review the current branch against") {
            tokio::fs::write(
                polyphony_dir.join("review.md"),
                "Summary\n\nAutomated review found no blockers.",
            )
            .await
            .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;
            return Ok(AgentRunResult::succeeded(1));
        }

        tokio::fs::write(
            spec.workspace_path.join("e2e-pr.txt"),
            "polyphony end-to-end dogfood\n",
        )
        .await
        .map_err(|error| polyphony_core::Error::Adapter(error.to_string()))?;

        Ok(AgentRunResult {
            status: AttemptStatus::Succeeded,
            turns_completed: 1,
            error: None,
            final_issue_state: Some("Done".into()),
        })
    }
}

#[derive(Clone)]
struct RecordingCommitter {
    requests: Arc<Mutex<Vec<WorkspaceCommitRequest>>>,
    result: Option<WorkspaceCommitResult>,
}

impl RecordingCommitter {
    fn new(result: Option<WorkspaceCommitResult>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            result,
        }
    }

    fn requests(&self) -> Vec<WorkspaceCommitRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkspaceCommitter for RecordingCommitter {
    fn component_key(&self) -> String {
        "git:test-committer".into()
    }

    async fn commit_and_push(
        &self,
        request: &WorkspaceCommitRequest,
    ) -> Result<Option<WorkspaceCommitResult>, polyphony_core::Error> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.result.clone())
    }
}

#[derive(Clone)]
struct RecordingPullRequestManager {
    requests: Arc<Mutex<Vec<PullRequestRequest>>>,
    ensured_pull_request: PullRequestRef,
}

impl RecordingPullRequestManager {
    fn new(ensured_pull_request: PullRequestRef) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            ensured_pull_request,
        }
    }

    fn requests(&self) -> Vec<PullRequestRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl PullRequestManager for RecordingPullRequestManager {
    fn component_key(&self) -> String {
        "github:test-prs".into()
    }

    async fn ensure_pull_request(
        &self,
        request: &PullRequestRequest,
    ) -> Result<PullRequestRef, polyphony_core::Error> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.ensured_pull_request.clone())
    }

    async fn merge_pull_request(
        &self,
        _pull_request: &PullRequestRef,
    ) -> Result<(), polyphony_core::Error> {
        Ok(())
    }
}

fn test_workflow(workspace_root: &Path) -> LoadedWorkflow {
    test_workflow_with_front_matter(
        workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\norchestration:\n  dispatch_mode: manual\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
    )
}

fn pipeline_workflow_with_automation(workspace_root: &Path) -> LoadedWorkflow {
    test_workflow_with_front_matter(
        workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: test-token\n  active_states: [Todo, In Progress]\n  terminal_states: [Done]\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\n  checkout_kind: linked_worktree\n  source_repo_path: __ROOT__/source-repo\nagent:\n  max_turns: 3\norchestration:\n  dispatch_mode: manual\n  router_agent: router\nagents:\n  default: implementer\n  profiles:\n    router:\n      kind: mock\n      transport: mock\n      command: mock\n    implementer:\n      kind: mock\n      transport: mock\n      command: mock\nautomation:\n  enabled: true\n  git:\n    remote_name: origin\n---\nFix {{ issue.identifier }}\n",
    )
}

fn test_workflow_with_front_matter(workspace_root: &Path, raw: &str) -> LoadedWorkflow {
    let workflow_path = workspace_root.join("WORKFLOW.md");
    fs::create_dir_all(workspace_root).unwrap();
    let raw = raw.replace("__ROOT__", &workspace_root.display().to_string());
    fs::write(&workflow_path, raw).unwrap();
    load_workflow(&workflow_path).unwrap()
}

fn test_service(
    tracker: TestTracker,
    provisioner: RecordingProvisioner,
    workspace_root: &Path,
) -> RuntimeService {
    let workflow = test_workflow(workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0
}

fn test_service_with_reload(
    workflow: LoadedWorkflow,
    tracker: Arc<dyn IssueTracker>,
    agent: Arc<dyn AgentRuntime>,
    provisioner: RecordingProvisioner,
    component_factory: Arc<RuntimeComponentFactory>,
) -> RuntimeService {
    let (tx, rx) = watch::channel(workflow.clone());
    RuntimeService::new(
        tracker,
        None,
        agent,
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0
    .with_workflow_reload(workflow.path.clone(), None, tx, component_factory)
}

fn sample_issue(issue_id: &str, identifier: &str, state: &str, title: &str) -> Issue {
    Issue {
        id: issue_id.to_string(),
        identifier: identifier.to_string(),
        title: title.to_string(),
        description: Some(format!("Description for {title}")),
        priority: Some(1),
        state: state.to_string(),
        branch_name: Some(format!("task/{}", identifier.to_ascii_lowercase())),
        labels: vec!["test".into()],
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
        ..Issue::default()
    }
}

fn sample_pull_request_comment_trigger() -> PullRequestCommentTrigger {
    let now = Utc::now();
    PullRequestCommentTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        pull_request_title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42#discussion_r1".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        thread_id: "thread-1".into(),
        comment_id: "comment-1".into(),
        path: "crates/core/src/lib.rs".into(),
        line: Some(42),
        body: "Please fix this branch.".into(),
        author_login: Some("greptileai".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(now - chrono::Duration::minutes(5)),
        updated_at: Some(now - chrono::Duration::minutes(2)),
        is_draft: false,
    }
}

fn sample_pull_request_conflict_trigger() -> PullRequestConflictTrigger {
    let now = Utc::now();
    PullRequestConflictTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 43,
        pull_request_title: "Merge me".into(),
        url: Some("https://github.com/penso/polyphony/pull/43".into()),
        base_branch: "main".into(),
        head_branch: "feature/conflict".into(),
        head_sha: "def456".into(),
        checkout_ref: Some("refs/pull/43/head".into()),
        author_login: Some("alice".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(now - chrono::Duration::minutes(10)),
        updated_at: Some(now - chrono::Duration::minutes(3)),
        is_draft: false,
        mergeable_state: "conflicting".into(),
        merge_state_status: "dirty".into(),
    }
}

fn make_running_task(issue: Issue, workspace_path: PathBuf) -> RunningTask {
    RunningTask {
        issue,
        agent_name: "mock".into(),
        model: None,
        attempt: None,
        workspace_path,
        stall_timeout_ms: 300_000,
        max_turns: 5,
        started_at: Utc::now(),
        session_id: None,
        thread_id: None,
        turn_id: None,
        codex_app_server_pid: None,
        last_event: None,
        last_message: None,
        last_event_at: None,
        tokens: TokenUsage::default(),
        last_reported_tokens: TokenUsage::default(),
        turn_count: 0,
        rate_limits: None,
        active_task_id: None,
        movement_id: None,
        review_target: None,
        review_comment_marker: None,
        recent_log: VecDeque::new(),
        handle: tokio::spawn(async {
            let _: () = std::future::pending().await;
        }),
    }
}

fn unique_workspace_root(test_name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "polyphony-orchestrator-{test_name}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

async fn handle_next_worker_message(service: &mut RuntimeService) {
    let message = timeout(Duration::from_secs(5), service.command_rx.recv())
        .await
        .expect("timed out waiting for orchestrator worker message")
        .expect("orchestrator command channel closed");
    service.handle_message(message).await.unwrap();
}

#[tokio::test]
async fn reconcile_running_releases_missing_issue() {
    let workspace_root = unique_workspace_root("missing");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-1", "FAC-1", "Todo", "Old");
    let workspace_path = workspace_root.join("FAC-1");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert!(!service.is_claimed(&issue.id));
}

#[tokio::test]
async fn reconcile_running_preserves_synthetic_pr_review_issue() {
    let workspace_root = unique_workspace_root("synthetic-pr");
    let provisioner = RecordingProvisioner::default();
    // Empty tracker — no issues will be returned by fetch_issues_by_ids.
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let synthetic_id = "pr_review:github:penso/arbor:89:abc123";
    let issue = Issue {
        id: synthetic_id.to_string(),
        identifier: "penso/arbor#89".into(),
        title: "Review PR #89: bump rustls-webpki".into(),
        state: "Review".into(),
        ..Issue::default()
    };
    let workspace_path = workspace_root.join("penso_arbor_89");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    // Synthetic issue must NOT be stopped — it has no tracker-side state.
    assert!(
        service.state.running.contains_key(synthetic_id),
        "synthetic PR review issue should survive reconciliation"
    );
    assert!(service.is_claimed(synthetic_id));
}

#[tokio::test]
async fn reconcile_running_preserves_synthetic_pr_comment_issue() {
    let workspace_root = unique_workspace_root("synthetic-comment");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let synthetic_id = "pr_comment:github:penso/arbor:42:thread-1";
    let issue = Issue {
        id: synthetic_id.to_string(),
        identifier: "penso/arbor#42".into(),
        title: "Comment on PR #42".into(),
        state: "Review".into(),
        ..Issue::default()
    };
    let workspace_path = workspace_root.join("penso_arbor_42");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );

    service.reconcile_running().await;

    assert!(
        service.state.running.contains_key(synthetic_id),
        "synthetic PR comment issue should survive reconciliation"
    );
}

#[tokio::test]
async fn reconcile_running_preserves_session_for_non_terminal_state() {
    let workspace_root = unique_workspace_root("non-terminal-state");
    let provisioner = RecordingProvisioner::default();
    // Tracker returns the issue with state "Open" — not in active_states ("Todo",
    // "In Progress") or terminal_states ("Done", "Closed", "Cancelled").
    // Reconciliation must NOT cancel it: only explicit terminal states stop work.
    let tracker_issue = sample_issue("issue-5", "FAC-5", "Open", "GitHub-style issue");
    let mut service = test_service(
        TestTracker::new(vec![tracker_issue.clone()]),
        provisioner,
        &workspace_root,
    );
    let running_issue = sample_issue("issue-5", "FAC-5", "Open", "GitHub-style issue");
    let workspace_path = workspace_root.join("FAC-5");
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue.clone(), workspace_path),
    );
    service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

    service.state.movements.insert("mov-5".into(), Movement {
        id: "mov-5".into(),
        kind: MovementKind::IssueDelivery,
        issue_id: Some("issue-5".into()),
        issue_identifier: Some("FAC-5".into()),
        title: "GitHub-style issue".into(),
        status: MovementStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
    });

    service.reconcile_running().await;

    // Session must survive — "Open" is not terminal.
    assert!(
        service.state.running.contains_key(&running_issue.id),
        "session with non-terminal state 'Open' must NOT be cancelled by reconciliation"
    );
    assert!(service.is_claimed(&running_issue.id));
    // Movement must remain in progress.
    let movement = service.state.movements.get("mov-5").unwrap();
    assert_eq!(movement.status, MovementStatus::InProgress);
    assert!(
        movement.cancel_reason.is_none(),
        "cancel_reason must be None for non-terminal state"
    );
}

#[tokio::test]
async fn reconcile_running_sets_cancel_reason_for_missing_issue() {
    let workspace_root = unique_workspace_root("missing-reason");
    let provisioner = RecordingProvisioner::default();
    // Empty tracker — issue will not be found.
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-6", "FAC-6", "Todo", "Vanished issue");
    let workspace_path = workspace_root.join("FAC-6");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    // Add a movement so stop_running can set cancel_reason on it.
    service.state.movements.insert("mov-6".into(), Movement {
        id: "mov-6".into(),
        kind: MovementKind::IssueDelivery,
        issue_id: Some("issue-6".into()),
        issue_identifier: Some("FAC-6".into()),
        title: "Vanished issue".into(),
        status: MovementStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
    });

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&issue.id));
    let movement = service.state.movements.get("mov-6").unwrap();
    assert_eq!(movement.status, MovementStatus::Cancelled);
    assert!(
        movement.cancel_reason.is_some(),
        "cancel_reason must be set for missing issues"
    );
    let reason = movement.cancel_reason.as_deref().unwrap();
    assert!(
        reason.contains("no longer found"),
        "cancel_reason should explain the issue is missing, got: {reason}"
    );
}

#[tokio::test]
async fn reconcile_running_sets_cancel_reason_for_terminal_state() {
    let workspace_root = unique_workspace_root("terminal-reason");
    let provisioner = RecordingProvisioner::default();
    let tracker_issue = sample_issue("issue-7", "FAC-7", "Done", "Finished issue");
    let mut service = test_service(
        TestTracker::new(vec![tracker_issue.clone()]),
        provisioner,
        &workspace_root,
    );
    let running_issue = sample_issue("issue-7", "FAC-7", "Todo", "Finished issue");
    let workspace_path = workspace_root.join("FAC-7");
    fs::create_dir_all(&workspace_path).unwrap();
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue.clone(), workspace_path),
    );
    service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

    service.state.movements.insert("mov-7".into(), Movement {
        id: "mov-7".into(),
        kind: MovementKind::IssueDelivery,
        issue_id: Some("issue-7".into()),
        issue_identifier: Some("FAC-7".into()),
        title: "Finished issue".into(),
        status: MovementStatus::InProgress,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: None,
        workspace_path: None,
        review_target: None,
        deliverable: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
    });

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&running_issue.id));
    let movement = service.state.movements.get("mov-7").unwrap();
    assert_eq!(movement.status, MovementStatus::Cancelled);
    assert!(
        movement.cancel_reason.is_some(),
        "cancel_reason must be set for terminal state"
    );
    let reason = movement.cancel_reason.as_deref().unwrap();
    assert!(
        reason.contains("terminal"),
        "cancel_reason should mention terminal state, got: {reason}"
    );
}

#[tokio::test]
async fn tick_tracks_visible_issues_when_no_agents_are_configured() {
    let workspace_root = unique_workspace_root("visible-issues");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  by_state: {}\n  by_label: {}\n  profiles: {}\n---\nTest prompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let tracker = TestTracker::new(vec![
        sample_issue("issue-1", "FAC-1", "Todo", "First"),
        sample_issue("issue-2", "FAC-2", "In Progress", "Second"),
    ]);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service.tick().await;

    let snapshot = service.snapshot();
    let visible = snapshot
        .visible_issues
        .iter()
        .map(|issue| issue.issue_identifier.as_str())
        .collect::<Vec<_>>();

    assert_eq!(visible, vec!["FAC-1", "FAC-2"]);
    assert!(snapshot.running.is_empty());
}

#[tokio::test]
async fn disappearing_issues_become_already_fixed_triggers() {
    let workspace_root = unique_workspace_root("discarded-issue");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let tracker_handle = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    service.tick().await;
    tracker_handle.issues.lock().unwrap().clear();
    service.tick().await;

    let snapshot = service.snapshot();
    let discarded = snapshot
        .visible_triggers
        .iter()
        .find(|trigger| trigger.trigger_id == "issue-1")
        .expect("missing discarded issue trigger");
    assert_eq!(discarded.kind, VisibleTriggerKind::Issue);
    assert_eq!(discarded.status, "already_fixed");
}

#[tokio::test]
async fn idle_mode_dispatches_when_budget_has_headroom() {
    let workspace_root = unique_workspace_root("idle-budget-headroom");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Idle;
    service
        .state
        .budgets
        .insert("agent:mock".into(), BudgetSnapshot {
            component: "agent:mock".into(),
            captured_at: Utc::now(),
            credits_remaining: Some(12.0),
            credits_total: Some(20.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: Some(json!({ "weekly_deficit": 0 })),
        });

    service.tick().await;

    assert!(service.state.running.contains_key("issue-1"));
}

#[tokio::test]
async fn idle_mode_skips_dispatch_when_weekly_budget_is_underwater() {
    let workspace_root = unique_workspace_root("idle-weekly-deficit");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Idle;
    service
        .state
        .budgets
        .insert("agent:mock".into(), BudgetSnapshot {
            component: "agent:mock".into(),
            captured_at: Utc::now(),
            credits_remaining: Some(12.0),
            credits_total: Some(20.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: Some(json!({ "weekly": { "deficit": 1 } })),
        });

    service.tick().await;

    assert!(!service.state.running.contains_key("issue-1"));
}

#[tokio::test]
async fn idle_mode_only_dispatches_when_no_other_work_is_running() {
    let workspace_root = unique_workspace_root("idle-busy");
    let tracker = TestTracker::new(vec![
        sample_issue("issue-1", "FAC-1", "Todo", "First"),
        sample_issue("issue-2", "FAC-2", "In Progress", "Second"),
    ]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner.clone(), &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Idle;
    service
        .state
        .budgets
        .insert("agent:mock".into(), BudgetSnapshot {
            component: "agent:mock".into(),
            captured_at: Utc::now(),
            credits_remaining: Some(12.0),
            credits_total: Some(20.0),
            spent_usd: None,
            soft_limit_usd: None,
            hard_limit_usd: None,
            reset_at: None,
            raw: Some(json!({ "weekly_remaining": 3 })),
        });
    let running_issue = sample_issue("issue-2", "FAC-2", "In Progress", "Second");
    let workspace_path = workspace_root.join(sanitize_workspace_key(&running_issue.identifier));
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue, workspace_path),
    );

    service.tick().await;

    assert!(!service.state.running.contains_key("issue-1"));
    assert!(service.state.running.contains_key("issue-2"));
}

#[tokio::test]
async fn completed_pull_request_reviews_are_marked_reviewed_and_not_redispatched() {
    let workspace_root = unique_workspace_root("pr-review");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        author_login: Some("alice".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    let commenter = RecordingPullRequestCommenter::default();
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        Some(Arc::new(commenter.clone())),
        None,
        None,
        None,
        rx,
    )
    .0;
    let issue = synthetic_issue_for_pull_request_review(&trigger);
    let workspace_path = workspace_root.join(sanitize_workspace_key(&issue.identifier));
    tokio::fs::create_dir_all(workspace_path.join(".polyphony"))
        .await
        .unwrap();
    tokio::fs::write(
        workspace_path.join(".polyphony").join("review.md"),
        "Summary\n\nReviewed penso/polyphony#42",
    )
    .await
    .unwrap();
    service
        .state
        .movements
        .insert("mov-review".into(), Movement {
            id: "mov-review".into(),
            kind: MovementKind::PullRequestReview,
            issue_id: Some(issue.id.clone()),
            issue_identifier: Some(issue.identifier.clone()),
            title: trigger.title.clone(),
            status: MovementStatus::InProgress,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
            workspace_path: Some(workspace_path.clone()),
            review_target: Some(trigger.review_target()),
            deliverable: None,
            created_at: Utc::now(),
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: Utc::now(),
        });
    let running = RunningTask {
        issue: issue.clone(),
        agent_name: "reviewer".into(),
        model: None,
        attempt: None,
        workspace_path,
        stall_timeout_ms: 300_000,
        max_turns: 4,
        started_at: Utc::now(),
        session_id: None,
        thread_id: None,
        turn_id: None,
        codex_app_server_pid: None,
        last_event: None,
        last_message: None,
        last_event_at: None,
        tokens: TokenUsage::default(),
        last_reported_tokens: TokenUsage::default(),
        turn_count: 0,
        rate_limits: None,
        active_task_id: None,
        movement_id: Some("mov-review".into()),
        review_target: Some(trigger.review_target()),
        review_comment_marker: Some(pull_request_review_comment_marker(&trigger.review_target())),
        recent_log: VecDeque::new(),
        handle: tokio::spawn(async {
            let _: () = std::future::pending().await;
        }),
    };
    service
        .finish_pull_request_review(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            running,
            AgentRunResult::succeeded(1),
        )
        .await
        .unwrap();

    let comment_bodies = commenter.comment_bodies();
    assert_eq!(comment_bodies.len(), 1);
    assert!(comment_bodies[0].contains("Summary"));
    assert!(comment_bodies[0].contains("polyphony:pr-review"));
    assert!(
        service
            .state
            .reviewed_pull_request_heads
            .contains_key(&trigger.dedupe_key())
    );
    assert_eq!(
        service.pull_request_trigger_suppression(
            &service.workflow(),
            &PullRequestTrigger::Review(trigger.clone()),
        ),
        Some(ReviewTriggerSuppression::AlreadyReviewed)
    );
    // Verify deliverable was created for the PR review movement.
    let movement = service.state.movements.get("mov-review").unwrap();
    let deliverable = movement
        .deliverable
        .as_ref()
        .expect("deliverable should be set for PR review");
    assert_eq!(deliverable.kind, DeliverableKind::PullRequestReview);
    assert_eq!(deliverable.status, DeliverableStatus::Reviewed);
    assert!(
        deliverable
            .description
            .as_ref()
            .unwrap()
            .contains("Summary")
    );

    tokio::fs::write(
        workspace_root
            .join(sanitize_workspace_key(&issue.identifier))
            .join(".polyphony")
            .join("review.md"),
        "Summary\n\nUpdated review body",
    )
    .await
    .unwrap();
    service
        .post_pull_request_review_comment(
            &RunningTask {
                issue,
                agent_name: "reviewer".into(),
                model: None,
                attempt: None,
                workspace_path: workspace_root
                    .join(sanitize_workspace_key(&trigger.display_identifier())),
                stall_timeout_ms: 300_000,
                max_turns: 4,
                started_at: Utc::now(),
                session_id: None,
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                last_event: None,
                last_message: None,
                last_event_at: None,
                tokens: TokenUsage::default(),
                last_reported_tokens: TokenUsage::default(),
                turn_count: 0,
                rate_limits: None,
                active_task_id: None,
                movement_id: Some("mov-review".into()),
                review_target: Some(trigger.review_target()),
                review_comment_marker: Some(pull_request_review_comment_marker(
                    &trigger.review_target(),
                )),
                recent_log: VecDeque::new(),
                handle: tokio::spawn(async {
                    let _: () = std::future::pending().await;
                }),
            },
            &trigger.review_target(),
        )
        .await
        .unwrap();
    let comment_bodies = commenter.comment_bodies();
    assert_eq!(comment_bodies.len(), 1);
    assert!(comment_bodies[0].contains("Updated review body"));
}

#[test]
fn review_trigger_suppression_respects_authors_labels_and_bots() {
    let workspace_root = unique_workspace_root("pr-review-suppression");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n    only_labels: [ready]\n    ignore_labels: [wip]\n    ignore_authors: [skip-me]\n    ignore_bot_authors: true\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let workflow = service.workflow();

    let base_trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 1,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/1".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "sha1".into(),
        checkout_ref: Some("refs/pull/1/head".into()),
        author_login: Some("skip-me".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    assert_eq!(
        service.pull_request_trigger_suppression(
            &workflow,
            &PullRequestTrigger::Review(base_trigger.clone()),
        ),
        Some(ReviewTriggerSuppression::IgnoredAuthor {
            author: "skip-me".into()
        })
    );

    let bot_trigger = PullRequestReviewTrigger {
        number: 2,
        head_sha: "sha2".into(),
        checkout_ref: Some("refs/pull/2/head".into()),
        author_login: Some("dependabot[bot]".into()),
        ..base_trigger.clone()
    };
    assert_eq!(
        service.pull_request_trigger_suppression(
            &workflow,
            &PullRequestTrigger::Review(bot_trigger.clone()),
        ),
        Some(ReviewTriggerSuppression::BotAuthor {
            author: "dependabot[bot]".into()
        })
    );

    let ignored_label_trigger = PullRequestReviewTrigger {
        number: 3,
        head_sha: "sha3".into(),
        checkout_ref: Some("refs/pull/3/head".into()),
        author_login: Some("alice".into()),
        labels: vec!["wip".into()],
        ..base_trigger.clone()
    };
    assert_eq!(
        service.pull_request_trigger_suppression(
            &workflow,
            &PullRequestTrigger::Review(ignored_label_trigger.clone()),
        ),
        Some(ReviewTriggerSuppression::IgnoredLabel {
            label: "wip".into()
        })
    );

    let missing_label_trigger = PullRequestReviewTrigger {
        number: 4,
        head_sha: "sha4".into(),
        checkout_ref: Some("refs/pull/4/head".into()),
        author_login: Some("alice".into()),
        labels: vec!["backend".into()],
        ..base_trigger
    };
    assert_eq!(
        service.pull_request_trigger_suppression(
            &workflow,
            &PullRequestTrigger::Review(missing_label_trigger),
        ),
        Some(ReviewTriggerSuppression::MissingLabels {
            labels: vec!["ready".into()]
        })
    );
}

#[tokio::test]
async fn untrusted_pull_request_triggers_require_manual_approval() {
    let workspace_root = unique_workspace_root("pr-review-approval");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 9,
        title: "Review me carefully".into(),
        url: Some("https://github.com/penso/polyphony/pull/9".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "sha9".into(),
        checkout_ref: Some("refs/pull/9/head".into()),
        author_login: Some("outsider".into()),
        approval_state: IssueApprovalState::Waiting,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    service
        .state
        .visible_review_triggers
        .insert(trigger.dedupe_key(), trigger.clone());

    assert_eq!(
        service.pull_request_trigger_suppression(
            &service.workflow(),
            &PullRequestTrigger::Review(trigger.clone()),
        ),
        Some(ReviewTriggerSuppression::AwaitingApproval)
    );

    service
        .pending_issue_approvals
        .push((trigger.dedupe_key(), "github".into()));
    service.process_pending_issue_approvals().await;

    assert_eq!(
        service.pull_request_trigger_approval_state(&PullRequestTrigger::Review(trigger.clone())),
        IssueApprovalState::Approved
    );
    let approved = service
        .snapshot()
        .visible_triggers
        .into_iter()
        .find(|row| row.trigger_id == trigger.dedupe_key())
        .expect("missing visible trigger after approval");
    assert_eq!(approved.approval_state, IssueApprovalState::Approved);
}

#[test]
fn pull_request_comment_triggers_are_suppressed_after_a_newer_review() {
    let workspace_root = unique_workspace_root("pr-comment-suppression");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let workflow = service.workflow();
    let now = Utc::now();
    let trigger = PullRequestCommentTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        pull_request_title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42#discussion_r1".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        thread_id: "thread-1".into(),
        comment_id: "comment-1".into(),
        path: "crates/core/src/lib.rs".into(),
        line: Some(42),
        body: "Please fix this branch.".into(),
        author_login: Some("greptileai".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(now - chrono::Duration::minutes(5)),
        updated_at: Some(now - chrono::Duration::minutes(2)),
        is_draft: false,
    };
    service.state.reviewed_pull_request_heads.insert(
        review_target_key(&trigger.review_target()),
        ReviewedPullRequestHead {
            key: review_target_key(&trigger.review_target()),
            target: trigger.review_target(),
            reviewed_at: now - chrono::Duration::minutes(1),
            movement_id: None,
        },
    );

    assert_eq!(
        service.pull_request_trigger_suppression(
            &workflow,
            &PullRequestTrigger::Comment(trigger.clone()),
        ),
        Some(ReviewTriggerSuppression::AlreadyReviewed)
    );

    service.state.reviewed_pull_request_heads.insert(
        review_target_key(&trigger.review_target()),
        ReviewedPullRequestHead {
            key: review_target_key(&trigger.review_target()),
            target: trigger.review_target(),
            reviewed_at: now - chrono::Duration::minutes(3),
            movement_id: None,
        },
    );

    assert!(matches!(
        service.pull_request_trigger_suppression(
            &workflow,
            &PullRequestTrigger::Comment(trigger),
        ),
        Some(ReviewTriggerSuppression::Debounced { remaining_seconds })
            if remaining_seconds > 0 && remaining_seconds <= 180
    ));
}

#[tokio::test]
async fn disappearing_pr_comment_triggers_become_already_fixed() {
    let workspace_root = unique_workspace_root("discarded-pr-comment");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let source = SequencedPullRequestTriggerSource::new(vec![
        vec![PullRequestTrigger::Comment(
            sample_pull_request_comment_trigger(),
        )],
        Vec::new(),
    ]);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        Some(Arc::new(source)),
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    service.state.dispatch_mode = polyphony_core::DispatchMode::Manual;

    service.tick().await;
    service.tick().await;

    let snapshot = service.snapshot();
    let discarded = snapshot
        .visible_triggers
        .iter()
        .find(|trigger| trigger.kind == VisibleTriggerKind::PullRequestComment)
        .expect("missing discarded pr comment trigger");
    assert_eq!(discarded.identifier, "penso/polyphony#42");
    assert_eq!(discarded.status, "already_fixed");
}

#[tokio::test]
async fn conflict_triggers_become_already_fixed_without_retry_churn() {
    let workspace_root = unique_workspace_root("pr-conflict-visible");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let source = SequencedPullRequestTriggerSource::new(vec![
        vec![PullRequestTrigger::Conflict(
            sample_pull_request_conflict_trigger(),
        )],
        Vec::new(),
    ]);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        Some(Arc::new(source)),
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service.tick().await;

    let snapshot = service.snapshot();
    let conflict = snapshot
        .visible_triggers
        .iter()
        .find(|trigger| trigger.kind == VisibleTriggerKind::PullRequestConflict)
        .expect("missing conflict trigger");
    assert_eq!(conflict.status, "ready");
    assert!(service.state.retrying.is_empty());
    assert!(service.state.running.is_empty());

    service.tick().await;

    let snapshot = service.snapshot();
    let discarded = snapshot
        .visible_triggers
        .iter()
        .find(|trigger| trigger.kind == VisibleTriggerKind::PullRequestConflict)
        .expect("missing discarded conflict trigger");
    assert_eq!(discarded.status, "already_fixed");
    assert!(service.state.retrying.is_empty());
}

#[tokio::test]
async fn inline_pull_request_review_comments_are_submitted_when_requested() {
    let workspace_root = unique_workspace_root("pr-review-inline");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n    comment_mode: inline\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
    let trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 42,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/42".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/42/head".into()),
        author_login: Some("alice".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    let commenter = RecordingPullRequestCommenter::default();
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        Some(Arc::new(commenter.clone())),
        None,
        None,
        None,
        rx,
    )
    .0;
    let issue = synthetic_issue_for_pull_request_review(&trigger);
    let workspace_path = workspace_root.join(sanitize_workspace_key(&issue.identifier));
    tokio::fs::create_dir_all(workspace_path.join(".polyphony"))
        .await
        .unwrap();
    tokio::fs::write(
        workspace_path.join(".polyphony").join("review.md"),
        "Summary\n\nNeeds fixes",
    )
    .await
    .unwrap();
    tokio::fs::write(
        workspace_path
            .join(".polyphony")
            .join("review-comments.json"),
        r#"[{"path":"crates/core/src/lib.rs","line":42,"body":"Fix this branch."}]"#,
    )
    .await
    .unwrap();

    service
        .post_pull_request_review_comment(
            &RunningTask {
                issue,
                agent_name: "reviewer".into(),
                model: None,
                attempt: None,
                workspace_path,
                stall_timeout_ms: 300_000,
                max_turns: 4,
                started_at: Utc::now(),
                session_id: None,
                thread_id: None,
                turn_id: None,
                codex_app_server_pid: None,
                last_event: None,
                last_message: None,
                last_event_at: None,
                tokens: TokenUsage::default(),
                last_reported_tokens: TokenUsage::default(),
                turn_count: 0,
                rate_limits: None,
                active_task_id: None,
                movement_id: Some("mov-inline".into()),
                review_target: Some(trigger.review_target()),
                review_comment_marker: Some(pull_request_review_comment_marker(
                    &trigger.review_target(),
                )),
                recent_log: VecDeque::new(),
                handle: tokio::spawn(async {
                    let _: () = std::future::pending().await;
                }),
            },
            &trigger.review_target(),
        )
        .await
        .unwrap();

    assert!(commenter.comment_bodies().is_empty());
    let reviews = commenter.reviews();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].2.len(), 1);
    assert_eq!(reviews[0].2[0].path, "crates/core/src/lib.rs");
    assert_eq!(reviews[0].2[0].line, 42);
    assert_eq!(reviews[0].3, "abc123");
}

#[tokio::test]
async fn orphan_auto_dispatch_uses_loaded_issue_without_refetch_by_id() {
    let workspace_root = unique_workspace_root("orphan-direct-dispatch");
    let issue = sample_issue("issue-1", "FAC-1", "Todo", "First");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service
        .state
        .orphan_dispatch_keys
        .insert(sanitize_workspace_key(&issue.identifier));

    service.tick().await;

    assert_eq!(tracker_handle.fetch_by_ids_calls(), 0);
    assert!(service.state.running.contains_key(&issue.id));
}

#[tokio::test]
async fn first_tick_shows_issues_before_startup_cleanup_finishes() {
    let workspace_root = unique_workspace_root("startup-first-paint");
    let issue = sample_issue("issue-startup-1", "FAC-STARTUP-1", "Todo", "First paint");
    let tracker = DelayedCleanupTracker {
        issues: Arc::new(vec![issue.clone()]),
        cleanup_gate: Arc::new(Notify::new()),
    };
    let workflow = test_workflow(&workspace_root);
    let (_tx, workflow_rx) = watch::channel(workflow);
    let (service, handle) = RuntimeService::new(
        Arc::new(tracker.clone()),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        workflow_rx,
    );
    let mut snapshot_rx = handle.snapshot_rx.clone();
    let command_tx = handle.command_tx.clone();
    let service_task = tokio::spawn(async move { service.run().await });

    timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = snapshot_rx.borrow().clone();
            if snapshot
                .visible_issues
                .iter()
                .any(|row| row.issue_id == issue.id)
            {
                break;
            }
            snapshot_rx
                .changed()
                .await
                .expect("snapshot channel closed");
        }
    })
    .await
    .expect("first issue snapshot should not wait for startup cleanup");

    tracker.cleanup_gate.notify_waiters();
    let _ = command_tx.send(RuntimeCommand::Shutdown);
    service_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn run_normalizes_restored_stale_movement_before_first_snapshot() {
    let workspace_root = unique_workspace_root("startup-normalize-stale-movement");
    let tracker = TestTracker::new(Vec::new());
    let workflow = test_workflow(&workspace_root);
    let (_tx, workflow_rx) = watch::channel(workflow);
    let store = Arc::new(polyphony_core::file_store::JsonStateStore::new(
        workspace_root.join("state.json"),
    ));
    let now = Utc::now();
    let movement = Movement {
        id: "mov-startup-stale".into(),
        kind: MovementKind::PullRequestReview,
        issue_id: Some("issue-89".into()),
        issue_identifier: Some("penso/arbor#89".into()),
        title: "Review PR".into(),
        status: MovementStatus::Cancelled,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("penso_arbor_89".into()),
        workspace_path: Some(workspace_root.join("penso_arbor_89")),
        review_target: None,
        deliverable: None,
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
        updated_at: now,
    };
    let task = Task {
        id: "task-startup-stale".into(),
        movement_id: movement.id.clone(),
        title: "Run PR review".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Review,
        status: TaskStatus::InProgress,
        ordinal: 1,
        parent_id: None,
        agent_name: Some("reviewer".into()),
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: Some(now),
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    polyphony_core::StateStore::save_movement(store.as_ref(), &movement)
        .await
        .unwrap();
    polyphony_core::StateStore::save_task(store.as_ref(), &task)
        .await
        .unwrap();

    let (service, handle) = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        Some(store),
        None,
        workflow_rx,
    );
    let mut snapshot_rx = handle.snapshot_rx.clone();
    let command_tx = handle.command_tx.clone();
    let service_task = tokio::spawn(async move { service.run().await });

    let snapshot = timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = snapshot_rx.borrow().clone();
            if snapshot
                .movements
                .iter()
                .any(|row| row.id == movement.id && row.status == MovementStatus::Failed)
            {
                break snapshot;
            }
            snapshot_rx
                .changed()
                .await
                .expect("snapshot channel closed");
        }
    })
    .await
    .expect("startup snapshot should include normalized stale movement");

    let movement_row = snapshot
        .movements
        .iter()
        .find(|row| row.id == movement.id)
        .expect("movement row");
    assert_eq!(movement_row.status, MovementStatus::Failed);
    let task_row = snapshot
        .tasks
        .iter()
        .find(|row| row.id == task.id)
        .expect("task row");
    assert_eq!(task_row.status, TaskStatus::Failed);
    assert_eq!(
        task_row.error.as_deref(),
        Some("restored without an active agent session; retry the movement to continue")
    );

    let _ = command_tx.send(RuntimeCommand::Shutdown);
    service_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn automatic_dispatch_skips_waiting_issue_approval() {
    let workspace_root = unique_workspace_root("approval-waiting");
    let mut issue = sample_issue("issue-approval-1", "FAC-APPROVAL-1", "Todo", "Review input");
    issue.approval_state = polyphony_core::IssueApprovalState::Waiting;
    let tracker = TestTracker::new(vec![issue.clone()]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;

    assert!(!service.state.running.contains_key(&issue.id));
    assert_eq!(service.state.visible_issues.len(), 1);
    assert_eq!(
        service.state.visible_issues[0].approval_state,
        polyphony_core::IssueApprovalState::Waiting
    );
}

#[tokio::test]
async fn manual_dispatch_still_runs_waiting_issue_without_approval_override() {
    let workspace_root = unique_workspace_root("approval-manual-dispatch");
    let mut issue = sample_issue("issue-approval-2", "FAC-APPROVAL-2", "Todo", "Manual only");
    issue.approval_state = polyphony_core::IssueApprovalState::Waiting;
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: issue.id.clone(),
            agent_name: None,
            directives: None,
        });

    service.process_manual_dispatches().await;

    assert!(service.state.running.contains_key(&issue.id));
    assert!(service.state.approved_issue_keys.is_empty());
}

#[tokio::test]
async fn approving_waiting_issue_persists_and_allows_automatic_dispatch() {
    let workspace_root = unique_workspace_root("approval-approved");
    let mut issue = sample_issue("issue-approval-3", "FAC-APPROVAL-3", "Todo", "Approve me");
    issue.approval_state = polyphony_core::IssueApprovalState::Waiting;
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;
    service
        .pending_issue_approvals
        .push((issue.id.clone(), "mock".into()));
    service.process_pending_issue_approvals().await;

    let snapshot = service.snapshot();
    assert_eq!(snapshot.approved_issue_keys, vec!["mock:issue-approval-3"]);
    assert_eq!(snapshot.visible_triggers.len(), 1);
    assert_eq!(
        snapshot.visible_triggers[0].approval_state,
        polyphony_core::IssueApprovalState::Approved
    );

    service.tick().await;

    assert!(service.state.running.contains_key(&issue.id));
}

#[tokio::test]
async fn closing_visible_issue_updates_tracker_and_cleans_workspace() {
    let workspace_root = unique_workspace_root("close-issue-trigger");
    let issue = sample_issue("issue-close-1", "FAC-CLOSE-1", "Todo", "Already done");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_for_assertions = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let provisioner_for_assertions = provisioner.clone();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    service.tick().await;

    let workspace_key = sanitize_workspace_key(&issue.identifier);
    service.state.worktree_keys.insert(workspace_key.clone());
    tokio::fs::create_dir_all(workspace_root.join(&workspace_key))
        .await
        .expect("workspace directory created");

    service.pending_issue_closures.push(issue.id.clone());
    service.process_pending_issue_closures().await;

    assert!(
        service
            .state
            .visible_issues
            .iter()
            .all(|row| row.issue_id != issue.id),
        "closed issue should be removed from active issue rows"
    );
    assert_eq!(
        tracker_for_assertions
            .issues
            .lock()
            .unwrap()
            .get(&issue.id)
            .expect("issue exists")
            .state,
        "Closed"
    );
    assert_eq!(
        tracker_for_assertions.recorded_issue_updates().len(),
        1,
        "tracker should receive one close update"
    );
    assert_eq!(
        provisioner_for_assertions.cleaned_issue_identifiers(),
        vec![issue.identifier.clone()],
    );
    assert!(
        !service.state.worktree_keys.contains(&workspace_key),
        "workspace key should be removed after cleanup"
    );
}

#[tokio::test]
async fn reconcile_running_cleans_workspace_for_terminal_issue() {
    let workspace_root = unique_workspace_root("terminal");
    let provisioner = RecordingProvisioner::default();
    let tracker_issue = sample_issue("issue-2", "FAC-2", "Done", "Closed");
    let mut service = test_service(
        TestTracker::new(vec![tracker_issue.clone()]),
        provisioner.clone(),
        &workspace_root,
    );
    let running_issue = sample_issue("issue-2", "FAC-2", "Todo", "Open");
    let workspace_path = workspace_root.join("FAC-2");
    fs::create_dir_all(&workspace_path).unwrap();
    service.state.running.insert(
        running_issue.id.clone(),
        make_running_task(running_issue.clone(), workspace_path),
    );
    service.claim_issue(running_issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    assert!(!service.state.running.contains_key(&running_issue.id));
    assert_eq!(provisioner.cleaned_issue_identifiers(), vec![
        running_issue.identifier
    ]);
}

#[tokio::test]
async fn reconcile_running_replaces_full_issue_snapshot() {
    let workspace_root = unique_workspace_root("refresh");
    let provisioner = RecordingProvisioner::default();
    let mut refreshed_issue = sample_issue("issue-3", "FAC-3", "Todo", "Updated title");
    refreshed_issue.author = Some(IssueAuthor {
        id: Some("author-1".into()),
        username: Some("outsider".into()),
        display_name: Some("Outsider".into()),
        role: Some("none".into()),
        trust_level: Some("outsider".into()),
        url: None,
    });
    refreshed_issue.comments.push(IssueComment {
        id: "comment-1".into(),
        body: "New follow-up context".into(),
        author: refreshed_issue.author.clone(),
        url: None,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    });
    let mut service = test_service(
        TestTracker::new(vec![refreshed_issue.clone()]),
        provisioner,
        &workspace_root,
    );
    let stale_issue = sample_issue("issue-3", "FAC-3", "Todo", "Old title");
    let workspace_path = workspace_root.join("FAC-3");
    service.state.running.insert(
        stale_issue.id.clone(),
        make_running_task(stale_issue.clone(), workspace_path),
    );
    service.claim_issue(stale_issue.id.clone(), IssueClaimState::Running);

    service.reconcile_running().await;

    let running = service.state.running.get(&stale_issue.id).unwrap();
    assert_eq!(running.issue.title, "Updated title");
    assert_eq!(running.issue.comments.len(), 1);
    assert_eq!(
        running
            .issue
            .author
            .as_ref()
            .and_then(|author| author.trust_level.as_deref()),
        Some("outsider")
    );
}

#[tokio::test]
async fn finish_running_success_marks_completed_and_queues_retry() {
    let workspace_root = unique_workspace_root("finish");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-4", "FAC-4", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-4");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some("Human Review".into()),
            },
        )
        .await
        .unwrap();

    assert!(service.state.completed.contains(&issue.id));
    assert!(service.state.retrying.contains_key(&issue.id));
    assert_eq!(
        service.state.claim_states.get(&issue.id),
        Some(&IssueClaimState::RetryQueued)
    );
}

#[tokio::test]
async fn finish_running_with_active_final_state_skips_workflow_transition() {
    let workspace_root = unique_workspace_root("finish-active");
    let provisioner = RecordingProvisioner::default();
    let tracker = TestTracker::new(Vec::new());
    let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
    let issue = sample_issue("issue-4b", "FAC-4B", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-4B");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 2,
                error: None,
                final_issue_state: Some("Todo".into()),
            },
        )
        .await
        .unwrap();

    assert!(tracker.recorded_workflow_updates().is_empty());
    assert!(service.state.retrying.contains_key(&issue.id));
}

#[test]
fn worker_timeout_errors_map_to_timed_out_attempts() {
    let result =
        agent_run_result_from_error(&Error::Core(CoreError::Adapter("turn_timeout".into())));
    assert!(matches!(result.status, AttemptStatus::TimedOut));
    assert_eq!(result.error.as_deref(), Some("turn_timeout"));

    let startup_timeout =
        agent_run_result_from_error(&Error::Core(CoreError::Adapter("response_timeout".into())));
    assert!(matches!(startup_timeout.status, AttemptStatus::TimedOut));
    assert_eq!(startup_timeout.error.as_deref(), Some("response_timeout"));
}

#[tokio::test]
async fn fail_running_preserves_stalled_status() {
    let workspace_root = unique_workspace_root("finish-stalled");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-4c", "FAC-4C", "Todo", "Stalled");
    let workspace_path = workspace_root.join("FAC-4C");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);

    service
        .fail_running(&issue.id, AttemptStatus::Stalled, "stall_timeout")
        .await;

    assert!(!service.state.running.contains_key(&issue.id));
    let retry = service.state.retrying.get(&issue.id).unwrap();
    assert_eq!(retry.row.error.as_deref(), Some("stall_timeout"));
    let context = service.state.saved_contexts.get(&issue.id).unwrap();
    assert_eq!(context.status, Some(AttemptStatus::Stalled));
    assert_eq!(context.error.as_deref(), Some("stall_timeout"));
}

#[tokio::test]
async fn run_worker_attempt_reuses_live_session_and_continues_while_issue_active() {
    let workspace_root = unique_workspace_root("worker-turns");
    let provisioner = Arc::new(RecordingProvisioner::default());
    let workspace_manager = WorkspaceManager::new(
        workspace_root.clone(),
        provisioner,
        polyphony_core::CheckoutKind::Directory,
        true,
        Vec::new(),
        None,
        None,
        None,
    );
    let issue = sample_issue("issue-turns", "FAC-TURNS", "Todo", "Loop");
    let tracker = Arc::new(SequencedStateTracker::new(issue.clone(), vec![
        "Todo",
        "Human Review",
    ]));
    let agent = Arc::new(RecordingSessionAgent::default());
    let hooks = HooksConfig {
        after_create: None,
        before_run: None,
        after_run: None,
        after_outcome: None,
        before_remove: None,
        timeout_ms: 1_000,
    };
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();

    let result = run_worker_attempt(
        &workspace_manager,
        &hooks,
        agent.clone(),
        tracker,
        issue,
        Some(2),
        workspace_root.join("FAC-TURNS"),
        "Initial prompt".into(),
        vec!["Todo".into(), "In Progress".into()],
        4,
        Some(
            "Continue {{ issue.identifier }} in state {{ issue.state }}.\n\
Turn {{ turn_number }} of {{ max_turns }}. Continuation={{ is_continuation }}."
                .into(),
        ),
        polyphony_core::AgentDefinition {
            name: "codex".into(),
            kind: "codex".into(),
            transport: polyphony_core::AgentTransport::AppServer,
            ..polyphony_core::AgentDefinition::default()
        },
        None,
        command_tx,
    )
    .await
    .unwrap();

    while command_rx.try_recv().is_ok() {}

    assert!(matches!(result.status, AttemptStatus::Succeeded));
    assert_eq!(result.turns_completed, 2);
    assert_eq!(result.final_issue_state.as_deref(), Some("Human Review"));
    assert_eq!(agent.session_starts(), 1);
    assert_eq!(agent.stops(), 1);
    let prompts = agent.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0], "Initial prompt");
    assert_eq!(
        prompts[1],
        "Continue FAC-TURNS in state Todo.\nTurn 2 of 4. Continuation=true."
    );
}

#[tokio::test]
async fn saved_context_updates_from_streamed_agent_events() {
    let workspace_root = unique_workspace_root("context-events");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-5", "FAC-5", "Todo", "Context");
    let workspace_path = workspace_root.join("FAC-5");
    let mut running = make_running_task(issue.clone(), workspace_path);
    running.model = Some("kimi-2.5".into());
    service.state.running.insert(issue.id.clone(), running);

    service
        .handle_message(OrchestratorMessage::AgentEvent(AgentEvent {
            issue_id: issue.id.clone(),
            issue_identifier: issue.identifier.clone(),
            agent_name: "kimi".into(),
            session_id: Some("sess-1".into()),
            thread_id: Some("thread-1".into()),
            turn_id: Some("turn-3".into()),
            codex_app_server_pid: Some("4242".into()),
            kind: AgentEventKind::Notification,
            at: Utc::now(),
            message: Some("Investigating failing test".into()),
            usage: Some(TokenUsage {
                input_tokens: 12,
                output_tokens: 8,
                total_tokens: 20,
            }),
            rate_limits: None,
            raw: None,
        }))
        .await
        .unwrap();

    let context = service.state.saved_contexts.get(&issue.id).unwrap();
    assert_eq!(context.agent_name, "kimi");
    assert_eq!(context.model.as_deref(), Some("kimi-2.5"));
    assert_eq!(context.session_id.as_deref(), Some("sess-1"));
    assert_eq!(context.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(context.turn_id.as_deref(), Some("turn-3"));
    assert_eq!(context.codex_app_server_pid.as_deref(), Some("4242"));
    assert_eq!(context.usage.total_tokens, 20);
    assert_eq!(context.transcript.len(), 1);
    assert!(
        context.transcript[0]
            .message
            .contains("Investigating failing test")
    );
    let snapshot = service.snapshot();
    let running = &snapshot.running[0];
    assert_eq!(running.session_id.as_deref(), Some("sess-1"));
    assert_eq!(running.thread_id.as_deref(), Some("thread-1"));
    assert_eq!(running.turn_id.as_deref(), Some("turn-3"));
    assert_eq!(running.codex_app_server_pid.as_deref(), Some("4242"));
    let artifact_dir = workspace_root
        .join("FAC-5")
        .join(".polyphony")
        .join("runtime");
    let saved_context = tokio::fs::read_to_string(artifact_dir.join("saved-context.json"))
        .await
        .unwrap();
    let events = tokio::fs::read_to_string(artifact_dir.join("agent-events.jsonl"))
        .await
        .unwrap();
    assert!(saved_context.contains("\"issue_identifier\": \"FAC-5\""));
    assert!(events.contains("\"issue_identifier\":\"FAC-5\""));
}

#[tokio::test]
async fn retry_dispatch_rotates_to_fallback_agent_using_saved_context() {
    let workspace_root = unique_workspace_root("fallback");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: codex\n  profiles:\n    codex:\n      kind: codex\n      transport: app_server\n      command: codex app-server\n      fallbacks:\n        - kimi\n        - claude\n    kimi:\n      kind: kimi\n      api_key: test-kimi\n      model: kimi-2.5\n    claude:\n      kind: claude\n      transport: local_cli\n      command: claude\n---\nTest prompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let tracker = TestTracker::new(vec![sample_issue("issue-6", "FAC-6", "Todo", "Retry")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(NoopAgent),
        Arc::new(provisioner),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let issue = sample_issue("issue-6", "FAC-6", "Todo", "Retry");
    service
        .state
        .saved_contexts
        .insert(issue.id.clone(), AgentContextSnapshot {
            issue_id: issue.id.clone(),
            issue_identifier: issue.identifier.clone(),
            updated_at: Utc::now(),
            agent_name: "codex".into(),
            model: Some("gpt-5-codex".into()),
            session_id: Some("session-1".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Failed),
            error: Some("rate limited".into()),
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: Utc::now(),
                kind: AgentEventKind::Notification,
                message: "Partial work already completed".into(),
            }],
        });

    service
        .dispatch_issue(workflow, issue.clone(), Some(2), true, None, false, None)
        .await
        .unwrap();

    let running = service.state.running.get(&issue.id).unwrap();
    assert_eq!(running.agent_name, "kimi");
    running.handle.abort();
}

#[test]
fn rate_limited_errors_are_detected_for_fast_retry() {
    assert!(is_rate_limited_error(Some(
        "rate_limited: Claude usage limit reached"
    )));
    assert!(is_rate_limited_error(Some("quota exhausted")));
    assert!(!is_rate_limited_error(Some("response_error")));
    assert!(!is_rate_limited_error(None));
}

#[test]
fn rate_limited_retries_skip_workspace_sync() {
    assert!(should_skip_workspace_sync_for_retry(Some(
        "rate_limited: You've hit your limit"
    )));
    assert!(should_skip_workspace_sync_for_retry(Some(
        "quota exhausted"
    )));
    assert!(!should_skip_workspace_sync_for_retry(Some(
        "response_error"
    )));
    assert!(!should_skip_workspace_sync_for_retry(None));
}

#[tokio::test]
async fn tick_defensively_reloads_workflow_and_rebuilds_components() {
    let workspace_root = unique_workspace_root("workflow-reload");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nInitial prompt\n",
    );
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(|workflow| {
        Ok(RuntimeComponents {
            tracker: Arc::new(NamedTracker::new(
                format!("tracker:{}", workflow.config.tracker.kind),
                Vec::new(),
            )),
            pull_request_trigger_source: None,
            agent: Arc::new(NamedAgent::new(format!(
                "agent:{}",
                workflow.config.tracker.kind
            ))),
            committer: None,
            pull_request_manager: None,
            pull_request_commenter: None,
            feedback: None,
        })
    });
    let mut service = test_service_with_reload(
        workflow.clone(),
        Arc::new(NamedTracker::new("tracker:mock", Vec::new())),
        Arc::new(NamedAgent::new("agent:mock")),
        RecordingProvisioner::default(),
        component_factory,
    );

    fs::write(
            &workflow.path,
            "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 250\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nReloaded prompt\n"
                .replace("__ROOT__", &workspace_root.display().to_string()),
        )
        .unwrap();

    service.tick().await;

    assert_eq!(service.tracker.component_key(), "tracker:none");
    assert_eq!(service.agent.component_key(), "agent:none");
    assert_eq!(service.workflow().config.polling.interval_ms, 250);
    assert_eq!(
        service.workflow().definition.prompt_template,
        "Reloaded prompt"
    );
}

#[tokio::test]
async fn invalid_reloaded_workflow_blocks_dispatch_until_fixed() {
    let workspace_root = unique_workspace_root("workflow-invalid");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nPrompt\n",
    );
    let issue = sample_issue("issue-reload", "FAC-RELOAD", "Todo", "Blocked");
    let issue_for_factory = issue.clone();
    let component_factory: Arc<RuntimeComponentFactory> = Arc::new(move |workflow| {
        Ok(RuntimeComponents {
            tracker: Arc::new(NamedTracker::new(
                format!("tracker:{}", workflow.config.tracker.kind),
                vec![issue_for_factory.clone()],
            )),
            pull_request_trigger_source: None,
            agent: Arc::new(NamedAgent::new(format!(
                "agent:{}",
                workflow.config.tracker.kind
            ))),
            committer: None,
            pull_request_manager: None,
            pull_request_commenter: None,
            feedback: None,
        })
    });
    let mut service = test_service_with_reload(
        workflow.clone(),
        Arc::new(NamedTracker::new("tracker:none", vec![issue.clone()])),
        Arc::new(NamedAgent::new("agent:none")),
        RecordingProvisioner::default(),
        component_factory,
    );

    fs::write(&workflow.path, "---\ntracker:\n  kind: [\n").unwrap();

    service.tick().await;

    assert!(service.workflow_reload_error().is_some());
    assert!(service.state.running.is_empty());
    assert_eq!(service.workflow().definition.prompt_template, "Prompt");
}

#[test]
fn append_saved_context_includes_recent_transcript() {
    let prompt = append_saved_context(
        "Base prompt".into(),
        Some(&AgentContextSnapshot {
            issue_id: "issue-7".into(),
            issue_identifier: "FAC-7".into(),
            updated_at: Utc::now(),
            agent_name: "claude".into(),
            model: Some("claude-sonnet".into()),
            session_id: Some("session-2".into()),
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: Some(AttemptStatus::Failed),
            error: Some("tool timeout".into()),
            usage: TokenUsage::default(),
            transcript: vec![AgentContextEntry {
                at: Utc::now(),
                kind: AgentEventKind::Notification,
                message: "Implemented parser, tests still failing".into(),
            }],
        }),
        true,
    );

    assert!(prompt.contains("## Saved Polyphony Context"));
    assert!(prompt.contains("Last agent: claude (claude-sonnet)"));
    assert!(prompt.contains("Last error: tool timeout"));
    assert!(prompt.contains("Implemented parser, tests still failing"));
}

#[tokio::test]
async fn pipeline_issue_trigger_creates_pull_request_deliverable_without_github() {
    let workspace_root = unique_workspace_root("pipeline-issue-pr");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue("issue-pipeline-pr", "DOG-101", "Todo", "Create e2e file");
    issue.url = Some("https://example.test/issues/DOG-101".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let agent = ScriptedPipelineAgent::default();
    let agent_handle = agent.clone();
    let committer = RecordingCommitter::new(Some(WorkspaceCommitResult {
        branch_name: "task/dog-101".into(),
        head_sha: "abc123def".into(),
        changed_files: 1,
        lines_added: None,
        lines_removed: None,
    }));
    let committer_handle = committer.clone();
    let pull_request_manager = RecordingPullRequestManager::new(PullRequestRef {
        repository: "penso/polyphony".into(),
        number: 17,
        url: Some("https://github.com/penso/polyphony/pull/17".into()),
    });
    let pull_request_manager_handle = pull_request_manager.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(committer)),
        Some(Arc::new(pull_request_manager)),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_issue(
            workflow.clone(),
            issue.clone(),
            None,
            false,
            None,
            false,
            None,
        )
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let movement = service
        .state
        .movements
        .values()
        .find(|movement| movement.issue_id.as_deref() == Some(issue.id.as_str()))
        .cloned()
        .expect("issue movement missing after pipeline completion");
    assert_eq!(movement.status, MovementStatus::Delivered);
    let deliverable = movement
        .deliverable
        .expect("movement should record the pull request deliverable");
    assert_eq!(deliverable.kind, DeliverableKind::GithubPullRequest);
    assert_eq!(deliverable.status, DeliverableStatus::Open);
    assert_eq!(
        deliverable.url.as_deref(),
        Some("https://github.com/penso/polyphony/pull/17")
    );
    assert_eq!(tracker_handle.recorded_workflow_updates(), vec![
        "In Progress",
        "Done"
    ]);
    assert_eq!(committer_handle.requests().len(), 1);
    assert_eq!(
        committer_handle.requests()[0].base_branch.as_deref(),
        Some("main")
    );
    assert_eq!(pull_request_manager_handle.requests().len(), 1);
    assert_eq!(
        pull_request_manager_handle.requests()[0].head_branch,
        "task/dog-101"
    );
    assert_eq!(
        pull_request_manager_handle.requests()[0].title,
        "DOG-101: Create e2e file"
    );
    assert_eq!(agent_handle.recorded_agent_names(), vec![
        "router",
        "implementer",
        "implementer"
    ]);
    assert_eq!(
        tokio::fs::read_to_string(workspace_root.join("DOG-101").join("e2e-pr.txt"))
            .await
            .unwrap(),
        "polyphony end-to-end dogfood\n"
    );

    let snapshot = service.snapshot();
    let movement_row = snapshot
        .movements
        .iter()
        .find(|movement| movement.issue_identifier.as_deref() == Some("DOG-101"))
        .expect("movement row missing from runtime snapshot");
    assert_eq!(movement_row.status, MovementStatus::Delivered);
    assert!(movement_row.has_deliverable);
}

#[tokio::test]
async fn pipeline_issue_trigger_writes_workspace_artifacts_and_runs_after_outcome_hook() {
    let workspace_root = unique_workspace_root("pipeline-issue-artifacts");
    let mut workflow = pipeline_workflow_with_automation(&workspace_root);
    workflow.config.hooks.after_outcome = Some("printf cleaned > .after_outcome".into());
    let (_tx, rx) = watch::channel(workflow.clone());
    let mut issue = sample_issue(
        "issue-pipeline-artifacts",
        "DOG-103",
        "Todo",
        "Archive artifacts",
    );
    issue.url = Some("https://example.test/issues/DOG-103".into());
    let tracker = TestTracker::new(vec![issue.clone()]);
    let agent = ScriptedPipelineAgent::default();
    let committer = RecordingCommitter::new(Some(WorkspaceCommitResult {
        branch_name: "task/dog-103".into(),
        head_sha: "abc123def".into(),
        changed_files: 1,
        lines_added: None,
        lines_removed: None,
    }));
    let pull_request_manager = RecordingPullRequestManager::new(PullRequestRef {
        repository: "penso/polyphony".into(),
        number: 23,
        url: Some("https://github.com/penso/polyphony/pull/23".into()),
    });
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(committer)),
        Some(Arc::new(pull_request_manager)),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_issue(workflow, issue, None, false, None, false, None)
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let workspace_path = workspace_root.join("DOG-103");
    let artifact_dir = workspace_path.join(".polyphony").join("runtime");
    assert_eq!(
        tokio::fs::read_to_string(workspace_path.join(".after_outcome"))
            .await
            .unwrap(),
        "cleaned"
    );
    let saved_context = tokio::fs::read_to_string(artifact_dir.join("saved-context.json"))
        .await
        .unwrap();
    let runs = tokio::fs::read_to_string(artifact_dir.join("run-history.jsonl"))
        .await
        .unwrap();
    assert!(saved_context.contains("\"issue_identifier\": \"DOG-103\""));
    assert!(runs.contains("\"issue_identifier\":\"DOG-103\""));
}

#[test]
fn restore_bootstrap_rehydrates_saved_context_from_workspace_artifact() {
    let workspace_root = unique_workspace_root("restore-context-artifact");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let workspace_path = workspace_root.join("DOG-104");
    std::fs::create_dir_all(workspace_path.join(".polyphony/runtime")).unwrap();
    let now = Utc::now();
    let context = AgentContextSnapshot {
        issue_id: "issue-restore".into(),
        issue_identifier: "DOG-104".into(),
        updated_at: now,
        agent_name: "implementer".into(),
        model: None,
        session_id: None,
        thread_id: None,
        turn_id: None,
        codex_app_server_pid: None,
        status: Some(AttemptStatus::Succeeded),
        error: None,
        usage: TokenUsage::default(),
        transcript: vec![AgentContextEntry {
            at: now,
            kind: AgentEventKind::Notification,
            message: "rehydrated from workspace".into(),
        }],
    };
    std::fs::write(
        polyphony_core::workspace_saved_context_artifact_path(&workspace_path),
        serde_json::to_vec_pretty(&context).unwrap(),
    )
    .unwrap();

    service.restore_bootstrap(StoreBootstrap {
        snapshot: Some(RuntimeSnapshot {
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            visible_issues: Vec::new(),
            visible_triggers: Vec::new(),
            approved_issue_keys: Vec::new(),
            running: Vec::new(),
            agent_history: Vec::new(),
            retrying: Vec::new(),
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: vec![saved_context_metadata(AgentContextSnapshot {
                transcript: Vec::new(),
                ..context.clone()
            })],
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            movements: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::default(),
            tracker_kind: TrackerKind::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
        }),
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        movements: std::collections::HashMap::new(),
        tasks: std::collections::HashMap::new(),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        run_history: vec![PersistedRunRecord {
            issue_id: "issue-restore".into(),
            issue_identifier: "DOG-104".into(),
            agent_name: "implementer".into(),
            model: None,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            status: AttemptStatus::Succeeded,
            attempt: Some(1),
            max_turns: 3,
            turn_count: 1,
            last_event: None,
            last_message: None,
            started_at: now,
            finished_at: Some(now),
            last_event_at: Some(now),
            tokens: TokenUsage::default(),
            workspace_path: Some(workspace_path.clone()),
            error: None,
            saved_context: None,
        }],
    });

    let restored = service.state.saved_contexts.get("issue-restore").unwrap();
    assert_eq!(restored.transcript.len(), 1);
    assert_eq!(restored.transcript[0].message, "rehydrated from workspace");
}

#[tokio::test]
async fn pipeline_issue_trigger_can_finish_without_opening_a_pull_request() {
    let workspace_root = unique_workspace_root("pipeline-issue-no-pr");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-pipeline-clean",
        "DOG-102",
        "Todo",
        "Workspace already done",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let tracker_handle = tracker.clone();
    let agent = ScriptedPipelineAgent::default();
    let agent_handle = agent.clone();
    let committer = RecordingCommitter::new(None);
    let committer_handle = committer.clone();
    let pull_request_manager = RecordingPullRequestManager::new(PullRequestRef {
        repository: "penso/polyphony".into(),
        number: 99,
        url: Some("https://github.com/penso/polyphony/pull/99".into()),
    });
    let pull_request_manager_handle = pull_request_manager.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(committer)),
        Some(Arc::new(pull_request_manager)),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .dispatch_issue(workflow, issue.clone(), None, false, None, false, None)
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let movement = service
        .state
        .movements
        .values()
        .find(|movement| movement.issue_id.as_deref() == Some(issue.id.as_str()))
        .cloned()
        .expect("issue movement missing after clean pipeline completion");
    assert_eq!(movement.status, MovementStatus::Review);
    assert!(movement.deliverable.is_none());
    assert_eq!(tracker_handle.recorded_workflow_updates(), vec![
        "In Progress",
        "Done"
    ]);
    assert_eq!(committer_handle.requests().len(), 1);
    assert!(pull_request_manager_handle.requests().is_empty());
    assert_eq!(agent_handle.recorded_agent_names(), vec![
        "router",
        "implementer"
    ]);

    let snapshot = service.snapshot();
    let movement_row = snapshot
        .movements
        .iter()
        .find(|movement| movement.issue_identifier.as_deref() == Some("DOG-102"))
        .expect("movement row missing from runtime snapshot");
    assert_eq!(movement_row.status, MovementStatus::Review);
    assert!(!movement_row.has_deliverable);
}

#[tokio::test]
async fn resolving_movement_deliverable_updates_decision_and_snapshot() {
    let workspace_root = unique_workspace_root("deliverable-decision");
    let tracker = TestTracker::new(vec![sample_issue("github:7", "#7", "Todo", "Need a PR")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    service.state.movements.insert("mov-1".into(), Movement {
        id: "mov-1".into(),
        kind: MovementKind::IssueDelivery,
        issue_id: Some("github:7".into()),
        issue_identifier: Some("#7".into()),
        title: "Need a PR".into(),
        status: MovementStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(workspace_root.join("_7")),
        review_target: None,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::GithubPullRequest,
            status: DeliverableStatus::Open,
            url: Some("https://github.com/penso/polyphony/pull/8".into()),
            decision: DeliverableDecision::Waiting,
            title: None,
            description: None,
            metadata: Default::default(),
        }),
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
        updated_at: now,
    });

    service
        .pending_deliverable_resolutions
        .push(("mov-1".into(), DeliverableDecision::Accepted));
    service.process_pending_deliverable_resolutions().await;

    let movement = service
        .state
        .movements
        .get("mov-1")
        .expect("movement exists");
    let deliverable = movement
        .deliverable
        .as_ref()
        .expect("deliverable exists after resolution");
    assert_eq!(deliverable.decision, DeliverableDecision::Accepted);

    let snapshot = service.snapshot();
    let row = snapshot.movements.first().expect("movement row exists");
    assert_eq!(
        row.deliverable
            .as_ref()
            .expect("deliverable row exists")
            .decision,
        DeliverableDecision::Accepted
    );
}

#[tokio::test]
async fn resolving_already_accepted_deliverable_is_ignored() {
    let workspace_root = unique_workspace_root("deliverable-decision-ignored");
    let tracker = TestTracker::new(vec![sample_issue("github:7", "#7", "Todo", "Need a PR")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    service.state.movements.insert("mov-1".into(), Movement {
        id: "mov-1".into(),
        kind: MovementKind::IssueDelivery,
        issue_id: Some("github:7".into()),
        issue_identifier: Some("#7".into()),
        title: "Need a PR".into(),
        status: MovementStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(workspace_root.join("_7")),
        review_target: None,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::Patch,
            status: DeliverableStatus::Open,
            url: None,
            decision: DeliverableDecision::Accepted,
            title: None,
            description: None,
            metadata: Default::default(),
        }),
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
        updated_at: now,
    });

    service
        .pending_deliverable_resolutions
        .push(("mov-1".into(), DeliverableDecision::Accepted));
    service.process_pending_deliverable_resolutions().await;

    let movement = service
        .state
        .movements
        .get("mov-1")
        .expect("movement exists");
    let deliverable = movement
        .deliverable
        .as_ref()
        .expect("deliverable exists after ignored resolution");
    assert_eq!(deliverable.decision, DeliverableDecision::Accepted);
    assert_eq!(
        service
            .state
            .recent_events
            .front()
            .expect("ignored event recorded")
            .message,
        "deliverable decision ignored: #7 already accepted"
    );
}

#[tokio::test]
async fn startup_cleanup_finalizes_merged_accepted_movements() {
    let workspace_root = unique_workspace_root("startup-finalize-accepted");
    let tracker = TestTracker::new(vec![sample_issue("github:7", "#7", "Todo", "Need a PR")]);
    let tracker_for_assertions = tracker.clone();
    let provisioner = RecordingProvisioner::default();
    let provisioner_for_assertions = provisioner.clone();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    service.state.movements.insert("mov-1".into(), Movement {
        id: "mov-1".into(),
        kind: MovementKind::IssueDelivery,
        issue_id: Some("github:7".into()),
        issue_identifier: Some("#7".into()),
        title: "Need a PR".into(),
        status: MovementStatus::Delivered,
        pipeline_stage: None,
        manual_dispatch_directives: None,
        workspace_key: Some("_7".into()),
        workspace_path: Some(workspace_root.join("_7")),
        review_target: None,
        deliverable: Some(Deliverable {
            kind: DeliverableKind::LocalBranch,
            status: DeliverableStatus::Merged,
            url: None,
            decision: DeliverableDecision::Accepted,
            title: Some("Branch: task/7".into()),
            description: None,
            metadata: std::collections::HashMap::from([(
                "branch".into(),
                serde_json::Value::String("task/7".into()),
            )]),
        }),
        created_at: now,
        activity_log: Vec::new(),
        cancel_reason: None,
        steps: Vec::new(),
        updated_at: now,
    });
    service.state.worktree_keys.insert("_7".into());
    tokio::fs::create_dir_all(workspace_root.join("_7"))
        .await
        .expect("workspace directory created");

    service.startup_cleanup().await;

    assert_eq!(
        tracker_for_assertions
            .issues
            .lock()
            .unwrap()
            .get("github:7")
            .expect("issue exists")
            .state,
        "Closed"
    );
    assert_eq!(
        tracker_for_assertions.recorded_issue_updates().len(),
        1,
        "startup cleanup should close the tracker issue once"
    );
    assert_eq!(
        provisioner_for_assertions.cleaned_issue_identifiers(),
        vec!["#7".to_string()],
    );
    assert!(
        !service.state.worktree_keys.contains("_7"),
        "startup cleanup should remove the cleaned worktree key"
    );
}

// ---------------------------------------------------------------------------
// Stop mode tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stop_mode_skips_dispatch_on_tick() {
    let workspace_root = unique_workspace_root("stop-tick");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service.tick().await;

    assert!(
        !service.state.running.contains_key("issue-1"),
        "stop mode should prevent dispatch"
    );
}

#[tokio::test]
async fn manual_dispatch_works_in_stop_mode() {
    let workspace_root = unique_workspace_root("stop-manual");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;
    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: "issue-1".into(),
            agent_name: None,
            directives: None,
        });

    service.process_manual_dispatches().await;

    assert!(
        service.state.running.contains_key("issue-1"),
        "manual dispatch should work even in stop mode"
    );
}

#[tokio::test]
async fn stop_mode_blocks_automatic_dispatch() {
    let workspace_root = unique_workspace_root("stop-auto");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "Auto issue")]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service.tick().await;

    assert!(
        !service.state.running.contains_key("issue-1"),
        "stop mode should block automatic dispatch"
    );
}

#[tokio::test]
async fn manual_dispatch_is_processed_on_next_tick() {
    let workspace_root = unique_workspace_root("manual-tick");
    let issue = sample_issue("issue-tick-1", "FAC-TICK-1", "Todo", "Tick test");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);

    // Queue a manual dispatch
    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: issue.id.clone(),
            agent_name: None,
            directives: None,
        });

    // A single tick should process it
    service.tick().await;

    assert!(
        service.state.running.contains_key(&issue.id),
        "manual dispatch should be processed on the immediate next tick"
    );
}

#[tokio::test]
async fn manual_pull_request_dispatch_failure_creates_visible_failed_movement() {
    let workspace_root = unique_workspace_root("manual-pr-dispatch-failure");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 89,
        title: "Review me".into(),
        url: Some("https://github.com/penso/polyphony/pull/89".into()),
        base_branch: "main".into(),
        head_branch: "feature/review".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/89/head".into()),
        author_login: Some("alice".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        is_draft: false,
    };
    let issue = synthetic_issue_for_pull_request_review(&trigger);
    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(FailingProvisioner {
            message: "ssh auth failed".into(),
        }),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let dispatch = service
        .dispatch_pull_request_review(workflow, trigger, None, Some("Check auth"))
        .await;
    assert!(
        dispatch.is_err(),
        "workspace setup should fail in this test"
    );

    let (movement_id, movement) = service
        .state
        .movements
        .iter()
        .next()
        .expect("movement should be created before workspace setup succeeds");
    assert_eq!(movement.issue_id.as_deref(), Some(issue.id.as_str()));
    assert_eq!(movement.status, MovementStatus::Failed);
    assert_eq!(
        movement.manual_dispatch_directives.as_deref(),
        Some("Check auth")
    );
    let tasks = service
        .state
        .tasks
        .get(movement_id)
        .expect("tasks should exist for PR dispatch");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].title, "Creating worktree");
    assert_eq!(tasks[0].status, TaskStatus::Failed);
    assert_eq!(tasks[1].title, "Run PR review");
    assert_eq!(tasks[1].status, TaskStatus::Cancelled);
    assert!(
        !service.state.running.contains_key(&issue.id),
        "worker should not start when workspace setup fails"
    );
}

#[tokio::test]
async fn workspace_progress_updates_are_appended_to_worktree_task() {
    let workspace_root = unique_workspace_root("workspace-progress");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let now = Utc::now();
    let movement_id = "mov-progress-1".to_string();
    let task_id = "task-progress-1".to_string();
    let issue_identifier = "penso/polyphony#89".to_string();
    let workspace_key = "penso_polyphony_89".to_string();

    service
        .state
        .movements
        .insert(movement_id.clone(), Movement {
            id: movement_id.clone(),
            kind: MovementKind::PullRequestReview,
            issue_id: Some("pr_review:github:penso/polyphony:89:head".into()),
            issue_identifier: Some(issue_identifier.clone()),
            title: "Review me".into(),
            status: MovementStatus::InProgress,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: Some(workspace_key.clone()),
            workspace_path: None,
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: now,
        });
    service.state.tasks.insert(movement_id.clone(), vec![Task {
        id: task_id.clone(),
        movement_id: movement_id.clone(),
        title: "Creating worktree".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Research,
        status: TaskStatus::InProgress,
        ordinal: 0,
        parent_id: None,
        agent_name: Some("orchestrator".into()),
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: Some(now),
        finished_at: None,
        error: None,
        created_at: now,
        updated_at: now,
    }]);
    service
        .state
        .workspace_setup_tasks_by_issue_identifier
        .insert(
            issue_identifier.clone(),
            (movement_id.clone(), task_id.clone()),
        );
    service.state.workspace_setup_tasks_by_key.insert(
        workspace_key.clone(),
        (movement_id.clone(), task_id.clone()),
    );

    let update = WorkspaceProgressUpdate {
        issue_identifier: issue_identifier.clone(),
        workspace_key: workspace_key.clone(),
        message: "Fetching origin".into(),
        at: now,
    };
    service
        .record_workspace_progress(update.clone())
        .await
        .unwrap();
    service.record_workspace_progress(update).await.unwrap();
    service
        .record_workspace_progress(WorkspaceProgressUpdate {
            issue_identifier,
            workspace_key,
            message: "Waiting for SSH key touch on github.com".into(),
            at: now + chrono::Duration::seconds(1),
        })
        .await
        .unwrap();
    service
        .record_workspace_progress(WorkspaceProgressUpdate {
            issue_identifier: "penso/arbor#89".into(),
            workspace_key: "penso_arbor_89".into(),
            message: "Waiting for SSH key touch on github.com".into(),
            at: now + chrono::Duration::seconds(2),
        })
        .await
        .unwrap();

    let tasks = service.state.tasks.get(&movement_id).unwrap();
    assert_eq!(tasks[0].activity_log.len(), 2);
    assert!(tasks[0].activity_log[0].ends_with("Fetching origin"));
    assert!(tasks[0].activity_log[1].ends_with("Waiting for SSH key touch on github.com"));

    let snapshot = service.snapshot();
    let task_row = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(task_row.activity_log.len(), 2);
    assert!(task_row.activity_log[0].ends_with("Fetching origin"));
}

#[tokio::test]
async fn task_retry_ignores_non_failed_tasks() {
    let workspace_root = unique_workspace_root("retry-only-failed");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let now = Utc::now();
    let movement_id = "mov-retry-1".to_string();
    let task_id = "task-retry-1".to_string();

    service
        .state
        .movements
        .insert(movement_id.clone(), Movement {
            id: movement_id.clone(),
            kind: MovementKind::PullRequestReview,
            issue_id: Some("pr_review:github:penso/polyphony:89:head".into()),
            issue_identifier: Some("penso/polyphony#89".into()),
            title: "Retry me".into(),
            status: MovementStatus::Failed,
            pipeline_stage: Some(PipelineStage::Executing),
            manual_dispatch_directives: None,
            workspace_key: Some("penso_polyphony_89".into()),
            workspace_path: Some(workspace_root.join("penso_polyphony_89")),
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: now,
        });
    service.state.tasks.insert(movement_id.clone(), vec![Task {
        id: task_id.clone(),
        movement_id: movement_id.clone(),
        title: "Creating worktree".into(),
        description: None,
        activity_log: Vec::new(),
        category: polyphony_core::TaskCategory::Research,
        status: TaskStatus::Completed,
        ordinal: 0,
        parent_id: None,
        agent_name: Some("orchestrator".into()),
        session_id: None,
        thread_id: None,
        turns_completed: 0,
        tokens: TokenUsage::default(),
        started_at: Some(now),
        finished_at: Some(now),
        error: None,
        created_at: now,
        updated_at: now,
    }]);
    service
        .pending_task_retries
        .push((movement_id.clone(), task_id.clone()));

    service.process_pending_task_retries().await;

    let task = service
        .state
        .tasks
        .get(&movement_id)
        .and_then(|tasks| tasks.iter().find(|task| task.id == task_id))
        .expect("task should remain present");
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(task.finished_at.is_some());
    assert!(
        service
            .state
            .recent_events
            .iter()
            .any(|event| event.message.contains("only failed tasks can retry")),
        "runtime should record why the retry was ignored"
    );
}

#[tokio::test]
async fn movement_retry_relaunches_pull_request_review_from_first_failed_task() {
    let workspace_root = unique_workspace_root("retry-pr-movement");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 89,
        title: "Retry me".into(),
        url: Some("https://github.com/penso/polyphony/pull/89".into()),
        base_branch: "main".into(),
        head_branch: "feature/retry".into(),
        head_sha: "abc123".into(),
        checkout_ref: Some("refs/pull/89/head".into()),
        author_login: Some("alice".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::minutes(1)),
        is_draft: false,
    };
    let issue = synthetic_issue_for_pull_request_review(&trigger);
    let workspace_key = sanitize_workspace_key(&issue.identifier);
    let workspace_path = workspace_root.join(&workspace_key);
    let movement_id = "mov-retry-pr-1".to_string();
    let workspace_task_id = "task-retry-pr-setup".to_string();
    let review_task_id = "task-retry-pr-review".to_string();
    let now = Utc::now();

    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        Some(Arc::new(RecordingPullRequestCommenter::default())),
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .state
        .movements
        .insert(movement_id.clone(), Movement {
            id: movement_id.clone(),
            kind: MovementKind::PullRequestReview,
            issue_id: Some(issue.id.clone()),
            issue_identifier: Some(issue.identifier.clone()),
            title: issue.title.clone(),
            status: MovementStatus::Failed,
            pipeline_stage: None,
            manual_dispatch_directives: Some("Check auth".into()),
            workspace_key: Some(workspace_key.clone()),
            workspace_path: Some(workspace_path.clone()),
            review_target: Some(trigger.review_target()),
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: now,
        });
    service.state.tasks.insert(movement_id.clone(), vec![
        Task {
            id: workspace_task_id.clone(),
            movement_id: movement_id.clone(),
            title: "Creating worktree".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Research,
            status: TaskStatus::Failed,
            ordinal: 0,
            parent_id: None,
            agent_name: Some("orchestrator".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: Some(now),
            finished_at: Some(now),
            error: Some("auth failed".into()),
            created_at: now,
            updated_at: now,
        },
        Task {
            id: review_task_id.clone(),
            movement_id: movement_id.clone(),
            title: "Run PR review".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Review,
            status: TaskStatus::Cancelled,
            ordinal: 1,
            parent_id: None,
            agent_name: Some("reviewer".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: Some(now),
            error: Some("workspace setup failed".into()),
            created_at: now,
            updated_at: now,
        },
    ]);
    service.state.pull_request_retry_triggers.insert(
        issue.id.clone(),
        PullRequestTrigger::Review(trigger.clone()),
    );
    service.pending_movement_retries.push(movement_id.clone());

    service.process_pending_movement_retries().await;

    let movement = service
        .state
        .movements
        .get(&movement_id)
        .expect("movement should remain present");
    assert_eq!(movement.status, MovementStatus::InProgress);

    let tasks = service
        .state
        .tasks
        .get(&movement_id)
        .expect("tasks should remain present");
    assert_eq!(tasks[0].status, TaskStatus::Completed);
    assert_eq!(tasks[0].error, None);
    assert_eq!(tasks[1].status, TaskStatus::InProgress);
    assert_eq!(tasks[1].error, None);

    let running = service
        .state
        .running
        .get(&issue.id)
        .expect("review worker should be relaunched");
    assert_eq!(
        running.active_task_id.as_deref(),
        Some(review_task_id.as_str())
    );
    assert_eq!(running.movement_id.as_deref(), Some(movement_id.as_str()));
    assert_eq!(running.issue.identifier, issue.identifier);
}

#[tokio::test]
async fn movement_retry_recovers_stalled_pull_request_review_after_restart() {
    let workspace_root = unique_workspace_root("retry-pr-stalled");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
    );
    let (_tx, rx) = watch::channel(workflow.clone());
    let trigger = PullRequestReviewTrigger {
        provider: polyphony_core::ReviewProviderKind::Github,
        repository: "penso/polyphony".into(),
        number: 90,
        title: "Retry stale review".into(),
        url: Some("https://github.com/penso/polyphony/pull/90".into()),
        base_branch: "main".into(),
        head_branch: "feature/stalled".into(),
        head_sha: "def456".into(),
        checkout_ref: Some("refs/pull/90/head".into()),
        author_login: Some("alice".into()),
        approval_state: IssueApprovalState::Approved,
        labels: vec!["ready".into()],
        created_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        updated_at: Some(Utc::now() - chrono::Duration::minutes(1)),
        is_draft: false,
    };
    let issue = synthetic_issue_for_pull_request_review(&trigger);
    let workspace_key = sanitize_workspace_key(&issue.identifier);
    let workspace_path = workspace_root.join(&workspace_key);
    let movement_id = "mov-retry-pr-stalled".to_string();
    let workspace_task_id = "task-retry-pr-stalled-setup".to_string();
    let review_task_id = "task-retry-pr-stalled-review".to_string();
    let now = Utc::now();

    let mut service = RuntimeService::new(
        Arc::new(TestTracker::new(Vec::new())),
        None,
        Arc::new(NoopAgent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        Some(Arc::new(RecordingPullRequestCommenter::default())),
        None,
        None,
        None,
        rx,
    )
    .0;

    service
        .state
        .movements
        .insert(movement_id.clone(), Movement {
            id: movement_id.clone(),
            kind: MovementKind::PullRequestReview,
            issue_id: Some(issue.id.clone()),
            issue_identifier: Some(issue.identifier.clone()),
            title: issue.title.clone(),
            status: MovementStatus::InProgress,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: Some(workspace_key.clone()),
            workspace_path: Some(workspace_path.clone()),
            review_target: Some(trigger.review_target()),
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: now,
        });
    service.state.tasks.insert(movement_id.clone(), vec![
        Task {
            id: workspace_task_id.clone(),
            movement_id: movement_id.clone(),
            title: "Creating worktree".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Research,
            status: TaskStatus::Pending,
            ordinal: 0,
            parent_id: None,
            agent_name: Some("orchestrator".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        },
        Task {
            id: review_task_id.clone(),
            movement_id: movement_id.clone(),
            title: "Run PR review".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Review,
            status: TaskStatus::Cancelled,
            ordinal: 1,
            parent_id: None,
            agent_name: Some("reviewer".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: Some(now),
            error: Some("workspace setup failed".into()),
            created_at: now,
            updated_at: now,
        },
    ]);
    service
        .state
        .visible_review_triggers
        .insert(trigger.dedupe_key(), trigger.clone());
    service.pending_movement_retries.push(movement_id.clone());

    service.process_pending_movement_retries().await;

    let tasks = service
        .state
        .tasks
        .get(&movement_id)
        .expect("tasks should remain present");
    assert_eq!(tasks[0].status, TaskStatus::Completed);
    assert_eq!(tasks[1].status, TaskStatus::InProgress);
    assert!(
        service.state.running.contains_key(&issue.id),
        "stalled movement retry should relaunch the review worker"
    );
}

#[tokio::test]
async fn manual_dispatch_with_agent_name() {
    let workspace_root = unique_workspace_root("manual-agent");
    let issue = sample_issue("issue-agent-1", "FAC-AGENT-1", "Todo", "Agent test");
    let tracker = TestTracker::new(vec![issue.clone()]);
    let mut service = test_service(tracker, RecordingProvisioner::default(), &workspace_root);

    service
        .pending_manual_dispatches
        .push(crate::ManualDispatchRequest {
            issue_id: issue.id.clone(),
            agent_name: Some("mock".into()),
            directives: None,
        });

    service.process_manual_dispatches().await;

    assert!(
        service.state.running.contains_key(&issue.id),
        "manual dispatch with explicit agent name should work"
    );
    let running = service.state.running.get(&issue.id).unwrap();
    assert_eq!(
        running.agent_name, "mock",
        "should use the explicitly requested agent"
    );
}

#[tokio::test]
async fn manual_dispatch_directives_are_prepended_to_direct_issue_prompt() {
    let workspace_root = unique_workspace_root("manual-directives-direct");
    let workflow = test_workflow(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-directives-1",
        "FAC-DIRECTIVES-1",
        "Todo",
        "Verify before fixing",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let agent = RecordingSessionAgent::default();
    let agent_handle = agent.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        None,
        None,
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let directives = "Please verify this is actually a bug before fixing it.";

    service
        .dispatch_issue(
            workflow,
            issue.clone(),
            None,
            false,
            None,
            false,
            Some(directives),
        )
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;

    let prompts = agent_handle.prompts();
    assert!(!prompts.is_empty());
    assert!(prompts[0].starts_with("## Operator Directives (Highest Priority)"));
    assert!(prompts[0].contains(directives));
    assert!(prompts[0].contains("Test prompt"));
    let movement = service
        .state
        .movements
        .values()
        .find(|movement| movement.issue_id.as_deref() == Some(issue.id.as_str()))
        .expect("movement should be created for direct dispatch");
    assert_eq!(
        movement.manual_dispatch_directives.as_deref(),
        Some(directives)
    );
}

#[tokio::test]
async fn manual_dispatch_directives_reach_pipeline_router_and_worker_prompts() {
    let workspace_root = unique_workspace_root("manual-directives-pipeline");
    let workflow = pipeline_workflow_with_automation(&workspace_root);
    let (_tx, rx) = watch::channel(workflow.clone());
    let issue = sample_issue(
        "issue-directives-2",
        "DOG-DIRECTIVES-2",
        "Todo",
        "Plan with operator guidance",
    );
    let tracker = TestTracker::new(vec![issue.clone()]);
    let agent = ScriptedPipelineAgent::default();
    let agent_handle = agent.clone();
    let mut service = RuntimeService::new(
        Arc::new(tracker),
        None,
        Arc::new(agent),
        Arc::new(RecordingProvisioner::default()),
        Some(Arc::new(RecordingCommitter::new(None))),
        Some(Arc::new(RecordingPullRequestManager::new(PullRequestRef {
            repository: "penso/polyphony".into(),
            number: 42,
            url: Some("https://github.com/penso/polyphony/pull/42".into()),
        }))),
        None,
        None,
        None,
        None,
        rx,
    )
    .0;
    let directives = "Please verify this is a bug first, then make a plan to fix it.";

    service
        .dispatch_issue(
            workflow,
            issue.clone(),
            None,
            false,
            None,
            false,
            Some(directives),
        )
        .await
        .unwrap();
    handle_next_worker_message(&mut service).await;
    handle_next_worker_message(&mut service).await;

    let calls = agent_handle.recorded_calls();
    let router_prompt = calls
        .iter()
        .find(|(agent_name, _)| agent_name == "router")
        .map(|(_, prompt)| prompt)
        .expect("router prompt should be recorded");
    assert!(router_prompt.starts_with("## Operator Directives (Highest Priority)"));
    assert!(router_prompt.contains(directives));

    let worker_prompt = calls
        .iter()
        .find(|(agent_name, prompt)| {
            agent_name == "implementer" && prompt.contains("## Pipeline Task")
        })
        .map(|(_, prompt)| prompt)
        .expect("pipeline worker prompt should be recorded");
    assert!(worker_prompt.contains(directives));

    let movement = service
        .state
        .movements
        .values()
        .find(|movement| movement.issue_id.as_deref() == Some(issue.id.as_str()))
        .expect("movement should be created for pipeline dispatch");
    assert_eq!(
        movement.manual_dispatch_directives.as_deref(),
        Some(directives)
    );
}

#[tokio::test]
async fn stop_mode_blocks_retries() {
    let workspace_root = unique_workspace_root("stop-retry");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;
    // Manually insert a due retry.
    service.state.retrying.insert("issue-1".into(), RetryEntry {
        row: RetryRow {
            issue_id: "issue-1".into(),
            issue_identifier: "FAC-1".into(),
            attempt: 1,
            due_at: Utc::now() - chrono::Duration::seconds(10),
            error: Some("test error".into()),
        },
        due_at: Instant::now() - Duration::from_secs(10),
    });

    service.process_due_retries().await;

    assert!(
        service.state.retrying.contains_key("issue-1"),
        "retry should remain queued and not be processed in stop mode"
    );
    assert!(
        !service.state.running.contains_key("issue-1"),
        "no task should be dispatched from retry in stop mode"
    );
}

#[tokio::test]
async fn abort_all_drains_retry_queue() {
    let workspace_root = unique_workspace_root("stop-abort-retries");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.claim_issue("issue-1".to_string(), IssueClaimState::RetryQueued);
    service.state.retrying.insert("issue-1".into(), RetryEntry {
        row: RetryRow {
            issue_id: "issue-1".into(),
            issue_identifier: "FAC-1".into(),
            attempt: 2,
            due_at: Utc::now() + chrono::Duration::minutes(5),
            error: Some("transient".into()),
        },
        due_at: Instant::now() + Duration::from_secs(300),
    });

    service.abort_all().await;

    assert!(
        service.state.retrying.is_empty(),
        "abort_all should drain the retry queue"
    );
    assert!(
        !service.is_claimed("issue-1"),
        "abort_all should release claims for drained retries"
    );
}

#[tokio::test]
async fn finish_running_in_stop_mode_does_not_schedule_retry_on_success() {
    let workspace_root = unique_workspace_root("stop-finish-success");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-5", "FAC-5", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-5");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            None,
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Succeeded,
                turns_completed: 1,
                error: None,
                final_issue_state: Some("Human Review".into()),
            },
        )
        .await
        .unwrap();

    assert!(
        service.state.completed.contains(&issue.id),
        "issue should still be marked as completed"
    );
    assert!(
        !service.state.retrying.contains_key(&issue.id),
        "no retry should be scheduled in stop mode"
    );
    assert!(
        !service.is_claimed(&issue.id),
        "issue claim should be released in stop mode"
    );
}

#[tokio::test]
async fn finish_running_in_stop_mode_does_not_schedule_retry_on_failure() {
    let workspace_root = unique_workspace_root("stop-finish-fail");
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(TestTracker::new(Vec::new()), provisioner, &workspace_root);
    let issue = sample_issue("issue-6", "FAC-6", "Todo", "Work");
    let workspace_path = workspace_root.join("FAC-6");
    service.state.running.insert(
        issue.id.clone(),
        make_running_task(issue.clone(), workspace_path),
    );
    service.claim_issue(issue.id.clone(), IssueClaimState::Running);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Stop;

    service
        .finish_running(
            issue.id.clone(),
            issue.identifier.clone(),
            Some(1),
            Utc::now(),
            AgentRunResult {
                status: AttemptStatus::Failed,
                turns_completed: 0,
                error: Some("test failure".into()),
                final_issue_state: None,
            },
        )
        .await
        .unwrap();

    assert!(
        !service.state.retrying.contains_key(&issue.id),
        "no retry should be scheduled in stop mode after failure"
    );
    assert!(
        !service.is_claimed(&issue.id),
        "issue claim should be released in stop mode after failure"
    );
}

// ---------------------------------------------------------------------------
// Movement deduplication tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_reuses_active_movement_for_same_issue() {
    let workspace_root = unique_workspace_root("movement-reuse");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    // First dispatch creates a movement.
    service.tick().await;
    assert!(service.state.running.contains_key("issue-1"));
    let movement_count_after_first = service.state.movements.len();
    assert_eq!(
        movement_count_after_first, 1,
        "first dispatch should create one movement"
    );

    // Simulate the task finishing with success so it gets a continuation retry.
    handle_next_worker_message(&mut service).await;

    // The issue should now be in the retry queue with a movement still present.
    assert!(
        service.state.retrying.contains_key("issue-1"),
        "successful finish should schedule a continuation retry"
    );

    // Process the retry (it fires after 1 second but we can trigger manually).
    service.state.retrying.get_mut("issue-1").unwrap().due_at =
        Instant::now() - Duration::from_secs(1);
    service.process_due_retries().await;

    // After the retry dispatch, there should still be only one movement.
    let movement_count_after_retry = service
        .state
        .movements
        .values()
        .filter(|m| m.issue_id.as_deref() == Some("issue-1"))
        .count();
    assert_eq!(
        movement_count_after_retry, 1,
        "retry dispatch should reuse the existing movement, not create a duplicate"
    );
}

#[tokio::test]
async fn dispatch_acknowledges_issue_on_first_attempt() {
    let workspace_root = unique_workspace_root("ack-dispatch");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    service.tick().await;
    assert!(service.state.running.contains_key("issue-1"));

    let acked = tracker.acknowledged_issues();
    assert_eq!(
        acked,
        vec!["issue-1"],
        "issue should be acknowledged on first dispatch"
    );
}

#[tokio::test]
async fn dispatch_does_not_acknowledge_on_retry() {
    let workspace_root = unique_workspace_root("ack-retry");
    let tracker = TestTracker::new(vec![sample_issue("issue-1", "FAC-1", "Todo", "First")]);
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker.clone(), provisioner, &workspace_root);
    service.state.dispatch_mode = polyphony_core::DispatchMode::Automatic;

    // First dispatch — should acknowledge.
    service.tick().await;
    assert_eq!(tracker.acknowledged_issues().len(), 1);

    // Simulate worker finishing with success so it queues a retry.
    handle_next_worker_message(&mut service).await;
    assert!(service.state.retrying.contains_key("issue-1"));

    // Trigger retry — should NOT acknowledge again.
    service.state.retrying.get_mut("issue-1").unwrap().due_at =
        Instant::now() - Duration::from_secs(1);
    service.process_due_retries().await;

    assert_eq!(
        tracker.acknowledged_issues().len(),
        1,
        "retry dispatch must not re-acknowledge the issue"
    );
}

#[test]
fn find_existing_movement_prefers_active_over_terminal() {
    let workspace_root = unique_workspace_root("movement-find-existing");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    let now = Utc::now();

    // Insert a delivered (terminal) movement for the issue.
    service
        .state
        .movements
        .insert("mov-delivered".into(), Movement {
            id: "mov-delivered".into(),
            kind: MovementKind::IssueDelivery,
            issue_id: Some("issue-1".into()),
            issue_identifier: Some("FAC-1".into()),
            title: "Delivered work".into(),
            status: MovementStatus::Delivered,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: None,
            workspace_path: None,
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: now,
        });

    // Even a terminal movement should be found — prevents duplicate movements
    // when an issue is re-dispatched via continuation retry.
    assert_eq!(
        service.find_existing_movement_for_issue("issue-1"),
        Some("mov-delivered".into()),
        "delivered movement should be found when no active one exists"
    );

    // Insert an in-progress (active) movement — should be preferred.
    service
        .state
        .movements
        .insert("mov-active".into(), Movement {
            id: "mov-active".into(),
            kind: MovementKind::IssueDelivery,
            issue_id: Some("issue-1".into()),
            issue_identifier: Some("FAC-1".into()),
            title: "Active work".into(),
            status: MovementStatus::InProgress,
            pipeline_stage: None,
            manual_dispatch_directives: None,
            workspace_key: None,
            workspace_path: None,
            review_target: None,
            deliverable: None,
            created_at: now,
            activity_log: Vec::new(),
            cancel_reason: None,
            steps: Vec::new(),
            updated_at: now,
        });

    assert_eq!(
        service.find_existing_movement_for_issue("issue-1"),
        Some("mov-active".into()),
        "active movement should be preferred over terminal one"
    );

    // No movement for a different issue.
    assert!(
        service
            .find_existing_movement_for_issue("issue-999")
            .is_none(),
        "should return None for an issue with no movements"
    );
}

// ---------------------------------------------------------------------------
// Dispatch mode persistence tests
// ---------------------------------------------------------------------------

#[test]
fn restore_bootstrap_preserves_persisted_dispatch_mode() {
    let workspace_root = unique_workspace_root("mode-persist");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);

    // Default before bootstrap is Manual (from test config).
    assert_eq!(service.state.dispatch_mode, DispatchMode::Manual);
    assert!(!service.state.bootstrap_restored);

    let now = Utc::now();
    service.restore_bootstrap(StoreBootstrap {
        snapshot: Some(RuntimeSnapshot {
            generated_at: now,
            counts: SnapshotCounts::default(),
            cadence: RuntimeCadence::default(),
            visible_issues: Vec::new(),
            visible_triggers: Vec::new(),
            approved_issue_keys: Vec::new(),
            running: Vec::new(),
            agent_history: Vec::new(),
            retrying: Vec::new(),
            codex_totals: CodexTotals::default(),
            rate_limits: None,
            throttles: Vec::new(),
            budgets: Vec::new(),
            agent_catalogs: Vec::new(),
            saved_contexts: Vec::new(),
            recent_events: Vec::new(),
            pending_user_interactions: Vec::new(),
            movements: Vec::new(),
            tasks: Vec::new(),
            loading: LoadingState::default(),
            dispatch_mode: DispatchMode::Stop,
            tracker_kind: TrackerKind::default(),
            tracker_connection: None,
            from_cache: false,
            cached_at: None,
            agent_profile_names: Vec::new(),
            agent_profiles: Vec::new(),
        }),
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        movements: std::collections::HashMap::new(),
        tasks: std::collections::HashMap::new(),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        run_history: Vec::new(),
    });

    assert!(service.state.bootstrap_restored);
    assert_eq!(
        service.state.dispatch_mode,
        DispatchMode::Stop,
        "dispatch mode should be restored from snapshot"
    );
}

#[tokio::test]
async fn normalize_restored_in_progress_movements_marks_stale_running_task_failed() {
    let workspace_root = unique_workspace_root("normalize-stale-running-task");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    let movement_id = "mov-stale-running".to_string();
    let task_id = "task-stale-running".to_string();

    service.restore_bootstrap(StoreBootstrap {
        snapshot: None,
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        movements: std::collections::HashMap::from([(
            movement_id.clone(),
            polyphony_core::Movement {
                id: movement_id.clone(),
                kind: polyphony_core::MovementKind::PullRequestReview,
                issue_id: Some("issue-89".into()),
                issue_identifier: Some("penso/arbor#89".into()),
                title: "Review PR".into(),
                status: polyphony_core::MovementStatus::Cancelled,
                pipeline_stage: None,
                manual_dispatch_directives: None,
                workspace_key: Some("penso_arbor_89".into()),
                workspace_path: Some(workspace_root.join("penso_arbor_89")),
                review_target: None,
                deliverable: None,
                created_at: now,
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
                updated_at: now,
            },
        )]),
        tasks: std::collections::HashMap::from([(task_id.clone(), polyphony_core::Task {
            id: task_id.clone(),
            movement_id: movement_id.clone(),
            title: "Run PR review".into(),
            description: None,
            activity_log: Vec::new(),
            category: polyphony_core::TaskCategory::Review,
            status: polyphony_core::TaskStatus::InProgress,
            ordinal: 1,
            parent_id: None,
            agent_name: Some("reviewer".into()),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: Some(now),
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        })]),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        run_history: Vec::new(),
    });

    service
        .normalize_restored_in_progress_movements()
        .await
        .unwrap();

    let movement = service.state.movements.get(&movement_id).unwrap();
    assert_eq!(movement.status, polyphony_core::MovementStatus::Failed);
    let task = service
        .state
        .tasks
        .get(&movement_id)
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(task.status, polyphony_core::TaskStatus::Failed);
    assert_eq!(
        task.error.as_deref(),
        Some("restored without an active agent session; retry the movement to continue")
    );
    assert!(task.finished_at.is_some());
}

#[tokio::test]
async fn normalize_restored_in_progress_movements_marks_first_pending_task_failed() {
    let workspace_root = unique_workspace_root("normalize-stale-pending-task");
    let tracker = TestTracker::new(Vec::new());
    let provisioner = RecordingProvisioner::default();
    let mut service = test_service(tracker, provisioner, &workspace_root);
    let now = Utc::now();
    let movement_id = "mov-stale-pending".to_string();
    let workspace_task_id = "task-worktree".to_string();
    let review_task_id = "task-review".to_string();

    service.restore_bootstrap(StoreBootstrap {
        snapshot: None,
        retrying: std::collections::HashMap::new(),
        throttles: std::collections::HashMap::new(),
        budgets: std::collections::HashMap::new(),
        saved_contexts: std::collections::HashMap::new(),
        recent_events: Vec::new(),
        movements: std::collections::HashMap::from([(
            movement_id.clone(),
            polyphony_core::Movement {
                id: movement_id.clone(),
                kind: polyphony_core::MovementKind::PullRequestReview,
                issue_id: Some("issue-89".into()),
                issue_identifier: Some("penso/arbor#89".into()),
                title: "Review PR".into(),
                status: polyphony_core::MovementStatus::InProgress,
                pipeline_stage: None,
                manual_dispatch_directives: None,
                workspace_key: Some("penso_arbor_89".into()),
                workspace_path: Some(workspace_root.join("penso_arbor_89")),
                review_target: None,
                deliverable: None,
                created_at: now,
                activity_log: Vec::new(),
                cancel_reason: None,
                steps: Vec::new(),
                updated_at: now,
            },
        )]),
        tasks: std::collections::HashMap::from([
            (workspace_task_id.clone(), polyphony_core::Task {
                id: workspace_task_id.clone(),
                movement_id: movement_id.clone(),
                title: "Creating worktree".into(),
                description: None,
                activity_log: Vec::new(),
                category: polyphony_core::TaskCategory::Research,
                status: polyphony_core::TaskStatus::Completed,
                ordinal: 0,
                parent_id: None,
                agent_name: Some("orchestrator".into()),
                session_id: None,
                thread_id: None,
                turns_completed: 0,
                tokens: TokenUsage::default(),
                started_at: Some(now),
                finished_at: Some(now),
                error: None,
                created_at: now,
                updated_at: now,
            }),
            (review_task_id.clone(), polyphony_core::Task {
                id: review_task_id.clone(),
                movement_id: movement_id.clone(),
                title: "Run PR review".into(),
                description: None,
                activity_log: Vec::new(),
                category: polyphony_core::TaskCategory::Review,
                status: polyphony_core::TaskStatus::Pending,
                ordinal: 1,
                parent_id: None,
                agent_name: Some("reviewer".into()),
                session_id: None,
                thread_id: None,
                turns_completed: 0,
                tokens: TokenUsage::default(),
                started_at: None,
                finished_at: None,
                error: None,
                created_at: now,
                updated_at: now,
            }),
        ]),
        reviewed_pull_request_heads: std::collections::HashMap::new(),
        run_history: Vec::new(),
    });

    service
        .normalize_restored_in_progress_movements()
        .await
        .unwrap();

    let movement = service.state.movements.get(&movement_id).unwrap();
    assert_eq!(movement.status, polyphony_core::MovementStatus::Failed);
    let tasks = service.state.tasks.get(&movement_id).unwrap();
    let workspace_task = tasks
        .iter()
        .find(|task| task.id == workspace_task_id)
        .unwrap();
    assert_eq!(workspace_task.status, polyphony_core::TaskStatus::Completed);
    let review_task = tasks.iter().find(|task| task.id == review_task_id).unwrap();
    assert_eq!(review_task.status, polyphony_core::TaskStatus::Failed);
    assert_eq!(
        review_task.error.as_deref(),
        Some("restored without an active agent session; retry the movement to continue")
    );
}
