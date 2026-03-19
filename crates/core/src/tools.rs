use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Issue};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub name: String,
    pub arguments: Value,
    pub issue: Issue,
    pub workspace_path: PathBuf,
    pub agent_name: String,
    pub call_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallResult {
    pub success: bool,
    pub output: String,
    pub content_items: Vec<Value>,
}

impl ToolCallResult {
    pub fn new(success: bool, output: impl Into<String>, content_items: Vec<Value>) -> Self {
        Self {
            success,
            output: output.into(),
            content_items,
        }
    }
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn list_tools(&self, agent_name: &str) -> Vec<ToolSpec>;
    async fn execute(&self, request: ToolCallRequest) -> Result<ToolCallResult, Error>;
}
