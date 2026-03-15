use crate::{prelude::*, *};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackInboundMode {
    None,
    Polling,
    Webhook,
    Websocket,
    Cli,
    Mcp,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedbackCapabilities {
    pub supports_outbound: bool,
    pub supports_links: bool,
    pub supports_interactive: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackChannelKind {
    Telegram,
    Webhook,
}

impl fmt::Display for FeedbackChannelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format!("{self:?}").to_lowercase())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackChannelDescriptor {
    pub kind: FeedbackChannelKind,
    pub inbound_mode: FeedbackInboundMode,
    pub capabilities: FeedbackCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackAction {
    pub id: String,
    pub label: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackNotification {
    pub key: String,
    pub title: String,
    pub body: String,
    pub links: Vec<FeedbackLink>,
    pub actions: Vec<FeedbackAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueAssignment {
    pub issue_id: String,
    pub issue_identifier: String,
    pub pull_request: Option<PullRequestRef>,
}
