use std::sync::Arc;

use crate::{prelude::*, *};

#[async_trait]
pub trait IssueTracker: Send + Sync {
    fn component_key(&self) -> String;
    async fn fetch_candidate_issues(&self, query: &TrackerQuery) -> Result<Vec<Issue>, Error>;
    async fn fetch_issues_by_states(
        &self,
        project_slug: Option<&str>,
        states: &[String],
    ) -> Result<Vec<Issue>, Error>;
    async fn fetch_issues_by_ids(&self, issue_ids: &[String]) -> Result<Vec<Issue>, Error>;
    async fn fetch_issue_states_by_ids(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<IssueStateUpdate>, Error>;
    async fn fetch_budget(&self) -> Result<Option<BudgetSnapshot>, Error> {
        Ok(None)
    }
    async fn fetch_connection_status(&self) -> Result<Option<TrackerConnectionStatus>, Error> {
        Ok(None)
    }
    async fn ensure_issue_workflow_tracking(&self, _issue: &Issue) -> Result<(), Error> {
        Ok(())
    }
    async fn update_issue_workflow_status(
        &self,
        _issue: &Issue,
        _status: &str,
    ) -> Result<(), Error> {
        Ok(())
    }
    async fn create_issue(&self, _request: &CreateIssueRequest) -> Result<Issue, Error> {
        Err(Error::Adapter("create_issue not supported".into()))
    }
    async fn update_issue(&self, _request: &UpdateIssueRequest) -> Result<Issue, Error> {
        Err(Error::Adapter("update_issue not supported".into()))
    }
    async fn comment_on_issue(
        &self,
        _request: &AddIssueCommentRequest,
    ) -> Result<IssueComment, Error> {
        Err(Error::Adapter("comment_on_issue not supported".into()))
    }
    /// Fetch the state of a pull request ("open", "closed", or "merged").
    /// Default implementation returns `Ok(None)` (not supported by this tracker).
    async fn fetch_pull_request_state(
        &self,
        _repository: &str,
        _number: u64,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }
}

#[async_trait]
pub trait PullRequestTriggerSource: Send + Sync {
    fn component_key(&self) -> String;
    async fn fetch_triggers(&self) -> Result<Vec<PullRequestTrigger>, Error>;
}

#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn component_key(&self) -> String;

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, Error> {
        Ok(None)
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, Error>;
    async fn fetch_budgets(&self, _agents: &[AgentDefinition]) -> Result<BudgetPollResult, Error> {
        Ok(BudgetPollResult::default())
    }
    async fn discover_models(
        &self,
        _agents: &[AgentDefinition],
    ) -> Result<Vec<AgentModelCatalog>, Error> {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait AgentProviderRuntime: Send + Sync {
    fn runtime_key(&self) -> String;
    fn supports(&self, agent: &AgentDefinition) -> bool;

    async fn start_session(
        &self,
        _spec: AgentRunSpec,
        _event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Option<Box<dyn AgentSession>>, Error> {
        Ok(None)
    }

    async fn run(
        &self,
        spec: AgentRunSpec,
        event_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentRunResult, Error>;
    async fn fetch_budget(
        &self,
        _agent: &AgentDefinition,
    ) -> Result<Option<BudgetSnapshot>, Error> {
        Ok(None)
    }
    async fn discover_models(
        &self,
        _agent: &AgentDefinition,
    ) -> Result<Option<AgentModelCatalog>, Error> {
        Ok(None)
    }
}

#[async_trait]
pub trait WorkspaceProvisioner: Send + Sync {
    fn component_key(&self) -> String;
    fn set_interaction_reporter(&self, _reporter: Option<Arc<dyn UserInteractionReporter>>) {}
    fn set_progress_reporter(&self, _reporter: Option<Arc<dyn WorkspaceProgressReporter>>) {}
    async fn ensure_workspace(&self, request: WorkspaceRequest) -> Result<Workspace, Error>;
    async fn cleanup_workspace(&self, request: WorkspaceRequest) -> Result<(), Error>;
}

pub trait UserInteractionReporter: Send + Sync {
    fn begin(&self, interaction: UserInteractionRequest);
    fn end(&self, interaction_id: &str);
}

pub trait WorkspaceProgressReporter: Send + Sync {
    fn log(&self, update: WorkspaceProgressUpdate);
}

#[async_trait]
pub trait PullRequestCommenter: Send + Sync {
    fn component_key(&self) -> String;
    async fn comment_on_pull_request(
        &self,
        pull_request: &PullRequestRef,
        body: &str,
    ) -> Result<(), Error>;
    async fn sync_pull_request_review(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
        comments: &[PullRequestReviewComment],
        commit_sha: &str,
        verdict: crate::ReviewVerdict,
    ) -> Result<(), Error> {
        let _ = comments;
        let _ = commit_sha;
        let _ = verdict;
        self.sync_pull_request_comment(pull_request, marker, body)
            .await
    }
    async fn sync_pull_request_comment(
        &self,
        pull_request: &PullRequestRef,
        marker: &str,
        body: &str,
    ) -> Result<(), Error> {
        let _ = marker;
        self.comment_on_pull_request(pull_request, body).await
    }
}

#[async_trait]
pub trait PullRequestManager: Send + Sync {
    fn component_key(&self) -> String;
    async fn ensure_pull_request(
        &self,
        request: &PullRequestRequest,
    ) -> Result<PullRequestRef, Error>;
    async fn merge_pull_request(&self, pull_request: &PullRequestRef) -> Result<(), Error>;
}

#[async_trait]
pub trait WorkspaceCommitter: Send + Sync {
    fn component_key(&self) -> String;
    fn set_interaction_reporter(&self, _reporter: Option<Arc<dyn UserInteractionReporter>>) {}
    async fn commit_and_push(
        &self,
        request: &WorkspaceCommitRequest,
    ) -> Result<Option<WorkspaceCommitResult>, Error>;
}

#[async_trait]
pub trait FeedbackSink: Send + Sync {
    fn component_key(&self) -> String;
    fn descriptor(&self) -> FeedbackChannelDescriptor;
    async fn send(&self, notification: &FeedbackNotification) -> Result<(), Error>;
}

#[async_trait]
pub trait StateBootstrapStore: Send + Sync {
    async fn bootstrap(&self) -> Result<StoreBootstrap, Error>;
}

#[async_trait]
pub trait SnapshotPersistence: Send + Sync {
    async fn save_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), Error>;
}

#[async_trait]
pub trait RunPersistence: Send + Sync {
    async fn record_run(&self, run: &PersistedRunRecord) -> Result<(), Error>;
}

#[async_trait]
pub trait BudgetPersistence: Send + Sync {
    async fn record_budget(&self, snapshot: &BudgetSnapshot) -> Result<(), Error>;
}

#[async_trait]
pub trait WorkflowPersistence: Send + Sync {
    async fn save_movement(&self, _movement: &Movement) -> Result<(), Error> {
        Ok(())
    }
    async fn save_task(&self, _task: &Task) -> Result<(), Error> {
        Ok(())
    }
    async fn load_movements(&self) -> Result<Vec<Movement>, Error> {
        Ok(Vec::new())
    }
    async fn load_tasks_for_movement(&self, _movement_id: &str) -> Result<Vec<Task>, Error> {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait ReviewPersistence: Send + Sync {
    async fn save_reviewed_pull_request_head(
        &self,
        _head: &ReviewedPullRequestHead,
    ) -> Result<(), Error> {
        Ok(())
    }
    async fn load_reviewed_pull_request_heads(
        &self,
    ) -> Result<Vec<ReviewedPullRequestHead>, Error> {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn bootstrap(&self) -> Result<StoreBootstrap, Error>;
    async fn save_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), Error>;
    async fn record_run(&self, run: &PersistedRunRecord) -> Result<(), Error>;
    async fn record_budget(&self, snapshot: &BudgetSnapshot) -> Result<(), Error>;

    async fn save_movement(&self, movement: &Movement) -> Result<(), Error>;
    async fn save_task(&self, task: &Task) -> Result<(), Error>;
    async fn load_movements(&self) -> Result<Vec<Movement>, Error>;
    async fn load_tasks_for_movement(&self, movement_id: &str) -> Result<Vec<Task>, Error>;
    async fn save_reviewed_pull_request_head(
        &self,
        head: &ReviewedPullRequestHead,
    ) -> Result<(), Error>;
    async fn load_reviewed_pull_request_heads(&self)
    -> Result<Vec<ReviewedPullRequestHead>, Error>;
}

#[async_trait]
impl<T> StateBootstrapStore for T
where
    T: StateStore + ?Sized,
{
    async fn bootstrap(&self) -> Result<StoreBootstrap, Error> {
        StateStore::bootstrap(self).await
    }
}

#[async_trait]
impl<T> SnapshotPersistence for T
where
    T: StateStore + ?Sized,
{
    async fn save_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), Error> {
        StateStore::save_snapshot(self, snapshot).await
    }
}

#[async_trait]
impl<T> RunPersistence for T
where
    T: StateStore + ?Sized,
{
    async fn record_run(&self, run: &PersistedRunRecord) -> Result<(), Error> {
        StateStore::record_run(self, run).await
    }
}

#[async_trait]
impl<T> BudgetPersistence for T
where
    T: StateStore + ?Sized,
{
    async fn record_budget(&self, snapshot: &BudgetSnapshot) -> Result<(), Error> {
        StateStore::record_budget(self, snapshot).await
    }
}

#[async_trait]
impl<T> WorkflowPersistence for T
where
    T: StateStore + ?Sized,
{
    async fn save_movement(&self, movement: &Movement) -> Result<(), Error> {
        StateStore::save_movement(self, movement).await
    }

    async fn save_task(&self, task: &Task) -> Result<(), Error> {
        StateStore::save_task(self, task).await
    }

    async fn load_movements(&self) -> Result<Vec<Movement>, Error> {
        StateStore::load_movements(self).await
    }

    async fn load_tasks_for_movement(&self, movement_id: &str) -> Result<Vec<Task>, Error> {
        StateStore::load_tasks_for_movement(self, movement_id).await
    }
}

#[async_trait]
impl<T> ReviewPersistence for T
where
    T: StateStore + ?Sized,
{
    async fn save_reviewed_pull_request_head(
        &self,
        head: &ReviewedPullRequestHead,
    ) -> Result<(), Error> {
        StateStore::save_reviewed_pull_request_head(self, head).await
    }

    async fn load_reviewed_pull_request_heads(
        &self,
    ) -> Result<Vec<ReviewedPullRequestHead>, Error> {
        StateStore::load_reviewed_pull_request_heads(self).await
    }
}
