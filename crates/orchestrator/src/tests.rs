use {
    crate::{helpers::*, prelude::*, *},
    std::{
        collections::VecDeque,
        fs,
        path::Path,
        sync::{Arc, Mutex},
    },
};

use {
    async_trait::async_trait,
    polyphony_core::{
        AgentSession, IssueAuthor, IssueComment, IssueStateUpdate, PullRequestRef, Workspace,
        WorkspaceRequest,
    },
    polyphony_workflow::load_workflow,
    tokio::sync::watch,
};

#[derive(Clone)]
struct TestTracker {
    issues: Arc<Mutex<HashMap<String, Issue>>>,
    workflow_updates: Arc<Mutex<Vec<String>>>,
    fetch_by_ids_calls: Arc<Mutex<u32>>,
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
        }
    }

    fn recorded_workflow_updates(&self) -> Vec<String> {
        self.workflow_updates.lock().unwrap().clone()
    }

    fn fetch_by_ids_calls(&self) -> u32 {
        *self.fetch_by_ids_calls.lock().unwrap()
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
        Ok(())
    }
}

fn test_workflow(workspace_root: &Path) -> LoadedWorkflow {
    test_workflow_with_front_matter(
        workspace_root,
        "---\ntracker:\n  kind: mock\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: mock\n  profiles:\n    mock:\n      kind: mock\n      transport: mock\n      command: mock\n---\nTest prompt\n",
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
    let (_tx, rx) = watch::channel(workflow);
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
async fn tick_tracks_visible_issues_when_no_agents_are_configured() {
    let workspace_root = unique_workspace_root("visible-issues");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: none\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  by_state: {}\n  by_label: {}\n  profiles: {}\n---\nTest prompt\n",
    );
    let (_tx, rx) = watch::channel(workflow);
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
async fn completed_pull_request_reviews_are_marked_reviewed_and_not_redispatched() {
    let workspace_root = unique_workspace_root("pr-review");
    let workflow = test_workflow_with_front_matter(
        &workspace_root,
        "---\ntracker:\n  kind: github\n  repository: penso/polyphony\n  api_key: token\npolling:\n  interval_ms: 1000\nworkspace:\n  root: __ROOT__\nagents:\n  default: reviewer\n  profiles:\n    reviewer:\n      kind: claude\n      transport: local_cli\n      command: claude -p --verbose --dangerously-skip-permissions\nreview_triggers:\n  pr_reviews:\n    enabled: true\n    agent: reviewer\n    debounce_seconds: 1\n---\nPrompt\n",
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
            workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
            workspace_path: Some(workspace_path.clone()),
            review_target: Some(trigger.review_target()),
            deliverable: None,
            created_at: Utc::now(),
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
        .dispatch_issue(workflow, issue.clone(), Some(2), true, None, false)
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
