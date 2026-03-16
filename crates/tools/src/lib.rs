use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use {
    async_trait::async_trait,
    polyphony_core::{
        AddIssueCommentRequest, Error as CoreError, IssueTracker, PullRequestCommenter,
        PullRequestRef, ToolCallRequest, ToolCallResult, ToolExecutor, ToolSpec, TrackerKind,
        UpdateIssueRequest,
    },
    polyphony_linear::LinearTracker,
    polyphony_workflow::{LoadedWorkflow, ToolPolicyConfig},
    serde_json::{Value, json},
};

const DEFAULT_LIST_MAX_DEPTH: usize = 4;
const DEFAULT_LIST_MAX_ENTRIES: usize = 200;
const DEFAULT_READ_MAX_CHARS: usize = 20_000;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 50;
const DEFAULT_SEARCH_MAX_FILE_BYTES: u64 = 256 * 1024;

#[async_trait]
trait BuiltinTool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError>;
}

#[derive(Default)]
struct ToolPolicy {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl ToolPolicy {
    fn from_config(allow: &[String], deny: &[String]) -> Self {
        Self {
            allow: allow.to_vec(),
            deny: deny.to_vec(),
        }
    }

    fn merge(&self, other: &ToolPolicyConfig) -> Self {
        Self {
            allow: if other.allow.is_empty() {
                self.allow.clone()
            } else {
                other.allow.clone()
            },
            deny: self
                .deny
                .iter()
                .cloned()
                .chain(other.deny.iter().cloned())
                .collect(),
        }
    }

    fn is_allowed(&self, tool_name: &str) -> bool {
        if self
            .deny
            .iter()
            .any(|pattern| pattern_matches(pattern, tool_name))
        {
            return false;
        }
        if self.allow.is_empty() {
            return true;
        }
        self.allow
            .iter()
            .any(|pattern| pattern_matches(pattern, tool_name))
    }
}

fn pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

pub struct RegistryToolExecutor {
    tools: HashMap<String, Arc<dyn BuiltinTool>>,
    global_policy: ToolPolicy,
    agent_policies: HashMap<String, ToolPolicyConfig>,
}

impl RegistryToolExecutor {
    pub fn from_runtime_components(
        workflow: &LoadedWorkflow,
        tracker: Arc<dyn IssueTracker>,
        pull_request_commenter: Option<Arc<dyn PullRequestCommenter>>,
    ) -> Result<Option<Arc<dyn ToolExecutor>>, CoreError> {
        if !workflow.config.tools.enabled {
            return Ok(None);
        }

        let mut tools: HashMap<String, Arc<dyn BuiltinTool>> = HashMap::new();
        register_tool(&mut tools, WorkspaceListFilesTool);
        register_tool(&mut tools, WorkspaceReadFileTool);
        register_tool(&mut tools, WorkspaceSearchTool);

        if workflow.config.tracker.kind != TrackerKind::None {
            register_tool(&mut tools, IssueUpdateTool {
                tracker: tracker.clone(),
            });
            register_tool(&mut tools, IssueCommentTool { tracker });
        }

        if let Some(commenter) = pull_request_commenter {
            register_tool(&mut tools, PullRequestCommentTool { commenter });
        }

        if workflow.config.tracker.kind == TrackerKind::Linear
            && let Some(api_key) = workflow.config.tracker.api_key.clone()
        {
            let tracker = LinearTracker::new(
                workflow.config.tracker.endpoint.clone(),
                api_key,
                workflow.config.tracker.team_id.clone(),
            )?;
            register_tool(&mut tools, LinearGraphqlTool { tracker });
        }

        Ok(Some(Arc::new(Self {
            tools,
            global_policy: ToolPolicy::from_config(
                &workflow.config.tools.allow,
                &workflow.config.tools.deny,
            ),
            agent_policies: workflow.config.tools.by_agent.clone(),
        }) as Arc<dyn ToolExecutor>))
    }

    fn effective_policy(&self, agent_name: &str) -> ToolPolicy {
        match self.agent_policies.get(agent_name) {
            Some(policy) => self.global_policy.merge(policy),
            None => ToolPolicy {
                allow: self.global_policy.allow.clone(),
                deny: self.global_policy.deny.clone(),
            },
        }
    }

    fn visible_tools(&self, agent_name: &str) -> Vec<Arc<dyn BuiltinTool>> {
        let policy = self.effective_policy(agent_name);
        self.tools
            .values()
            .filter(|tool| policy.is_allowed(&tool.spec().name))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl ToolExecutor for RegistryToolExecutor {
    fn list_tools(&self, agent_name: &str) -> Vec<ToolSpec> {
        self.visible_tools(agent_name)
            .into_iter()
            .map(|tool| tool.spec())
            .collect()
    }

    async fn execute(&self, request: ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let policy = self.effective_policy(&request.agent_name);
        let Some(tool) = self.tools.get(&request.name).cloned() else {
            return Ok(unsupported_tool_result(
                &request.name,
                self.list_tools(&request.agent_name),
            ));
        };
        if !policy.is_allowed(&request.name) {
            return Ok(unsupported_tool_result(
                &request.name,
                self.list_tools(&request.agent_name),
            ));
        }
        tool.execute(&request).await
    }
}

fn register_tool<T>(tools: &mut HashMap<String, Arc<dyn BuiltinTool>>, tool: T)
where
    T: BuiltinTool + 'static,
{
    let tool = Arc::new(tool) as Arc<dyn BuiltinTool>;
    tools.insert(tool.spec().name.clone(), tool);
}

struct WorkspaceListFilesTool;

#[async_trait]
impl BuiltinTool for WorkspaceListFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "workspace_list_files".into(),
            description:
                "List files and directories inside the current workspace without shell access."
                    .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": {
                        "type": ["string", "null"],
                        "description": "Optional workspace-relative directory to list. Defaults to the workspace root."
                    },
                    "max_depth": {
                        "type": ["integer", "null"],
                        "minimum": 0,
                        "description": "Maximum recursion depth. Defaults to 4."
                    },
                    "max_entries": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Maximum number of entries to return. Defaults to 200."
                    },
                    "include_hidden": {
                        "type": ["boolean", "null"],
                        "description": "Whether to include dotfiles and dot-directories."
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let root = canonical_workspace_root(&request.workspace_path)?;
        let path = optional_trimmed_string(&request.arguments, "path")?;
        let target = resolve_workspace_path(&root, path.as_deref())?;
        if !target.is_dir() {
            return Err(CoreError::Adapter(format!(
                "workspace_list_files target is not a directory: {}",
                target.display()
            )));
        }

        let max_depth = bounded_usize(
            &request.arguments,
            "max_depth",
            DEFAULT_LIST_MAX_DEPTH,
            0,
            12,
        )?;
        let max_entries = bounded_usize(
            &request.arguments,
            "max_entries",
            DEFAULT_LIST_MAX_ENTRIES,
            1,
            1_000,
        )?;
        let include_hidden = optional_bool(&request.arguments, "include_hidden")?.unwrap_or(false);
        let base_relative = path_relative_to_root(&root, &target)?;
        let mut entries = Vec::new();
        let mut truncated = false;
        collect_directory_entries(
            &root,
            &target,
            &base_relative,
            0,
            max_depth,
            include_hidden,
            max_entries,
            &mut entries,
            &mut truncated,
        )?;

        Ok(json_result(
            true,
            json!({
                "path": display_workspace_relative_path(&base_relative),
                "entries": entries,
                "truncated": truncated,
            }),
        ))
    }
}

struct WorkspaceReadFileTool;

#[async_trait]
impl BuiltinTool for WorkspaceReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "workspace_read_file".into(),
            description: "Read a UTF-8 text file from the current workspace.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative file path to read."
                    },
                    "start_line": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "1-based line number to start from."
                    },
                    "end_line": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "1-based line number to end at."
                    },
                    "max_chars": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Maximum characters to return. Defaults to 20000."
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let root = canonical_workspace_root(&request.workspace_path)?;
        let path = required_trimmed_string(&request.arguments, "path")?;
        let target = resolve_workspace_path(&root, Some(path.as_str()))?;
        if !target.is_file() {
            return Err(CoreError::Adapter(format!(
                "workspace_read_file target is not a file: {}",
                target.display()
            )));
        }

        let raw = fs::read(&target).map_err(|error| {
            CoreError::Adapter(format!("failed to read {}: {error}", target.display()))
        })?;
        let text = String::from_utf8(raw).map_err(|_| {
            CoreError::Adapter(format!(
                "workspace_read_file only supports UTF-8 text files: {}",
                target.display()
            ))
        })?;
        let lines = text.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
        let total_lines = lines.len();
        let start_line = bounded_usize(&request.arguments, "start_line", 1, 1, usize::MAX)?;
        let end_line =
            optional_positive_usize(&request.arguments, "end_line")?.unwrap_or(total_lines.max(1));
        if end_line < start_line {
            return Err(CoreError::Adapter(
                "workspace_read_file.end_line must be >= start_line".into(),
            ));
        }
        let max_chars = bounded_usize(
            &request.arguments,
            "max_chars",
            DEFAULT_READ_MAX_CHARS,
            1,
            100_000,
        )?;
        let slice_start = start_line.saturating_sub(1).min(total_lines);
        let slice_end = end_line.min(total_lines);
        let mut content = if slice_start >= slice_end {
            String::new()
        } else {
            lines[slice_start..slice_end].join("\n")
        };
        let mut truncated = false;
        if let Some(truncated_content) = truncate_string(&content, max_chars) {
            content = truncated_content;
            truncated = true;
        }

        Ok(json_result(
            true,
            json!({
                "path": display_workspace_relative_path(&path_relative_to_root(&root, &target)?),
                "start_line": start_line,
                "end_line": slice_end,
                "total_lines": total_lines,
                "truncated": truncated,
                "content": content,
            }),
        ))
    }
}

struct WorkspaceSearchTool;

#[async_trait]
impl BuiltinTool for WorkspaceSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "workspace_search".into(),
            description: "Search UTF-8 text files in the current workspace for a substring.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Substring to search for."
                    },
                    "path": {
                        "type": ["string", "null"],
                        "description": "Optional workspace-relative file or directory to search."
                    },
                    "case_sensitive": {
                        "type": ["boolean", "null"],
                        "description": "Whether the match should be case sensitive."
                    },
                    "include_hidden": {
                        "type": ["boolean", "null"],
                        "description": "Whether to include hidden files and directories."
                    },
                    "max_results": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Maximum number of matches to return. Defaults to 50."
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let root = canonical_workspace_root(&request.workspace_path)?;
        let query = required_non_empty_string(&request.arguments, "query")?;
        let path = optional_trimmed_string(&request.arguments, "path")?;
        let target = resolve_workspace_path(&root, path.as_deref())?;
        let case_sensitive = optional_bool(&request.arguments, "case_sensitive")?.unwrap_or(false);
        let include_hidden = optional_bool(&request.arguments, "include_hidden")?.unwrap_or(false);
        let max_results = bounded_usize(
            &request.arguments,
            "max_results",
            DEFAULT_SEARCH_MAX_RESULTS,
            1,
            500,
        )?;

        let mut state = SearchState::default();
        if target.is_file() {
            search_file(
                &root,
                &target,
                &query,
                case_sensitive,
                max_results,
                DEFAULT_SEARCH_MAX_FILE_BYTES,
                &mut state,
            )?;
        } else if target.is_dir() {
            search_directory(
                &root,
                &target,
                &query,
                case_sensitive,
                include_hidden,
                max_results,
                DEFAULT_SEARCH_MAX_FILE_BYTES,
                &mut state,
            )?;
        } else {
            return Err(CoreError::Adapter(format!(
                "workspace_search target does not exist: {}",
                target.display()
            )));
        }

        Ok(json_result(
            true,
            json!({
                "query": query,
                "path": display_workspace_relative_path(&path_relative_to_root(&root, &target)?),
                "matches": state.matches,
                "searched_files": state.searched_files,
                "skipped_files": state.skipped_files,
                "truncated": state.truncated,
            }),
        ))
    }
}

struct IssueUpdateTool {
    tracker: Arc<dyn IssueTracker>,
}

#[async_trait]
impl BuiltinTool for IssueUpdateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "issue_update".into(),
            description:
                "Update tracker issue fields using Polyphony's configured tracker integration."
                    .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "issue_id": {
                        "type": ["string", "null"],
                        "description": "Tracker issue id. Defaults to the current issue."
                    },
                    "title": {
                        "type": ["string", "null"],
                        "description": "Updated issue title."
                    },
                    "description": {
                        "type": ["string", "null"],
                        "description": "Updated issue description."
                    },
                    "state": {
                        "type": ["string", "null"],
                        "description": "Tracker-native state value. GitHub expects open or closed; Linear expects a state id."
                    },
                    "priority": {
                        "type": ["integer", "null"],
                        "description": "Updated priority value."
                    },
                    "labels": {
                        "type": ["array", "null"],
                        "items": { "type": "string" },
                        "description": "Tracker-native label values. When provided, this replaces the current label set."
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let issue_id = issue_id_from_arguments(&request.arguments, request)?;
        let title = optional_string(&request.arguments, "title")?;
        let description = optional_string(&request.arguments, "description")?;
        let state = optional_trimmed_string(&request.arguments, "state")?;
        let priority = optional_i32(&request.arguments, "priority")?;
        let labels = optional_string_array(&request.arguments, "labels")?;

        if title.is_none()
            && description.is_none()
            && state.is_none()
            && priority.is_none()
            && labels.is_none()
        {
            return Err(CoreError::Adapter(
                "issue_update requires at least one mutable field".into(),
            ));
        }

        let issue = self
            .tracker
            .update_issue(&UpdateIssueRequest {
                id: issue_id,
                title,
                description,
                state,
                priority,
                labels,
            })
            .await?;
        Ok(json_result(true, json!({ "issue": issue })))
    }
}

struct IssueCommentTool {
    tracker: Arc<dyn IssueTracker>,
}

#[async_trait]
impl BuiltinTool for IssueCommentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "issue_comment".into(),
            description: "Post a comment to the current issue or another tracker issue.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["body"],
                "properties": {
                    "issue_id": {
                        "type": ["string", "null"],
                        "description": "Tracker issue id. Defaults to the current issue."
                    },
                    "body": {
                        "type": "string",
                        "description": "Comment body to post."
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let issue_id = issue_id_from_arguments(&request.arguments, request)?;
        let body = required_non_empty_string(&request.arguments, "body")?;
        let comment = self
            .tracker
            .comment_on_issue(&AddIssueCommentRequest { id: issue_id, body })
            .await?;
        Ok(json_result(true, json!({ "comment": comment })))
    }
}

struct PullRequestCommentTool {
    commenter: Arc<dyn PullRequestCommenter>,
}

#[async_trait]
impl BuiltinTool for PullRequestCommentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pr_comment".into(),
            description:
                "Post a summary comment to a pull request using Polyphony's configured commenter."
                    .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["repository", "pull_request_number", "body"],
                "properties": {
                    "repository": {
                        "type": "string",
                        "description": "Repository slug in owner/name format."
                    },
                    "pull_request_number": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Pull request number."
                    },
                    "body": {
                        "type": "string",
                        "description": "Comment body to post."
                    },
                    "url": {
                        "type": ["string", "null"],
                        "description": "Optional pull request URL."
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let repository = required_non_empty_string(&request.arguments, "repository")?;
        let number = required_u64(&request.arguments, "pull_request_number")?;
        let body = required_non_empty_string(&request.arguments, "body")?;
        let url = optional_trimmed_string(&request.arguments, "url")?;
        self.commenter
            .comment_on_pull_request(
                &PullRequestRef {
                    repository: repository.clone(),
                    number,
                    url: url.clone(),
                },
                &body,
            )
            .await?;
        Ok(json_result(
            true,
            json!({
                "repository": repository,
                "pull_request_number": number,
                "url": url,
                "commented": true,
            }),
        ))
    }
}

struct LinearGraphqlTool {
    tracker: LinearTracker,
}

#[async_trait]
impl BuiltinTool for LinearGraphqlTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "linear_graphql".into(),
            description: "Execute a raw GraphQL query or mutation against Linear using Polyphony's configured auth.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "GraphQL query or mutation document to execute against Linear."
                    },
                    "variables": {
                        "type": ["object", "null"],
                        "description": "Optional GraphQL variables object.",
                        "additionalProperties": true
                    }
                }
            }),
        }
    }

    async fn execute(&self, request: &ToolCallRequest) -> Result<ToolCallResult, CoreError> {
        let (query, variables) = normalize_linear_graphql_arguments(&request.arguments)?;
        let payload = match self.tracker.execute_raw_graphql(&query, variables).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(failure_result(json!({
                    "error": {
                        "message": "Linear GraphQL tool execution failed.",
                        "reason": error.to_string(),
                    }
                })));
            },
        };
        let success = payload
            .get("errors")
            .and_then(Value::as_array)
            .is_none_or(|errors| errors.is_empty());
        Ok(json_result(success, payload))
    }
}

#[derive(Default)]
struct SearchState {
    matches: Vec<Value>,
    searched_files: usize,
    skipped_files: usize,
    truncated: bool,
}

fn normalize_linear_graphql_arguments(arguments: &Value) -> Result<(String, Value), CoreError> {
    let query = required_non_empty_string(arguments, "query")?;
    let variables = match arguments.get("variables") {
        Some(Value::Null) | None => json!({}),
        Some(Value::Object(_)) => arguments
            .get("variables")
            .cloned()
            .unwrap_or_else(|| json!({})),
        Some(_) => {
            return Err(CoreError::Adapter(
                "linear_graphql.variables must be a JSON object when provided".into(),
            ));
        },
    };
    Ok((query, variables))
}

fn issue_id_from_arguments(
    arguments: &Value,
    request: &ToolCallRequest,
) -> Result<String, CoreError> {
    if let Some(issue_id) = optional_trimmed_string(arguments, "issue_id")?
        && !issue_id.is_empty()
    {
        return Ok(issue_id);
    }
    if request.issue.id.trim().is_empty() {
        return Err(CoreError::Adapter(
            "issue_id is required when the current issue has no tracker id".into(),
        ));
    }
    Ok(request.issue.id.clone())
}

fn canonical_workspace_root(workspace_path: &Path) -> Result<PathBuf, CoreError> {
    workspace_path.canonicalize().map_err(|error| {
        CoreError::Adapter(format!(
            "failed to resolve workspace root {}: {error}",
            workspace_path.display()
        ))
    })
}

fn resolve_workspace_path(root: &Path, path: Option<&str>) -> Result<PathBuf, CoreError> {
    let relative = sanitize_relative_path(path)?;
    let target = root.join(relative);
    let resolved = target.canonicalize().map_err(|error| {
        CoreError::Adapter(format!(
            "failed to resolve workspace path {}: {error}",
            target.display()
        ))
    })?;
    if !resolved.starts_with(root) {
        return Err(CoreError::Adapter(
            "workspace path must stay inside the workspace root".into(),
        ));
    }
    Ok(resolved)
}

fn sanitize_relative_path(path: Option<&str>) -> Result<PathBuf, CoreError> {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(PathBuf::new());
    };
    let raw = Path::new(path);
    let mut cleaned = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {},
            Component::Normal(value) => cleaned.push(value),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CoreError::Adapter(
                    "workspace paths must be relative to the workspace root".into(),
                ));
            },
        }
    }
    Ok(cleaned)
}

fn path_relative_to_root(root: &Path, path: &Path) -> Result<PathBuf, CoreError> {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| CoreError::Adapter("workspace path escaped the workspace root".into()))
}

fn display_workspace_relative_path(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        ".".into()
    } else {
        path.display().to_string()
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_directory_entries(
    root: &Path,
    dir: &Path,
    relative: &Path,
    depth: usize,
    max_depth: usize,
    include_hidden: bool,
    max_entries: usize,
    entries: &mut Vec<Value>,
    truncated: &mut bool,
) -> Result<(), CoreError> {
    let mut children = read_dir_sorted(dir)?;
    for child in children.drain(..) {
        if entries.len() >= max_entries {
            *truncated = true;
            return Ok(());
        }
        let file_name = child.file_name();
        if !include_hidden && is_hidden_name(&file_name) {
            continue;
        }
        let child_path = child.path();
        let file_type = child.file_type().map_err(|error| {
            CoreError::Adapter(format!(
                "failed to inspect workspace path {}: {error}",
                child_path.display()
            ))
        })?;
        let child_relative = relative.join(file_name);
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        let mut entry = json!({
            "path": display_workspace_relative_path(&child_relative),
            "kind": kind,
        });
        if file_type.is_file() {
            let size = child
                .metadata()
                .map_err(|error| {
                    CoreError::Adapter(format!(
                        "failed to read metadata for {}: {error}",
                        child_path.display()
                    ))
                })?
                .len();
            entry["size"] = json!(size);
        }
        entries.push(entry);
        if file_type.is_dir() && depth < max_depth {
            let canonical_child = child_path.canonicalize().map_err(|error| {
                CoreError::Adapter(format!(
                    "failed to resolve workspace path {}: {error}",
                    child_path.display()
                ))
            })?;
            if canonical_child.starts_with(root) {
                collect_directory_entries(
                    root,
                    &canonical_child,
                    &child_relative,
                    depth + 1,
                    max_depth,
                    include_hidden,
                    max_entries,
                    entries,
                    truncated,
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn search_directory(
    root: &Path,
    dir: &Path,
    query: &str,
    case_sensitive: bool,
    include_hidden: bool,
    max_results: usize,
    max_file_bytes: u64,
    state: &mut SearchState,
) -> Result<(), CoreError> {
    if state.truncated {
        return Ok(());
    }
    let mut children = read_dir_sorted(dir)?;
    for child in children.drain(..) {
        if state.truncated {
            return Ok(());
        }
        let file_name = child.file_name();
        if !include_hidden && is_hidden_name(&file_name) {
            continue;
        }
        let path = child.path();
        let file_type = child.file_type().map_err(|error| {
            CoreError::Adapter(format!(
                "failed to inspect workspace path {}: {error}",
                path.display()
            ))
        })?;
        if file_type.is_dir() {
            let canonical = path.canonicalize().map_err(|error| {
                CoreError::Adapter(format!(
                    "failed to resolve workspace path {}: {error}",
                    path.display()
                ))
            })?;
            if canonical.starts_with(root) {
                search_directory(
                    root,
                    &canonical,
                    query,
                    case_sensitive,
                    include_hidden,
                    max_results,
                    max_file_bytes,
                    state,
                )?;
            }
        } else if file_type.is_file() {
            search_file(
                root,
                &path,
                query,
                case_sensitive,
                max_results,
                max_file_bytes,
                state,
            )?;
        }
    }
    Ok(())
}

fn search_file(
    root: &Path,
    path: &Path,
    query: &str,
    case_sensitive: bool,
    max_results: usize,
    max_file_bytes: u64,
    state: &mut SearchState,
) -> Result<(), CoreError> {
    if state.truncated {
        return Ok(());
    }
    let metadata = fs::metadata(path).map_err(|error| {
        CoreError::Adapter(format!(
            "failed to read metadata for {}: {error}",
            path.display()
        ))
    })?;
    if metadata.len() > max_file_bytes {
        state.skipped_files += 1;
        return Ok(());
    }
    let file = File::open(path).map_err(|error| {
        CoreError::Adapter(format!("failed to open {}: {error}", path.display()))
    })?;
    let reader = BufReader::new(file);
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_ascii_lowercase()
    };
    state.searched_files += 1;
    for (index, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(_) => {
                state.skipped_files += 1;
                return Ok(());
            },
        };
        let haystack = if case_sensitive {
            line.clone()
        } else {
            line.to_ascii_lowercase()
        };
        if haystack.contains(&needle) {
            state.matches.push(json!({
                "path": display_workspace_relative_path(&path_relative_to_root(root, path)?),
                "line": index + 1,
                "text": line,
            }));
            if state.matches.len() >= max_results {
                state.truncated = true;
                return Ok(());
            }
        }
    }
    Ok(())
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<fs::DirEntry>, CoreError> {
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(dir).map_err(|error| {
        CoreError::Adapter(format!(
            "failed to read directory {}: {error}",
            dir.display()
        ))
    })?;
    for entry in read_dir {
        entries.push(entry.map_err(|error| {
            CoreError::Adapter(format!(
                "failed to read directory entry in {}: {error}",
                dir.display()
            ))
        })?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn truncate_string(value: &str, max_chars: usize) -> Option<String> {
    if value.chars().count() <= max_chars {
        return None;
    }
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        output.push(ch);
    }
    Some(output)
}

fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>, CoreError> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CoreError::Adapter(format!("{key} must be a string"))),
    }
}

fn optional_trimmed_string(arguments: &Value, key: &str) -> Result<Option<String>, CoreError> {
    Ok(optional_string(arguments, key)?.map(|value| value.trim().to_string()))
}

fn required_trimmed_string(arguments: &Value, key: &str) -> Result<String, CoreError> {
    let value = optional_trimmed_string(arguments, key)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::Adapter(format!("{key} requires a non-empty string")))?;
    Ok(value)
}

fn required_non_empty_string(arguments: &Value, key: &str) -> Result<String, CoreError> {
    let value = optional_string(arguments, key)?
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::Adapter(format!("{key} requires a non-empty string")))?;
    Ok(value)
}

fn optional_string_array(arguments: &Value, key: &str) -> Result<Option<Vec<String>>, CoreError> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Array(items) => {
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(item) = item else {
                    return Err(CoreError::Adapter(format!(
                        "{key} must contain only strings"
                    )));
                };
                parsed.push(item.trim().to_string());
            }
            Ok(Some(parsed))
        },
        _ => Err(CoreError::Adapter(format!(
            "{key} must be an array of strings"
        ))),
    }
}

fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>, CoreError> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(CoreError::Adapter(format!("{key} must be a boolean"))),
    }
}

fn optional_i32(arguments: &Value, key: &str) -> Result<Option<i32>, CoreError> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| CoreError::Adapter(format!("{key} must be an integer")))
            .and_then(|value| {
                i32::try_from(value)
                    .map(Some)
                    .map_err(|_| CoreError::Adapter(format!("{key} is out of range")))
            }),
        _ => Err(CoreError::Adapter(format!("{key} must be an integer"))),
    }
}

fn required_u64(arguments: &Value, key: &str) -> Result<u64, CoreError> {
    let value = arguments
        .get(key)
        .ok_or_else(|| CoreError::Adapter(format!("{key} is required")))?;
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| CoreError::Adapter(format!("{key} must be a positive integer"))),
        _ => Err(CoreError::Adapter(format!(
            "{key} must be a positive integer"
        ))),
    }
}

fn optional_positive_usize(arguments: &Value, key: &str) -> Result<Option<usize>, CoreError> {
    match arguments.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Number(number)) => {
            let value = number
                .as_u64()
                .ok_or_else(|| CoreError::Adapter(format!("{key} must be a positive integer")))?;
            usize::try_from(value)
                .map(Some)
                .map_err(|_| CoreError::Adapter(format!("{key} is out of range")))
        },
        Some(_) => Err(CoreError::Adapter(format!(
            "{key} must be a positive integer"
        ))),
    }
}

fn bounded_usize(
    arguments: &Value,
    key: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, CoreError> {
    let value = optional_positive_usize(arguments, key)?.unwrap_or(default);
    if value < min || value > max {
        return Err(CoreError::Adapter(format!(
            "{key} must be between {min} and {max}"
        )));
    }
    Ok(value)
}

fn unsupported_tool_result(tool_name: &str, supported_tools: Vec<ToolSpec>) -> ToolCallResult {
    failure_result(json!({
        "error": {
            "message": format!("Unsupported dynamic tool: {tool_name}."),
            "supportedTools": supported_tools
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>(),
        }
    }))
}

fn json_result(success: bool, payload: Value) -> ToolCallResult {
    let output = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    ToolCallResult::new(success, output.clone(), vec![json!({
        "type": "inputText",
        "text": output,
    })])
}

fn failure_result(payload: Value) -> ToolCallResult {
    json_result(false, payload)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use {
        super::{RegistryToolExecutor, ToolPolicy, pattern_matches},
        async_trait::async_trait,
        polyphony_core::{
            AddIssueCommentRequest, Error as CoreError, Issue, IssueComment, IssueTracker,
            PullRequestCommenter, PullRequestRef, ToolCallRequest, UpdateIssueRequest,
        },
        polyphony_workflow::{LoadedWorkflow, ServiceConfig, WorkflowDefinition, parse_workflow},
        serde_json::{Value, json},
        tempfile::TempDir,
    };

    #[derive(Default)]
    struct MockTracker {
        updates: Mutex<Vec<UpdateIssueRequest>>,
        comments: Mutex<Vec<AddIssueCommentRequest>>,
    }

    #[async_trait]
    impl IssueTracker for MockTracker {
        fn component_key(&self) -> String {
            "tracker:mock".into()
        }

        async fn fetch_candidate_issues(
            &self,
            _query: &polyphony_core::TrackerQuery,
        ) -> Result<Vec<Issue>, CoreError> {
            Ok(Vec::new())
        }

        async fn fetch_issues_by_states(
            &self,
            _project_slug: Option<&str>,
            _states: &[String],
        ) -> Result<Vec<Issue>, CoreError> {
            Ok(Vec::new())
        }

        async fn fetch_issues_by_ids(
            &self,
            _issue_ids: &[String],
        ) -> Result<Vec<Issue>, CoreError> {
            Ok(Vec::new())
        }

        async fn fetch_issue_states_by_ids(
            &self,
            _issue_ids: &[String],
        ) -> Result<Vec<polyphony_core::IssueStateUpdate>, CoreError> {
            Ok(Vec::new())
        }

        async fn update_issue(&self, request: &UpdateIssueRequest) -> Result<Issue, CoreError> {
            self.updates.lock().unwrap().push(request.clone());
            Ok(Issue {
                id: request.id.clone(),
                title: request.title.clone().unwrap_or_else(|| "updated".into()),
                description: request.description.clone(),
                priority: request.priority,
                state: request.state.clone().unwrap_or_else(|| "Todo".into()),
                labels: request.labels.clone().unwrap_or_default(),
                ..Issue::default()
            })
        }

        async fn comment_on_issue(
            &self,
            request: &AddIssueCommentRequest,
        ) -> Result<IssueComment, CoreError> {
            self.comments.lock().unwrap().push(request.clone());
            Ok(IssueComment {
                id: "comment-1".into(),
                body: request.body.clone(),
                author: None,
                url: None,
                created_at: None,
                updated_at: None,
            })
        }
    }

    #[derive(Default)]
    struct MockPullRequestCommenter {
        comments: Mutex<Vec<(PullRequestRef, String)>>,
    }

    #[async_trait]
    impl PullRequestCommenter for MockPullRequestCommenter {
        fn component_key(&self) -> String {
            "pr-commenter:mock".into()
        }

        async fn comment_on_pull_request(
            &self,
            pull_request: &PullRequestRef,
            body: &str,
        ) -> Result<(), CoreError> {
            self.comments
                .lock()
                .unwrap()
                .push((pull_request.clone(), body.to_string()));
            Ok(())
        }
    }

    #[test]
    fn wildcard_policy_matches() {
        assert!(pattern_matches("*", "linear_graphql"));
        assert!(pattern_matches("linear*", "linear_graphql"));
        assert!(!pattern_matches("github*", "linear_graphql"));
    }

    #[test]
    fn deny_wins() {
        let policy = ToolPolicy {
            allow: vec!["*".into()],
            deny: vec!["linear_graphql".into()],
        };
        assert!(!policy.is_allowed("linear_graphql"));
    }

    #[tokio::test]
    async fn registry_lists_and_executes_workspace_tools() {
        let tempdir = TempDir::new().unwrap();
        fs::write(tempdir.path().join("alpha.txt"), "hello\nneedle\n").unwrap();
        fs::create_dir(tempdir.path().join("src")).unwrap();
        fs::write(
            tempdir.path().join("src").join("lib.rs"),
            "pub fn needle() {}\n",
        )
        .unwrap();

        let workflow = loaded_workflow(
            r#"---
tools:
  enabled: true
  allow:
    - workspace_*
---
prompt
"#,
        );
        let tracker = Arc::new(MockTracker::default()) as Arc<dyn IssueTracker>;
        let executor = RegistryToolExecutor::from_runtime_components(&workflow, tracker, None)
            .unwrap()
            .unwrap();

        let tool_names = executor
            .list_tools("implementer")
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"workspace_list_files".into()));
        assert!(tool_names.contains(&"workspace_read_file".into()));
        assert!(tool_names.contains(&"workspace_search".into()));

        let list_result = executor
            .execute(tool_request(
                "workspace_list_files",
                tempdir.path(),
                json!({}),
            ))
            .await
            .unwrap();
        assert!(list_result.success);
        assert!(list_result.output.contains("alpha.txt"));

        let read_result = executor
            .execute(tool_request(
                "workspace_read_file",
                tempdir.path(),
                json!({ "path": "alpha.txt" }),
            ))
            .await
            .unwrap();
        assert!(read_result.success);
        assert!(read_result.output.contains("needle"));

        let search_result = executor
            .execute(tool_request(
                "workspace_search",
                tempdir.path(),
                json!({ "query": "needle" }),
            ))
            .await
            .unwrap();
        assert!(search_result.success);
        assert!(search_result.output.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn registry_executes_issue_and_pr_tools() {
        let tempdir = TempDir::new().unwrap();
        let tracker = Arc::new(MockTracker::default());
        let commenter = Arc::new(MockPullRequestCommenter::default());
        let workflow = loaded_workflow(
            r#"---
tracker:
  kind: github
  repository: owner/repo
  api_key: token
tools:
  enabled: true
  allow:
    - issue_update
    - issue_comment
    - pr_comment
---
prompt
"#,
        );
        let executor = RegistryToolExecutor::from_runtime_components(
            &workflow,
            tracker.clone(),
            Some(commenter.clone()),
        )
        .unwrap()
        .unwrap();

        let issue_update = executor
            .execute(tool_request(
                "issue_update",
                tempdir.path(),
                json!({ "title": "New title" }),
            ))
            .await
            .unwrap();
        assert!(issue_update.success);
        assert_eq!(tracker.updates.lock().unwrap()[0].id, "ISSUE-1");

        let issue_comment = executor
            .execute(tool_request(
                "issue_comment",
                tempdir.path(),
                json!({ "body": "Ship it" }),
            ))
            .await
            .unwrap();
        assert!(issue_comment.success);
        assert_eq!(tracker.comments.lock().unwrap()[0].id, "ISSUE-1");

        let pr_comment = executor
            .execute(tool_request(
                "pr_comment",
                tempdir.path(),
                json!({
                    "repository": "owner/repo",
                    "pull_request_number": 42,
                    "body": "Looks good"
                }),
            ))
            .await
            .unwrap();
        assert!(pr_comment.success);
        let comments = commenter.comments.lock().unwrap();
        assert_eq!(comments[0].0.number, 42);
        assert_eq!(comments[0].1, "Looks good");
    }

    fn loaded_workflow(raw: &str) -> LoadedWorkflow {
        let definition = parse_workflow(raw).unwrap();
        let config = ServiceConfig::from_workflow(&definition).unwrap();
        LoadedWorkflow {
            definition: WorkflowDefinition {
                config: definition.config.clone(),
                prompt_template: definition.prompt_template.clone(),
            },
            config,
            path: PathBuf::from("/tmp/WORKFLOW.md"),
            agent_prompts: Default::default(),
        }
    }

    fn tool_request(name: &str, workspace_path: &Path, arguments: Value) -> ToolCallRequest {
        ToolCallRequest {
            name: name.to_string(),
            arguments,
            issue: Issue {
                id: "ISSUE-1".into(),
                identifier: "ISSUE-1".into(),
                title: "Issue".into(),
                description: None,
                priority: None,
                state: "Todo".into(),
                branch_name: None,
                url: None,
                author: None,
                labels: Vec::new(),
                comments: Vec::new(),
                blocked_by: Vec::new(),
                approval_state: polyphony_core::IssueApprovalState::Approved,
                parent_id: None,
                created_at: None,
                updated_at: None,
            },
            workspace_path: workspace_path.to_path_buf(),
            agent_name: "implementer".into(),
            call_id: None,
            thread_id: None,
            turn_id: None,
        }
    }
}
