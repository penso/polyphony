use crate::{prelude::*, *};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrackerKind {
    #[default]
    None,
    Linear,
    Github,
    Beads,
    Mock,
}

impl fmt::Display for TrackerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Variants are single PascalCase words; lowercasing Debug matches serde rename_all.
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AttemptStatus {
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CancelledByReconciliation,
    CancelledByUser,
}

impl fmt::Display for AttemptStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MovementStatus {
    Pending,
    Planning,
    InProgress,
    Review,
    Delivered,
    Failed,
    Cancelled,
}

impl fmt::Display for MovementStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Planning => "planning",
            Self::InProgress => "in_progress",
            Self::Review => "review",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

/// Tracks where a pipeline movement is in its lifecycle.
///
/// Persisted on the `Movement` so the orchestrator can resume correctly after
/// a restart without re-running stages that already completed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    /// The router/planner agent has not yet run.
    Planning,
    /// The planner finished and created tasks; implementers are running.
    Executing,
    /// All tasks completed; handoff / deliverable creation is in progress.
    Completing,
}

impl fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Completing => "completing",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MovementKind {
    #[default]
    IssueDelivery,
    PullRequestReview,
    PullRequestCommentReview,
}

impl fmt::Display for MovementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::IssueDelivery => "issue_delivery",
            Self::PullRequestReview => "pull_request_review",
            Self::PullRequestCommentReview => "pull_request_comment_review",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    Research,
    Coding,
    Testing,
    Documentation,
    Review,
}

impl fmt::Display for TaskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Research => "research",
            Self::Coding => "coding",
            Self::Testing => "testing",
            Self::Documentation => "documentation",
            Self::Review => "review",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableKind {
    GithubPullRequest,
    GitlabMergeRequest,
    LocalBranch,
    Patch,
}

impl fmt::Display for DeliverableKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::GithubPullRequest => "github_pull_request",
            Self::GitlabMergeRequest => "gitlab_merge_request",
            Self::LocalBranch => "local_branch",
            Self::Patch => "patch",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableStatus {
    Pending,
    Open,
    Merged,
    Closed,
}

impl fmt::Display for DeliverableStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableDecision {
    #[default]
    Waiting,
    Accepted,
    Rejected,
}

impl fmt::Display for DeliverableDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Waiting => "waiting",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deliverable {
    pub kind: DeliverableKind,
    pub status: DeliverableStatus,
    pub url: Option<String>,
    #[serde(default)]
    pub decision: DeliverableDecision,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Freeform metadata (e.g. lines_added, lines_removed, files_changed).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    pub success: bool,
    pub message: String,
    pub merged_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewProviderKind {
    Github,
}

impl fmt::Display for ReviewProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Github => "github",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewTarget {
    pub provider: ReviewProviderKind,
    pub repository: String,
    pub number: u64,
    pub url: Option<String>,
    pub base_branch: String,
    pub head_branch: String,
    pub head_sha: String,
    #[serde(default)]
    pub checkout_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestReviewTrigger {
    pub provider: ReviewProviderKind,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: Option<String>,
    pub base_branch: String,
    pub head_branch: String,
    pub head_sha: String,
    #[serde(default)]
    pub checkout_ref: Option<String>,
    #[serde(default)]
    pub author_login: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_draft: bool,
}

impl PullRequestReviewTrigger {
    pub fn display_identifier(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    pub fn dedupe_key(&self) -> String {
        format!(
            "pr_review:{}:{}:{}:{}",
            self.provider, self.repository, self.number, self.head_sha
        )
    }

    pub fn synthetic_issue_id(&self) -> String {
        self.dedupe_key()
    }

    pub fn review_target(&self) -> ReviewTarget {
        ReviewTarget {
            provider: self.provider,
            repository: self.repository.clone(),
            number: self.number,
            url: self.url.clone(),
            base_branch: self.base_branch.clone(),
            head_branch: self.head_branch.clone(),
            head_sha: self.head_sha.clone(),
            checkout_ref: self.checkout_ref.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestCommentTrigger {
    pub provider: ReviewProviderKind,
    pub repository: String,
    pub number: u64,
    pub pull_request_title: String,
    pub url: Option<String>,
    pub base_branch: String,
    pub head_branch: String,
    pub head_sha: String,
    #[serde(default)]
    pub checkout_ref: Option<String>,
    pub thread_id: String,
    pub comment_id: String,
    pub path: String,
    pub line: Option<u32>,
    pub body: String,
    #[serde(default)]
    pub author_login: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_draft: bool,
}

impl PullRequestCommentTrigger {
    pub fn display_identifier(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    pub fn dedupe_key(&self) -> String {
        format!(
            "pr_comment:{}:{}:{}:{}",
            self.provider, self.repository, self.number, self.thread_id
        )
    }

    pub fn synthetic_issue_id(&self) -> String {
        self.dedupe_key()
    }

    pub fn review_target(&self) -> ReviewTarget {
        ReviewTarget {
            provider: self.provider,
            repository: self.repository.clone(),
            number: self.number,
            url: self.url.clone(),
            base_branch: self.base_branch.clone(),
            head_branch: self.head_branch.clone(),
            head_sha: self.head_sha.clone(),
            checkout_ref: self.checkout_ref.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestConflictTrigger {
    pub provider: ReviewProviderKind,
    pub repository: String,
    pub number: u64,
    pub pull_request_title: String,
    pub url: Option<String>,
    pub base_branch: String,
    pub head_branch: String,
    pub head_sha: String,
    #[serde(default)]
    pub checkout_ref: Option<String>,
    #[serde(default)]
    pub author_login: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_draft: bool,
    pub mergeable_state: String,
    pub merge_state_status: String,
}

impl PullRequestConflictTrigger {
    pub fn display_identifier(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    pub fn dedupe_key(&self) -> String {
        format!(
            "pr_conflict:{}:{}:{}:{}",
            self.provider, self.repository, self.number, self.head_sha
        )
    }

    pub fn synthetic_issue_id(&self) -> String {
        self.dedupe_key()
    }

    pub fn review_target(&self) -> ReviewTarget {
        ReviewTarget {
            provider: self.provider,
            repository: self.repository.clone(),
            number: self.number,
            url: self.url.clone(),
            base_branch: self.base_branch.clone(),
            head_branch: self.head_branch.clone(),
            head_sha: self.head_sha.clone(),
            checkout_ref: self.checkout_ref.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PullRequestTrigger {
    Review(PullRequestReviewTrigger),
    Comment(PullRequestCommentTrigger),
    Conflict(PullRequestConflictTrigger),
}

impl PullRequestTrigger {
    pub fn dedupe_key(&self) -> String {
        match self {
            Self::Review(trigger) => trigger.dedupe_key(),
            Self::Comment(trigger) => trigger.dedupe_key(),
            Self::Conflict(trigger) => trigger.dedupe_key(),
        }
    }

    pub fn synthetic_issue_id(&self) -> String {
        match self {
            Self::Review(trigger) => trigger.synthetic_issue_id(),
            Self::Comment(trigger) => trigger.synthetic_issue_id(),
            Self::Conflict(trigger) => trigger.synthetic_issue_id(),
        }
    }

    pub fn display_identifier(&self) -> String {
        match self {
            Self::Review(trigger) => trigger.display_identifier(),
            Self::Comment(trigger) => trigger.display_identifier(),
            Self::Conflict(trigger) => trigger.display_identifier(),
        }
    }

    pub fn review_target(&self) -> ReviewTarget {
        match self {
            Self::Review(trigger) => trigger.review_target(),
            Self::Comment(trigger) => trigger.review_target(),
            Self::Conflict(trigger) => trigger.review_target(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewedPullRequestHead {
    pub key: String,
    pub target: ReviewTarget,
    pub reviewed_at: DateTime<Utc>,
    pub movement_id: Option<MovementId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Movement {
    pub id: MovementId,
    #[serde(default)]
    pub kind: MovementKind,
    pub issue_id: Option<IssueId>,
    pub issue_identifier: Option<String>,
    pub title: String,
    pub status: MovementStatus,
    /// Tracks the current pipeline lifecycle stage for resume-after-restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_stage: Option<PipelineStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_dispatch_directives: Option<String>,
    pub workspace_key: Option<String>,
    pub workspace_path: Option<PathBuf>,
    #[serde(default)]
    pub review_target: Option<ReviewTarget>,
    pub deliverable: Option<Deliverable>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub movement_id: MovementId,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: TaskCategory,
    pub status: TaskStatus,
    pub ordinal: u32,
    pub parent_id: Option<TaskId>,
    pub agent_name: Option<String>,
    /// Session ID from the agent runtime (e.g., tmux session name, Claude session).
    /// Stored so the orchestrator can resume the session after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Thread/conversation ID from the agent provider (e.g., Codex thread UUID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub turns_completed: u32,
    pub tokens: TokenUsage,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlan {
    pub tasks: Vec<PlannedTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTask {
    pub title: String,
    pub category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

impl PlannedTask {
    pub fn to_task(&self, movement_id: &str, ordinal: u32) -> Task {
        let now = Utc::now();
        let category = match self.category.to_ascii_lowercase().as_str() {
            "research" => TaskCategory::Research,
            "coding" => TaskCategory::Coding,
            "testing" => TaskCategory::Testing,
            "documentation" => TaskCategory::Documentation,
            "review" => TaskCategory::Review,
            _ => TaskCategory::Coding,
        };
        Task {
            id: format!("task-{}", Uuid::new_v4()),
            movement_id: movement_id.to_string(),
            title: self.title.clone(),
            description: self.description.clone(),
            category,
            status: TaskStatus::Pending,
            ordinal,
            parent_id: None,
            agent_name: self.agent.clone(),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovementRow {
    pub id: MovementId,
    #[serde(default)]
    pub kind: MovementKind,
    pub issue_identifier: Option<String>,
    pub title: String,
    pub status: MovementStatus,
    pub task_count: usize,
    pub tasks_completed: usize,
    #[serde(default)]
    pub deliverable: Option<Deliverable>,
    pub has_deliverable: bool,
    #[serde(default)]
    pub review_target: Option<ReviewTarget>,
    #[serde(default)]
    pub workspace_key: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: TaskId,
    pub movement_id: MovementId,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: TaskCategory,
    pub status: TaskStatus,
    pub ordinal: u32,
    pub agent_name: Option<String>,
    pub turns_completed: u32,
    pub total_tokens: u64,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
