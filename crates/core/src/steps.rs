use std::{collections::HashMap, fmt};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TaskId;

/// A discrete unit of work in a run's lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Run the planner agent to produce a plan.json.
    PlannerRun,
    /// Run an agent for a specific task.
    AgentRun,
    /// Commit workspace changes to the local branch.
    Commit,
    /// Push the branch to the remote.
    Push,
    /// Create or update the pull request / merge request.
    CreatePullRequest,
    /// Run the optional self-review agent pass.
    ReviewPass,
    /// Post a comment on the PR (review body).
    PostReviewComment,
    /// Send handoff feedback (Slack, etc.).
    SendFeedback,
    /// Run after-outcome hooks.
    AfterOutcomeHooks,
}

impl fmt::Display for StepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::PlannerRun => "planner_run",
            Self::AgentRun => "agent_run",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::CreatePullRequest => "create_pull_request",
            Self::ReviewPass => "review_pass",
            Self::PostReviewComment => "post_review_comment",
            Self::SendFeedback => "send_feedback",
            Self::AfterOutcomeHooks => "after_outcome_hooks",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl fmt::Display for StepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        };
        f.write_str(s)
    }
}

/// A single recorded step in a run's execution log.
///
/// Persisted on the Run so that the orchestrator can resume from the
/// first non-succeeded step after a restart or retry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub kind: StepKind,
    pub status: StepStatus,
    /// Which task this step belongs to (for `AgentRun` steps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Ordinal within the step sequence (0-indexed).
    pub ordinal: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Freeform output (commit SHA, PR number, branch name, etc.)
    /// so later steps can read from earlier ones without re-computing.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output: HashMap<String, Value>,
}

impl StepRecord {
    pub fn new(kind: StepKind, ordinal: u32) -> Self {
        Self {
            kind,
            status: StepStatus::Pending,
            task_id: None,
            ordinal,
            started_at: None,
            finished_at: None,
            error: None,
            output: HashMap::new(),
        }
    }

    pub fn with_task_id(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn mark_running(&mut self) {
        self.status = StepStatus::Running;
        self.started_at = Some(Utc::now());
    }

    pub fn mark_succeeded(&mut self) {
        self.status = StepStatus::Succeeded;
        self.finished_at = Some(Utc::now());
    }

    pub fn mark_succeeded_with_output(&mut self, output: HashMap<String, Value>) {
        self.status = StepStatus::Succeeded;
        self.finished_at = Some(Utc::now());
        self.output = output;
    }

    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.status = StepStatus::Failed;
        self.error = Some(error.into());
        self.finished_at = Some(Utc::now());
    }

    pub fn mark_skipped(&mut self) {
        self.status = StepStatus::Skipped;
        self.finished_at = Some(Utc::now());
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.status, StepStatus::Succeeded | StepStatus::Skipped)
    }
}

/// Build the step sequence for a pipeline run that delivers code.
pub fn build_delivery_steps(
    task_ids: &[TaskId],
    automation_enabled: bool,
    has_review_agent: bool,
) -> Vec<StepRecord> {
    let mut steps = Vec::new();
    let mut ordinal = 0u32;

    for task_id in task_ids {
        steps.push(StepRecord::new(StepKind::AgentRun, ordinal).with_task_id(task_id.clone()));
        ordinal += 1;
    }

    if automation_enabled {
        steps.push(StepRecord::new(StepKind::Commit, ordinal));
        ordinal += 1;
        steps.push(StepRecord::new(StepKind::Push, ordinal));
        ordinal += 1;
        steps.push(StepRecord::new(StepKind::CreatePullRequest, ordinal));
        ordinal += 1;
        if has_review_agent {
            steps.push(StepRecord::new(StepKind::ReviewPass, ordinal));
            ordinal += 1;
            steps.push(StepRecord::new(StepKind::PostReviewComment, ordinal));
            ordinal += 1;
        }
        steps.push(StepRecord::new(StepKind::SendFeedback, ordinal));
        ordinal += 1;
    }

    steps.push(StepRecord::new(StepKind::AfterOutcomeHooks, ordinal));
    steps
}

/// Build steps for a planner-first pipeline.
pub fn build_planner_steps() -> Vec<StepRecord> {
    vec![StepRecord::new(StepKind::PlannerRun, 0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_record_lifecycle() {
        let mut step = StepRecord::new(StepKind::Push, 2);
        assert_eq!(step.status, StepStatus::Pending);
        assert!(!step.is_complete());

        step.mark_running();
        assert_eq!(step.status, StepStatus::Running);
        assert!(step.started_at.is_some());

        step.mark_succeeded();
        assert_eq!(step.status, StepStatus::Succeeded);
        assert!(step.finished_at.is_some());
        assert!(step.is_complete());
    }

    #[test]
    fn step_record_failure() {
        let mut step = StepRecord::new(StepKind::Push, 0);
        step.mark_running();
        step.mark_failed("SSH auth failed");
        assert_eq!(step.status, StepStatus::Failed);
        assert_eq!(step.error.as_deref(), Some("SSH auth failed"));
        assert!(!step.is_complete());
    }

    #[test]
    fn step_record_skip() {
        let mut step = StepRecord::new(StepKind::ReviewPass, 0);
        step.mark_skipped();
        assert!(step.is_complete());
    }

    #[test]
    fn build_delivery_steps_without_automation() {
        let steps = build_delivery_steps(&["task-1".into()], false, false);
        assert_eq!(steps.len(), 2); // AgentRun + AfterOutcomeHooks
        assert_eq!(steps[0].kind, StepKind::AgentRun);
        assert_eq!(steps[0].task_id.as_deref(), Some("task-1"));
        assert_eq!(steps[1].kind, StepKind::AfterOutcomeHooks);
    }

    #[test]
    fn build_delivery_steps_with_automation() {
        let steps = build_delivery_steps(&["task-1".into(), "task-2".into()], true, false);
        assert_eq!(steps.len(), 7);
        assert_eq!(steps[0].kind, StepKind::AgentRun);
        assert_eq!(steps[1].kind, StepKind::AgentRun);
        assert_eq!(steps[2].kind, StepKind::Commit);
        assert_eq!(steps[3].kind, StepKind::Push);
        assert_eq!(steps[4].kind, StepKind::CreatePullRequest);
        assert_eq!(steps[5].kind, StepKind::SendFeedback);
        assert_eq!(steps[6].kind, StepKind::AfterOutcomeHooks);
    }

    #[test]
    fn build_delivery_steps_with_review() {
        let steps = build_delivery_steps(&["task-1".into()], true, true);
        assert_eq!(steps.len(), 8);
        assert_eq!(steps[4].kind, StepKind::ReviewPass);
        assert_eq!(steps[5].kind, StepKind::PostReviewComment);
    }

    #[test]
    fn build_planner_steps_has_single_step() {
        let steps = build_planner_steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, StepKind::PlannerRun);
    }

    #[test]
    fn step_kind_display() {
        assert_eq!(StepKind::Push.to_string(), "push");
        assert_eq!(
            StepKind::CreatePullRequest.to_string(),
            "create_pull_request"
        );
        assert_eq!(StepKind::AgentRun.to_string(), "agent_run");
    }
}
