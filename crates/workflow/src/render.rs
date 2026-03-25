use crate::{prelude::*, *};

pub fn render_prompt(
    workflow: &LoadedWorkflow,
    agent_name: &str,
    issue: &Issue,
    attempt: Option<u32>,
) -> Result<String, Error> {
    render_agent_prompt(workflow, agent_name, issue, attempt, 1, 1)
}

pub fn render_turn_prompt(
    workflow: &WorkflowDefinition,
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
) -> Result<String, Error> {
    let source = if workflow.prompt_template.trim().is_empty() {
        "You are working on an issue from Linear."
    } else {
        workflow.prompt_template.as_str()
    };
    render_turn_template(source, issue, attempt, turn_number, max_turns)
}

pub fn render_agent_prompt(
    workflow: &LoadedWorkflow,
    agent_name: &str,
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
) -> Result<String, Error> {
    let base_prompt =
        render_turn_prompt(&workflow.definition, issue, attempt, turn_number, max_turns)?;
    apply_agent_prompt_template(
        workflow,
        agent_name,
        base_prompt,
        issue,
        attempt,
        turn_number,
        max_turns,
    )
}

pub fn apply_agent_prompt_template(
    workflow: &LoadedWorkflow,
    agent_name: &str,
    base_prompt: String,
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
) -> Result<String, Error> {
    let Some(agent_prompt) = workflow.agent_prompts.get(agent_name) else {
        return Ok(base_prompt);
    };
    if agent_prompt.prompt_template.trim().is_empty() {
        return Ok(base_prompt);
    }
    let rendered = render_turn_template(
        &agent_prompt.prompt_template,
        issue,
        attempt,
        turn_number,
        max_turns,
    )?;
    if rendered.trim().is_empty() {
        return Ok(base_prompt);
    }
    Ok(format!(
        "{base_prompt}\n\n## Agent Instructions ({agent_name})\n\n{rendered}"
    ))
}

pub fn render_turn_template(
    source: &str,
    issue: &Issue,
    attempt: Option<u32>,
    turn_number: u32,
    max_turns: u32,
) -> Result<String, Error> {
    let mut extra = Object::new();
    extra.insert("turn_number".into(), Value::scalar(turn_number));
    extra.insert("max_turns".into(), Value::scalar(max_turns));
    extra.insert("is_continuation".into(), Value::scalar(turn_number > 1));
    render_issue_template(source, issue, attempt, extra)
}

pub fn render_issue_template(
    source: &str,
    issue: &Issue,
    attempt: Option<u32>,
    extra: Object,
) -> Result<String, Error> {
    let parser = ParserBuilder::with_stdlib()
        .build()
        .map_err(|err| Error::TemplateParse(err.to_string()))?;
    let template = parser
        .parse(source)
        .map_err(|err| Error::TemplateParse(err.to_string()))?;
    let mut globals = object!({
        "issue": issue_to_liquid(issue),
        "attempt": attempt.map(Value::scalar).unwrap_or(Value::Nil),
    });
    for (key, value) in extra {
        globals.insert(key, value);
    }
    template
        .render(&globals)
        .map_err(|err| Error::TemplateRender(err.to_string()))
}

pub fn render_issue_template_with_strings(
    source: &str,
    issue: &Issue,
    attempt: Option<u32>,
    extra: &[(&str, String)],
) -> Result<String, Error> {
    let mut globals = Object::new();
    for (key, value) in extra {
        globals.insert(key.to_string().into(), Value::scalar(value.clone()));
    }
    render_issue_template(source, issue, attempt, globals)
}

pub(crate) fn issue_to_liquid(issue: &Issue) -> Value {
    let mut issue_obj = Object::new();
    issue_obj.insert("id".into(), Value::scalar(issue.id.clone()));
    issue_obj.insert("identifier".into(), Value::scalar(issue.identifier.clone()));
    issue_obj.insert("title".into(), Value::scalar(issue.title.clone()));
    issue_obj.insert(
        "description".into(),
        issue
            .description
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "priority".into(),
        issue.priority.map(Value::scalar).unwrap_or(Value::Nil),
    );
    issue_obj.insert("state".into(), Value::scalar(issue.state.clone()));
    issue_obj.insert(
        "branch_name".into(),
        issue
            .branch_name
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "url".into(),
        issue
            .url
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "author".into(),
        issue
            .author
            .as_ref()
            .map(issue_author_to_liquid)
            .unwrap_or(Value::Nil),
    );
    issue_obj.insert(
        "labels".into(),
        Value::Array(
            issue
                .labels
                .iter()
                .cloned()
                .map(Value::scalar)
                .collect::<Array>(),
        ),
    );
    issue_obj.insert(
        "comments".into(),
        Value::Array(
            issue
                .comments
                .iter()
                .map(|comment| {
                    let mut obj = Object::new();
                    obj.insert("id".into(), Value::scalar(comment.id.clone()));
                    obj.insert("body".into(), Value::scalar(comment.body.clone()));
                    obj.insert(
                        "author".into(),
                        comment
                            .author
                            .as_ref()
                            .map(issue_author_to_liquid)
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "url".into(),
                        comment
                            .url
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "created_at".into(),
                        comment
                            .created_at
                            .map(|value| Value::scalar(value.to_rfc3339()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "updated_at".into(),
                        comment
                            .updated_at
                            .map(|value| Value::scalar(value.to_rfc3339()))
                            .unwrap_or(Value::Nil),
                    );
                    Value::Object(obj)
                })
                .collect::<Array>(),
        ),
    );
    issue_obj.insert(
        "blocked_by".into(),
        Value::Array(
            issue
                .blocked_by
                .iter()
                .map(|blocker| {
                    let mut obj = Object::new();
                    obj.insert(
                        "id".into(),
                        blocker
                            .id
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "identifier".into(),
                        blocker
                            .identifier
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    obj.insert(
                        "state".into(),
                        blocker
                            .state
                            .as_ref()
                            .map(|value| Value::scalar(value.clone()))
                            .unwrap_or(Value::Nil),
                    );
                    Value::Object(obj)
                })
                .collect::<Array>(),
        ),
    );
    Value::Object(issue_obj)
}

pub(crate) fn issue_author_to_liquid(author: &polyphony_core::IssueAuthor) -> Value {
    let mut obj = Object::new();
    obj.insert(
        "id".into(),
        author
            .id
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "username".into(),
        author
            .username
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "display_name".into(),
        author
            .display_name
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "role".into(),
        author
            .role
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "trust_level".into(),
        author
            .trust_level
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    obj.insert(
        "url".into(),
        author
            .url
            .as_ref()
            .map(|value| Value::scalar(value.clone()))
            .unwrap_or(Value::Nil),
    );
    Value::Object(obj)
}

pub(crate) fn resolve_env_token(value: Option<String>) -> Option<String> {
    let value = value?;
    if let Some(name) = value.strip_prefix('$') {
        let resolved = env::var(name).ok()?;
        if resolved.is_empty() {
            None
        } else {
            Some(resolved)
        }
    } else if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub(crate) fn resolve_agent_api_key(kind: &str, api_key: Option<String>) -> Option<String> {
    let fallback = match kind {
        "openai" => env::var("OPENAI_API_KEY").ok(),
        "anthropic" | "claude" => env::var("ANTHROPIC_API_KEY").ok(),
        "copilot" | "github-copilot" => env::var("GITHUB_TOKEN")
            .ok()
            .or_else(|| env::var("GH_TOKEN").ok()),
        "kimi" | "kimi-2.5" | "kimi-k2" | "moonshot" | "moonshotai" => env::var("KIMI_API_KEY")
            .ok()
            .or_else(|| env::var("MOONSHOT_API_KEY").ok()),
        _ => None,
    };
    resolve_env_token(api_key.or(fallback))
}

pub(crate) fn shorthand_agent_profile(config: CodexConfig) -> (String, AgentProfileConfig) {
    let kind = normalize_optional_string(config.kind).unwrap_or_else(|| "codex".into());
    let name = kind.clone();
    let command = normalize_optional_string(config.command)
        .or_else(|| default_single_agent_command(&kind).map(str::to_string));
    (name.clone(), AgentProfileConfig {
        description: None,
        source: Default::default(),
        kind,
        transport: None,
        command,
        fallbacks: Vec::new(),
        model: None,
        reasoning_level: None,
        models: Vec::new(),
        models_command: None,
        fetch_models: true,
        base_url: None,
        api_key: None,
        approval_policy: config.approval_policy,
        thread_sandbox: config.thread_sandbox,
        turn_sandbox_policy: config.turn_sandbox_policy,
        turn_timeout_ms: config.turn_timeout_ms,
        read_timeout_ms: config.read_timeout_ms,
        stall_timeout_ms: config.stall_timeout_ms,
        credits_command: config.credits_command,
        spending_command: config.spending_command,
        use_tmux: false,
        tmux_session_prefix: Some(name),
        interaction_mode: None,
        prompt_mode: None,
        idle_timeout_ms: 5_000,
        completion_sentinel: None,
        env: BTreeMap::new(),
    })
}

pub(crate) fn default_single_agent_command(kind: &str) -> Option<&'static str> {
    match kind {
        "codex" => Some("codex app-server"),
        "mock" => Some("mock"),
        _ => None,
    }
}

pub(crate) fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(crate) fn infer_agent_transport(profile: &AgentProfileConfig) -> AgentTransport {
    match profile.transport.as_deref() {
        Some("mock") => AgentTransport::Mock,
        Some("app_server") => AgentTransport::AppServer,
        Some("rpc") => AgentTransport::Rpc,
        Some("local_cli") => AgentTransport::LocalCli,
        Some("acp") => AgentTransport::Acp,
        Some("acpx") => AgentTransport::Acpx,
        Some("openai_chat") => AgentTransport::OpenAiChat,
        _ => match profile.kind.as_str() {
            "mock" => AgentTransport::Mock,
            "codex" => AgentTransport::AppServer,
            "pi" => AgentTransport::Rpc,
            "acp" => AgentTransport::Acp,
            "acpx" => AgentTransport::Acpx,
            "openai" | "openai-compatible" | "openrouter" | "kimi" | "kimi-2.5" | "kimi-k2"
            | "moonshot" | "moonshotai" | "mistral" | "deepseek" | "cerebras" | "gemini"
            | "zai" | "minimax" | "venice" | "groq" => AgentTransport::OpenAiChat,
            _ => AgentTransport::LocalCli,
        },
    }
}

pub fn agent_definition(name: &str, profile: &AgentProfileConfig) -> AgentDefinition {
    agent_definition_with_pty(name, profile, PtyBackendKind::default())
}

pub fn agent_definition_with_pty(
    name: &str,
    profile: &AgentProfileConfig,
    pty_backend: PtyBackendKind,
) -> AgentDefinition {
    let transport = infer_agent_transport(profile);
    AgentDefinition {
        name: name.to_string(),
        kind: profile.kind.clone(),
        transport,
        command: normalize_optional_string(profile.command.clone())
            .or_else(|| matches!(transport, AgentTransport::Rpc).then(|| "pi".to_string()))
            .or_else(|| matches!(transport, AgentTransport::Acpx).then(|| "acpx".to_string())),
        fallback_agents: profile.fallbacks.clone(),
        model: profile.model.clone(),
        reasoning_level: profile.reasoning_level.clone(),
        models: profile.models.clone(),
        models_command: profile.models_command.clone(),
        fetch_models: profile.fetch_models,
        base_url: profile
            .base_url
            .clone()
            .or_else(|| default_agent_base_url(&profile.kind)),
        api_key: profile.api_key.clone(),
        approval_policy: profile.approval_policy.clone(),
        thread_sandbox: profile.thread_sandbox.clone(),
        turn_sandbox_policy: profile.turn_sandbox_policy.clone(),
        turn_timeout_ms: profile.turn_timeout_ms,
        read_timeout_ms: profile.read_timeout_ms,
        stall_timeout_ms: profile.stall_timeout_ms.unwrap_or(300_000),
        credits_command: profile.credits_command.clone(),
        spending_command: profile.spending_command.clone(),
        use_tmux: profile.use_tmux,
        tmux_session_prefix: profile.tmux_session_prefix.clone(),
        interaction_mode: parse_interaction_mode(profile.interaction_mode.as_deref()),
        prompt_mode: parse_prompt_mode(
            profile.prompt_mode.as_deref(),
            profile.use_tmux,
            profile.interaction_mode.as_deref(),
        ),
        idle_timeout_ms: profile.idle_timeout_ms,
        completion_sentinel: profile.completion_sentinel.clone(),
        env: profile.env.clone(),
        pty_backend,
    }
}

pub(crate) fn default_agent_base_url(kind: &str) -> Option<String> {
    let url = match kind {
        "kimi" | "kimi-2.5" | "kimi-k2" | "moonshot" | "moonshotai" => "https://api.moonshot.ai/v1",
        "openrouter" => "https://openrouter.ai/api/v1",
        "mistral" => "https://api.mistral.ai/v1",
        "deepseek" => "https://api.deepseek.com",
        "cerebras" => "https://api.cerebras.ai/v1",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai",
        "zai" => "https://api.z.ai/api/paas/v4",
        "minimax" => "https://api.minimax.io/v1",
        "venice" => "https://api.venice.ai/api/v1",
        "groq" => "https://api.groq.com/openai/v1",
        _ => return None,
    };
    Some(url.into())
}

pub(crate) fn parse_interaction_mode(value: Option<&str>) -> AgentInteractionMode {
    match value {
        Some("interactive") => AgentInteractionMode::Interactive,
        _ => AgentInteractionMode::OneShot,
    }
}

pub(crate) fn parse_prompt_mode(
    value: Option<&str>,
    use_tmux: bool,
    interaction_mode: Option<&str>,
) -> AgentPromptMode {
    match value {
        Some("stdin") => AgentPromptMode::Stdin,
        Some("tmux_paste") => AgentPromptMode::TmuxPaste,
        _ if interaction_mode == Some("interactive") && use_tmux => AgentPromptMode::TmuxPaste,
        _ if interaction_mode == Some("interactive") => AgentPromptMode::Stdin,
        _ if use_tmux => AgentPromptMode::TmuxPaste,
        _ => AgentPromptMode::Env,
    }
}
