use crate::{prelude::*, *};

pub type IssueId = String;
pub type MovementId = String;
pub type TaskId = String;

#[derive(Debug, Clone, Default)]
pub struct CreateIssueRequest {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub labels: Vec<String>,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateIssueRequest {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub priority: Option<i32>,
    pub labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct AddIssueCommentRequest {
    pub id: String,
    pub body: String,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid issue: {0}")]
    InvalidIssue(&'static str),
    #[error("adapter error: {0}")]
    Adapter(String),
    #[error("state store error: {0}")]
    Store(String),
    #[error("rate limited: {0:?}")]
    RateLimited(Box<RateLimitSignal>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueAuthor {
    pub id: Option<String>,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub trust_level: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueComment {
    pub id: String,
    pub body: String,
    pub author: Option<IssueAuthor>,
    pub url: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub state: String,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    pub author: Option<IssueAuthor>,
    pub labels: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub blocked_by: Vec<BlockerRef>,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Issue {
    pub fn normalized_state(&self) -> String {
        self.state.to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub path: PathBuf,
    pub workspace_key: String,
    pub created_now: bool,
    pub branch_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DispatchMode {
    #[default]
    Manual,
    Automatic,
    Nightshift,
    Idle,
}

impl fmt::Display for DispatchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
            Self::Nightshift => "nightshift",
            Self::Idle => "idle",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutKind {
    #[default]
    Directory,
    LinkedWorktree,
    DiscreteClone,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRequest {
    pub issue_identifier: String,
    pub workspace_root: PathBuf,
    pub workspace_path: PathBuf,
    pub workspace_key: String,
    pub branch_name: Option<String>,
    pub checkout_ref: Option<String>,
    pub checkout_kind: CheckoutKind,
    pub sync_on_reuse: bool,
    pub source_repo_path: Option<PathBuf>,
    pub clone_url: Option<String>,
    pub default_branch: Option<String>,
}
