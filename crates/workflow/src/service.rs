use crate::{prelude::*, render::*, *};

pub(crate) fn default_active_states() -> Vec<String> {
    vec!["Todo".to_string(), "In Progress".to_string()]
}

pub(crate) fn default_terminal_states() -> Vec<String> {
    vec![
        "Closed".to_string(),
        "Cancelled".to_string(),
        "Canceled".to_string(),
        "Duplicate".to_string(),
        "Done".to_string(),
        "Human Review".to_string(),
    ]
}

pub(crate) fn apply_tracker_profile(
    mut tracker: TrackerConfig,
    profiles: &HashMap<String, TrackerProfileConfig>,
) -> Result<TrackerConfig, Error> {
    let Some(profile_name) = resolve_env_token(tracker.profile.clone()) else {
        return Ok(tracker);
    };
    tracker.profile = Some(profile_name.clone());
    let profile = profiles.get(&profile_name).ok_or_else(|| {
        Error::InvalidConfig(format!("tracker.profile `{profile_name}` is not defined"))
    })?;

    if tracker.kind == TrackerKind::None
        && let Some(kind) = profile.kind
    {
        tracker.kind = kind;
    }
    if is_default_tracker_endpoint(&tracker.endpoint)
        && let Some(endpoint) = &profile.endpoint
    {
        tracker.endpoint = endpoint.clone();
    }
    if tracker.api_key.is_none() {
        tracker.api_key = profile.api_key.clone();
    }
    if tracker.project_slug.is_none() {
        tracker.project_slug = profile.project_slug.clone();
    }
    if tracker.project_owner.is_none() {
        tracker.project_owner = profile.project_owner.clone();
    }
    if tracker.project_number.is_none() {
        tracker.project_number = profile.project_number;
    }
    if tracker.project_status_field.is_none() {
        tracker.project_status_field = profile.project_status_field.clone();
    }
    if tracker.repository.is_none() {
        tracker.repository = profile.repository.clone();
    }
    if tracker.team_id.is_none() {
        tracker.team_id = profile.team_id.clone();
    }
    if (tracker.active_states.is_empty() || tracker.active_states == default_active_states())
        && !profile.active_states.is_empty()
    {
        tracker.active_states = profile.active_states.clone();
    }
    if (tracker.terminal_states.is_empty() || tracker.terminal_states == default_terminal_states())
        && !profile.terminal_states.is_empty()
    {
        tracker.terminal_states = profile.terminal_states.clone();
    }
    Ok(tracker)
}

pub(crate) fn is_default_tracker_endpoint(endpoint: &str) -> bool {
    endpoint.trim().is_empty() || endpoint == DEFAULT_LINEAR_ENDPOINT
}

pub fn parse_workflow(raw: &str) -> Result<WorkflowDefinition, Error> {
    if !raw.starts_with("---") {
        return Ok(WorkflowDefinition {
            config: YamlValue::Mapping(Mapping::new()),
            prompt_template: raw.trim().to_string(),
        });
    }

    let mut parts = raw.splitn(3, "---");
    let _ = parts.next();
    let front_matter = parts
        .next()
        .ok_or_else(|| Error::WorkflowParse("missing closing front matter".into()))?;
    let body = parts
        .next()
        .ok_or_else(|| Error::WorkflowParse("missing body after front matter".into()))?;
    let config = serde_yaml::from_str::<YamlValue>(front_matter)
        .map_err(|err| Error::WorkflowParse(err.to_string()))?;
    if !matches!(config, YamlValue::Mapping(_)) {
        return Err(Error::FrontMatterNotMap);
    }

    Ok(WorkflowDefinition {
        config,
        prompt_template: body.trim().to_string(),
    })
}

impl ServiceConfig {
    pub fn from_workflow(workflow: &WorkflowDefinition) -> Result<Self, Error> {
        Self::from_workflow_with_configs(workflow, None, None)
    }

    pub(crate) fn from_workflow_with_configs(
        workflow: &WorkflowDefinition,
        user_config_path: Option<&Path>,
        repo_config_path: Option<&Path>,
    ) -> Result<Self, Error> {
        let config =
            Self::build_from_workflow_with_configs(workflow, user_config_path, repo_config_path)?;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn build_from_workflow_with_configs(
        workflow: &WorkflowDefinition,
        user_config_path: Option<&Path>,
        repo_config_path: Option<&Path>,
    ) -> Result<Self, Error> {
        let front_matter = serde_yaml::to_string(&workflow.config)
            .map_err(|err| Error::WorkflowParse(err.to_string()))?;
        let mut builder = Config::builder()
            .set_default("tracker.kind", "none")
            .map_err(config_error)?
            .set_default("tracker.endpoint", DEFAULT_LINEAR_ENDPOINT)
            .map_err(config_error)?
            .set_default("tracker.active_states", default_active_states())
            .map_err(config_error)?
            .set_default("tracker.terminal_states", default_terminal_states())
            .map_err(config_error)?
            .set_default("polling.interval_ms", 60_000)
            .map_err(config_error)?
            .set_default(
                "workspace.root",
                env::temp_dir()
                    .join("symphony_workspaces")
                    .to_string_lossy()
                    .to_string(),
            )
            .map_err(config_error)?
            .set_default("workspace.checkout_kind", "directory")
            .map_err(config_error)?
            .set_default("workspace.sync_on_reuse", true)
            .map_err(config_error)?
            .set_default("workspace.transient_paths", vec!["tmp".to_string()])
            .map_err(config_error)?
            .set_default("hooks.timeout_ms", 60_000)
            .map_err(config_error)?
            .set_default("tools", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("agent.max_concurrent_agents", 10)
            .map_err(config_error)?
            .set_default(
                "agent.max_concurrent_agents_by_state",
                HashMap::<String, i64>::new(),
            )
            .map_err(config_error)?
            .set_default("agent.max_retry_backoff_ms", 300_000)
            .map_err(config_error)?
            .set_default("agent.pty_backend", "portable-pty")
            .map_err(config_error)?
            .set_default("agent.max_turns", 20)
            .map_err(config_error)?
            .set_default("orchestration", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("orchestration.dispatch_mode", "stop")
            .map_err(config_error)?
            .set_default("agents", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("automation.enabled", false)
            .map_err(config_error)?
            .set_default("automation.draft_pull_requests", true)
            .map_err(config_error)?
            .set_default("automation.git.remote_name", "origin")
            .map_err(config_error)?
            .set_default("review_events", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("pipeline.enabled", false)
            .map_err(config_error)?
            .set_default("pipeline.replan_on_failure", false)
            .map_err(config_error)?
            .set_default("feedback", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("server", HashMap::<String, i64>::new())
            .map_err(config_error)?
            .set_default("daemon.listen_address", "127.0.0.1")
            .map_err(config_error)?
            .set_default("daemon.listen_port", 0)
            .map_err(config_error)?
            .set_default("heartbeat.enabled", false)
            .map_err(config_error)?;
        if let Some(path) = user_config_path {
            let path = path.to_string_lossy().to_string();
            builder = builder.add_source(File::new(&path, FileFormat::Toml).required(false));
        }
        builder = builder.add_source(File::from_str(&front_matter, FileFormat::Yaml));
        if let Some(path) = repo_config_path {
            let path = path.to_string_lossy().to_string();
            builder = builder.add_source(File::new(&path, FileFormat::Toml).required(false));
        }
        let built = builder
            .add_source(
                Environment::with_prefix("POLYPHONY")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .map_err(config_error)?;
        let raw = built
            .try_deserialize::<RawServiceConfig>()
            .map_err(config_error)?;
        let tracker = apply_tracker_profile(raw.tracker, &raw.trackers.profiles)?;
        let mut config = ServiceConfig {
            tracker,
            polling: raw.polling,
            workspace: raw.workspace,
            hooks: raw.hooks,
            tools: raw.tools,
            agent: raw.agent,
            orchestration: raw.orchestration,
            agents: raw.agents,
            pipeline: raw.pipeline,
            automation: raw.automation,
            review_events: raw.review_events,
            feedback: raw.feedback,
            server: raw.server,
            daemon: raw.daemon,
            heartbeat: raw.heartbeat,
        };
        config.hydrate_agents(raw.codex, raw.provider)?;
        config.resolve();
        config.normalize();
        Ok(config)
    }

    pub(crate) fn hydrate_agents(
        &mut self,
        codex: Option<CodexConfig>,
        legacy_provider: Option<CodexConfig>,
    ) -> Result<(), Error> {
        let shorthand = match (codex, legacy_provider) {
            (Some(_), Some(_)) => {
                return Err(Error::InvalidConfig(
                    "`codex` and legacy `provider` cannot both be configured".into(),
                ));
            },
            (Some(codex), None) => Some(codex),
            (None, Some(provider)) => Some(provider),
            (None, None) => None,
        };
        if shorthand.is_some() && self.agents.is_configured() {
            return Err(Error::InvalidConfig(
                "`codex` and legacy `provider` cannot be combined with `agents`; use `agents` for multi-agent routing".into(),
            ));
        }

        if let Some(shorthand) = shorthand {
            let (default_name, profile) = shorthand_agent_profile(shorthand);
            self.agents.default = Some(default_name.clone());
            self.agents.profiles.insert(default_name, profile);
        }

        if self.agents.default.is_none() && self.agents.profiles.len() == 1 {
            self.agents.default = self.agents.profiles.keys().next().cloned();
        }

        Ok(())
    }

    pub(crate) fn apply_agent_prompt_overrides(
        &mut self,
        prompts: &HashMap<String, AgentPromptConfig>,
    ) {
        for (name, prompt) in prompts {
            let profile = self.agents.profiles.entry(name.clone()).or_default();
            profile.apply_override(&prompt.profile);
            profile.source = prompt.source;
        }
        if self.agents.default.is_none() {
            if self.agents.profiles.contains_key("implementer") {
                self.agents.default = Some("implementer".into());
            } else if self.agents.profiles.len() == 1 {
                self.agents.default = self.agents.profiles.keys().next().cloned();
            }
        }
        if self.agents.reviewer.is_none() && self.agents.profiles.contains_key("reviewer") {
            self.agents.reviewer = Some("reviewer".into());
        }
    }

    pub(crate) fn resolve(&mut self) {
        let tracker_api_key = match self.tracker.kind {
            TrackerKind::Linear => self
                .tracker
                .api_key
                .clone()
                .or_else(|| env::var("LINEAR_API_KEY").ok()),
            TrackerKind::Github => self
                .tracker
                .api_key
                .clone()
                .or_else(|| env::var("GITHUB_TOKEN").ok())
                .or_else(|| env::var("GH_TOKEN").ok()),
            _ => self.tracker.api_key.clone(),
        };
        self.tracker.api_key = resolve_env_token(tracker_api_key);
        self.workspace.root = expand_path_like(&self.workspace.root);
        self.workspace.source_repo_path = self
            .workspace
            .source_repo_path
            .take()
            .map(|path| expand_path_like(&path));
        self.orchestration.router_agent = resolve_env_token(self.orchestration.router_agent.take());
        self.pipeline.planner_agent = resolve_env_token(self.pipeline.planner_agent.take());
        self.pipeline.planner_prompt = resolve_env_token(self.pipeline.planner_prompt.take());
        self.pipeline.validation_agent = resolve_env_token(self.pipeline.validation_agent.take());
        self.agents.reviewer = resolve_env_token(self.agents.reviewer.take());
        self.automation.review_agent = resolve_env_token(self.automation.review_agent.take());
        self.automation.commit_message = resolve_env_token(self.automation.commit_message.take());
        self.automation.pr_title = resolve_env_token(self.automation.pr_title.take());
        self.automation.pr_body = resolve_env_token(self.automation.pr_body.take());
        self.automation.review_prompt = resolve_env_token(self.automation.review_prompt.take());
        self.review_events.pr_reviews.agent =
            resolve_env_token(self.review_events.pr_reviews.agent.take());
        self.review_events.pr_reviews.prompt =
            resolve_env_token(self.review_events.pr_reviews.prompt.take());
        self.automation.git.author.name = resolve_env_token(self.automation.git.author.name.take());
        self.automation.git.author.email =
            resolve_env_token(self.automation.git.author.email.take());
        self.feedback.action_base_url = resolve_env_token(self.feedback.action_base_url.take());
        normalize_tool_names(&mut self.tools.allow);
        normalize_tool_names(&mut self.tools.deny);
        for policy in self.tools.by_agent.values_mut() {
            normalize_tool_names(&mut policy.allow);
            normalize_tool_names(&mut policy.deny);
        }
        for config in self.feedback.telegram.values_mut() {
            config.bot_token = resolve_env_token(config.bot_token.take())
                .or_else(|| env::var("TELEGRAM_BOT_TOKEN").ok());
            config.chat_id = resolve_env_token(Some(config.chat_id.clone())).unwrap_or_default();
        }
        for config in self.feedback.webhook.values_mut() {
            config.url = resolve_env_token(config.url.take());
            config.bearer_token = resolve_env_token(config.bearer_token.take());
        }
        for profile in self.agents.profiles.values_mut() {
            profile.api_key = resolve_agent_api_key(&profile.kind, profile.api_key.clone());
            profile.base_url = resolve_env_token(profile.base_url.take());
            profile.command = resolve_env_token(profile.command.take());
            profile.models_command = resolve_env_token(profile.models_command.take());
            profile.credits_command = resolve_env_token(profile.credits_command.take());
            profile.spending_command = resolve_env_token(profile.spending_command.take());
            profile.env = profile
                .env
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        resolve_env_token(Some(value.clone())).unwrap_or_default(),
                    )
                })
                .collect();
        }
    }

    pub(crate) fn normalize(&mut self) {
        self.workspace.transient_paths = self
            .workspace
            .transient_paths
            .drain(..)
            .filter(|path| !path.trim().is_empty())
            .collect();
        self.agent.max_concurrent_agents_by_state = self
            .agent
            .max_concurrent_agents_by_state
            .drain()
            .filter_map(|(state, limit)| {
                if limit == 0 {
                    None
                } else {
                    Some((state.to_ascii_lowercase(), limit))
                }
            })
            .collect();
        self.agents.by_state = self
            .agents
            .by_state
            .drain()
            .map(|(state, agent)| (state.to_ascii_lowercase(), agent))
            .collect();
        self.agents.by_label = self
            .agents
            .by_label
            .drain()
            .map(|(label, agent)| (label.to_ascii_lowercase(), agent))
            .collect();
        for (name, profile) in &mut self.agents.profiles {
            if profile.kind.trim().is_empty() {
                profile.kind = name.clone();
            }
            if profile.turn_timeout_ms == 0 {
                profile.turn_timeout_ms = 3_600_000;
            }
            if profile.read_timeout_ms == 0 {
                profile.read_timeout_ms = 5_000;
            }
            if profile.stall_timeout_ms.is_none() {
                profile.stall_timeout_ms = Some(300_000);
            }
            if profile.tmux_session_prefix.is_none() {
                profile.tmux_session_prefix = Some(name.clone());
            }
            if profile.idle_timeout_ms == 0 {
                profile.idle_timeout_ms = 5_000;
            }
            profile.fallbacks = profile
                .fallbacks
                .drain(..)
                .filter(|fallback| !fallback.trim().is_empty() && fallback != name)
                .collect();
        }
        if self.automation.git.remote_name.trim().is_empty() {
            self.automation.git.remote_name = "origin".into();
        }
        self.review_events.pr_reviews.provider = self
            .review_events
            .pr_reviews
            .provider
            .trim()
            .to_ascii_lowercase();
        self.review_events.pr_reviews.comment_mode = self
            .review_events
            .pr_reviews
            .comment_mode
            .trim()
            .to_ascii_lowercase();
        self.review_events.pr_reviews.only_labels = self
            .review_events
            .pr_reviews
            .only_labels
            .drain(..)
            .map(|label| label.trim().to_ascii_lowercase())
            .filter(|label| !label.is_empty())
            .collect();
        self.review_events.pr_reviews.ignore_labels = self
            .review_events
            .pr_reviews
            .ignore_labels
            .drain(..)
            .map(|label| label.trim().to_ascii_lowercase())
            .filter(|label| !label.is_empty())
            .collect();
        self.review_events.pr_reviews.ignore_authors = self
            .review_events
            .pr_reviews
            .ignore_authors
            .drain(..)
            .map(|author| author.trim().to_ascii_lowercase())
            .filter(|author| !author.is_empty())
            .collect();
        if self.review_events.pr_reviews.provider.is_empty() {
            self.review_events.pr_reviews.provider = "github".into();
        }
        if self.review_events.pr_reviews.comment_mode.is_empty() {
            self.review_events.pr_reviews.comment_mode = "summary".into();
        }
        if self.review_events.pr_reviews.debounce_seconds == 0 {
            self.review_events.pr_reviews.debounce_seconds = 180;
        }
        self.orchestration.mode = self.orchestration.mode.trim().to_ascii_lowercase();
        if self.orchestration.mode.is_empty() {
            self.orchestration.mode = "advisory".into();
        }
        self.orchestration.dispatch_mode =
            self.orchestration.dispatch_mode.trim().to_ascii_lowercase();
        if self.orchestration.dispatch_mode.is_empty() {
            self.orchestration.dispatch_mode = "stop".into();
        }
        self.feedback.offered = self
            .feedback
            .offered
            .drain(..)
            .map(|item| item.to_ascii_lowercase())
            .filter(|item| !item.trim().is_empty())
            .collect();
        if self.hooks.timeout_ms == 0 {
            self.hooks.timeout_ms = 60_000;
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        for agent_name in self.tools.by_agent.keys() {
            if !self.agents.profiles.contains_key(agent_name) {
                return Err(Error::InvalidConfig(format!(
                    "tools.by_agent.{agent_name} references unknown agent `{agent_name}`"
                )));
            }
        }
        if self.tracker.kind == TrackerKind::Linear {
            if self
                .tracker
                .api_key
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "tracker.api_key is required for linear".into(),
                ));
            }
            if self
                .tracker
                .project_slug
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "tracker.project_slug is required for linear".into(),
                ));
            }
        }
        if self.tracker.kind == TrackerKind::Github
            && self
                .tracker
                .repository
                .as_deref()
                .unwrap_or_default()
                .is_empty()
        {
            return Err(Error::InvalidConfig(
                "tracker.repository is required for github".into(),
            ));
        }
        if self.agents.profiles.is_empty() {
            if let Some(default_agent) = self.agents.default.as_deref() {
                return Err(Error::InvalidConfig(format!(
                    "agents.default `{default_agent}` is not defined"
                )));
            }
            if !self.agents.by_state.is_empty() || !self.agents.by_label.is_empty() {
                return Err(Error::InvalidConfig(
                    "agent selectors require at least one configured agent profile".into(),
                ));
            }
        } else {
            let default_agent = self
                .agents
                .default
                .as_deref()
                .ok_or_else(|| Error::InvalidConfig("agents.default is required".into()))?;
            if !self.agents.profiles.contains_key(default_agent) {
                return Err(Error::InvalidConfig(format!(
                    "agents.default `{default_agent}` is not defined"
                )));
            }
        }
        for (agent_name, profile) in &self.agents.profiles {
            if matches!(
                infer_agent_transport(profile),
                AgentTransport::AppServer | AgentTransport::LocalCli | AgentTransport::Acp
            ) && profile
                .command
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(Error::InvalidConfig(format!(
                    "agents.profiles.{agent_name}.command must be non-empty"
                )));
            }
            if matches!(infer_agent_transport(profile), AgentTransport::OpenAiChat)
                && profile
                    .model
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && profile.models.is_empty()
                && !profile.fetch_models
            {
                return Err(Error::InvalidConfig(format!(
                    "agents.profiles.{agent_name}.model is required for openai_chat agents when automatic model discovery is disabled"
                )));
            }
            for fallback in &profile.fallbacks {
                if fallback == agent_name {
                    return Err(Error::InvalidConfig(format!(
                        "agents.profiles.{agent_name}.fallbacks cannot contain itself"
                    )));
                }
                if !self.agents.profiles.contains_key(fallback) {
                    return Err(Error::InvalidConfig(format!(
                        "agents.profiles.{agent_name}.fallbacks references unknown agent `{fallback}`"
                    )));
                }
            }
        }
        for configured_agent in self
            .agents
            .by_state
            .values()
            .chain(self.agents.by_label.values())
        {
            if !self.agents.profiles.contains_key(configured_agent) {
                return Err(Error::InvalidConfig(format!(
                    "agent selector references unknown agent `{configured_agent}`"
                )));
            }
        }
        if self.workspace.checkout_kind != CheckoutKind::Directory
            && self.workspace.source_repo_path.is_none()
            && self.workspace.clone_url.is_none()
        {
            return Err(Error::InvalidConfig(
                "workspace.source_repo_path or workspace.clone_url is required for git-backed workspaces".into(),
            ));
        }
        if self.automation.enabled {
            if self.tracker.kind != TrackerKind::Github {
                return Err(Error::InvalidConfig(
                    "automation.enabled currently requires tracker.kind = github".into(),
                ));
            }
            if self.workspace.checkout_kind == CheckoutKind::Directory {
                return Err(Error::InvalidConfig(
                    "automation.enabled requires a git-backed workspace checkout".into(),
                ));
            }
            if let Some(agent_name) = &self.automation.review_agent
                && !self.agents.profiles.contains_key(agent_name)
            {
                return Err(Error::InvalidConfig(format!(
                    "automation.review_agent `{agent_name}` is not defined"
                )));
            }
        }
        if let Some(agent_name) = &self.agents.reviewer
            && !self.agents.profiles.contains_key(agent_name)
        {
            return Err(Error::InvalidConfig(format!(
                "agents.reviewer `{agent_name}` is not defined"
            )));
        }
        if let Some(agent_name) = &self.orchestration.router_agent
            && !self.agents.profiles.contains_key(agent_name)
        {
            return Err(Error::InvalidConfig(format!(
                "orchestration.router_agent `{agent_name}` is not defined"
            )));
        }
        if !matches!(self.orchestration.mode.as_str(), "advisory" | "enforced") {
            return Err(Error::InvalidConfig(
                "orchestration.mode must be `advisory` or `enforced`".into(),
            ));
        }
        if !matches!(
            self.orchestration.dispatch_mode.as_str(),
            "manual" | "automatic" | "nightshift" | "idle" | "stop"
        ) {
            return Err(Error::InvalidConfig(
                "orchestration.dispatch_mode must be `manual`, `automatic`, `nightshift`, `idle`, or `stop`".into(),
            ));
        }
        if self.tracker.kind == TrackerKind::Github {
            if self.review_events.pr_reviews.provider != "github" {
                return Err(Error::InvalidConfig(
                    "review_events.pr_reviews.provider must be `github`".into(),
                ));
            }
            if self
                .tracker
                .repository
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(Error::InvalidConfig(
                    "review_events.pr_reviews requires tracker.repository".into(),
                ));
            }
            if !matches!(
                self.review_events.pr_reviews.comment_mode.as_str(),
                "summary" | "inline"
            ) {
                return Err(Error::InvalidConfig(
                    "review_events.pr_reviews.comment_mode must be `summary` or `inline`".into(),
                ));
            }
            if let Some(agent_name) = &self.review_events.pr_reviews.agent
                && !self.agents.profiles.contains_key(agent_name)
            {
                return Err(Error::InvalidConfig(format!(
                    "review_events.pr_reviews.agent `{agent_name}` is not defined"
                )));
            }
            if self
                .review_events
                .pr_reviews
                .only_labels
                .iter()
                .any(|label| self.review_events.pr_reviews.ignore_labels.contains(label))
            {
                return Err(Error::InvalidConfig(
                    "review_events.pr_reviews.only_labels and ignore_labels must not overlap"
                        .into(),
                ));
            }
        }
        if self.pipeline.enabled {
            if let Some(agent_name) = &self.pipeline.planner_agent
                && !self.agents.profiles.contains_key(agent_name)
            {
                return Err(Error::InvalidConfig(format!(
                    "pipeline.planner_agent `{agent_name}` is not defined"
                )));
            }
            if let Some(agent_name) = &self.pipeline.validation_agent
                && !self.agents.profiles.contains_key(agent_name)
            {
                return Err(Error::InvalidConfig(format!(
                    "pipeline.validation_agent `{agent_name}` is not defined"
                )));
            }
            for stage in &self.pipeline.stages {
                if let Some(agent_name) = &stage.agent
                    && !self.agents.profiles.contains_key(agent_name)
                {
                    return Err(Error::InvalidConfig(format!(
                        "pipeline.stages references unknown agent `{agent_name}`"
                    )));
                }
            }
        }
        if self.router_agent_name().is_none()
            && self.pipeline.enabled
            && self.pipeline.stages.is_empty()
        {
            return Err(Error::InvalidConfig(
                "pipeline.enabled requires orchestration.router_agent, pipeline.planner_agent, or pipeline.stages".into(),
            ));
        }
        for offered in &self.feedback.offered {
            match offered.as_str() {
                "telegram" | "webhook" => {},
                _ => {
                    return Err(Error::InvalidConfig(format!(
                        "feedback.offered contains unknown channel `{offered}`"
                    )));
                },
            }
        }
        for (name, config) in &self.feedback.telegram {
            if config.chat_id.trim().is_empty() {
                return Err(Error::InvalidConfig(format!(
                    "feedback.telegram.{name}.chat_id must be non-empty"
                )));
            }
            if config
                .bot_token
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(Error::InvalidConfig(format!(
                    "feedback.telegram.{name}.bot_token must be non-empty"
                )));
            }
        }
        for (name, config) in &self.feedback.webhook {
            if config.url.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(Error::InvalidConfig(format!(
                    "feedback.webhook.{name}.url must be non-empty"
                )));
            }
        }
        if self.heartbeat.enabled
            && let Some(agent_name) = &self.heartbeat.agent
            && !self.agents.profiles.contains_key(agent_name)
        {
            return Err(Error::InvalidConfig(format!(
                "heartbeat.agent `{agent_name}` is not defined"
            )));
        }
        Ok(())
    }

    pub fn tracker_query(&self) -> TrackerQuery {
        TrackerQuery {
            project_slug: self.tracker.project_slug.clone(),
            repository: self.tracker.repository.clone(),
            active_states: self.tracker.active_states.clone(),
            terminal_states: self.tracker.terminal_states.clone(),
        }
    }

    pub fn is_active_state(&self, state: &str) -> bool {
        self.tracker
            .active_states
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(state))
    }

    pub fn is_terminal_state(&self, state: &str) -> bool {
        self.tracker
            .terminal_states
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(state))
    }

    pub fn state_concurrency_limit(&self, state: &str) -> Option<usize> {
        self.agent
            .max_concurrent_agents_by_state
            .get(&state.to_ascii_lowercase())
            .copied()
    }

    pub fn all_agents(&self) -> Vec<AgentDefinition> {
        let pty = self.agent.pty_backend;
        self.agents
            .profiles
            .iter()
            .map(|(name, profile)| render::agent_definition_with_pty(name, profile, pty))
            .collect()
    }

    pub fn has_dispatch_agents(&self) -> bool {
        !self.agents.profiles.is_empty()
    }

    pub fn pipeline_active(&self) -> bool {
        self.pipeline.enabled || self.orchestration.router_agent.is_some()
    }

    pub fn router_agent_name(&self) -> Option<&str> {
        self.orchestration
            .router_agent
            .as_deref()
            .or(self.pipeline.planner_agent.as_deref())
    }

    pub fn startup_dispatch_mode(&self) -> polyphony_core::DispatchMode {
        match self.orchestration.dispatch_mode.as_str() {
            "automatic" => polyphony_core::DispatchMode::Automatic,
            "nightshift" => polyphony_core::DispatchMode::Nightshift,
            "idle" => polyphony_core::DispatchMode::Idle,
            "stop" => polyphony_core::DispatchMode::Stop,
            _ => polyphony_core::DispatchMode::Manual,
        }
    }

    pub fn candidate_agents_for_issue(&self, issue: &Issue) -> Result<Vec<AgentDefinition>, Error> {
        if self.agents.profiles.is_empty() {
            return Ok(Vec::new());
        }
        let selected_name =
            if let Some(agent_name) = self.agents.by_state.get(&issue.state.to_ascii_lowercase()) {
                agent_name.clone()
            } else if let Some(agent_name) = issue.labels.iter().find_map(|label| {
                self.agents
                    .by_label
                    .get(&label.to_ascii_lowercase())
                    .cloned()
            }) {
                agent_name
            } else {
                self.agents
                    .default
                    .clone()
                    .ok_or_else(|| Error::InvalidConfig("agents.default is required".into()))?
            };

        self.expand_agent_candidates(&selected_name)
    }

    pub fn select_agent_for_issue(&self, issue: &Issue) -> Result<AgentDefinition, Error> {
        self.candidate_agents_for_issue(issue)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidConfig("no candidate agents are configured".into()))
    }

    pub fn review_agent(&self) -> Result<Option<AgentDefinition>, Error> {
        let Some(agent_name) = &self.automation.review_agent else {
            return Ok(None);
        };
        let profile =
            self.agents.profiles.get(agent_name).ok_or_else(|| {
                Error::InvalidConfig(format!("unknown review agent `{agent_name}`"))
            })?;
        Ok(Some(render::agent_definition_with_pty(
            agent_name,
            profile,
            self.agent.pty_backend,
        )))
    }

    pub fn pr_review_agent(&self) -> Result<Option<AgentDefinition>, Error> {
        let selected_name = self
            .agents
            .reviewer
            .as_ref()
            .or_else(|| {
                self.agents
                    .profiles
                    .get_key_value("reviewer")
                    .map(|(name, _)| name)
            })
            .or(self.review_events.pr_reviews.agent.as_ref())
            .or(self.automation.review_agent.as_ref())
            .or(self.agents.default.as_ref());
        let Some(agent_name) = selected_name else {
            return Ok(None);
        };
        let profile = self.agents.profiles.get(agent_name).ok_or_else(|| {
            Error::InvalidConfig(format!("unknown PR review agent `{agent_name}`"))
        })?;
        Ok(Some(render::agent_definition_with_pty(
            agent_name,
            profile,
            self.agent.pty_backend,
        )))
    }

    pub fn expand_agent_candidates(
        &self,
        selected_name: &str,
    ) -> Result<Vec<AgentDefinition>, Error> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![selected_name.to_string()];
        let mut candidates = Vec::new();
        while let Some(agent_name) = stack.pop() {
            if !seen.insert(agent_name.clone()) {
                continue;
            }
            let profile = self.agents.profiles.get(&agent_name).ok_or_else(|| {
                Error::InvalidConfig(format!("unknown selected agent `{agent_name}`"))
            })?;
            stack.extend(profile.fallbacks.iter().rev().cloned());
            candidates.push(render::agent_definition_with_pty(
                &agent_name,
                profile,
                self.agent.pty_backend,
            ));
        }
        Ok(candidates)
    }
}

fn normalize_tool_names(entries: &mut Vec<String>) {
    *entries = entries
        .drain(..)
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect();
}

impl AgentsConfig {
    pub(crate) fn is_configured(&self) -> bool {
        self.default.is_some()
            || !self.by_state.is_empty()
            || !self.by_label.is_empty()
            || !self.profiles.is_empty()
    }
}

impl AgentProfileConfig {
    pub(crate) fn apply_override(&mut self, override_config: &AgentProfileOverride) {
        if let Some(value) = &override_config.description {
            self.description = Some(value.clone());
        }
        if let Some(value) = &override_config.kind {
            self.kind = value.clone();
        }
        if let Some(value) = &override_config.transport {
            self.transport = Some(value.clone());
        }
        if let Some(value) = &override_config.command {
            self.command = Some(value.clone());
        }
        if let Some(value) = &override_config.fallbacks {
            self.fallbacks = value.clone();
        }
        if let Some(value) = &override_config.model {
            self.model = Some(value.clone());
        }
        if let Some(value) = &override_config.reasoning_level {
            self.reasoning_level = Some(value.clone());
        }
        if let Some(value) = &override_config.models {
            self.models = value.clone();
        }
        if let Some(value) = &override_config.models_command {
            self.models_command = Some(value.clone());
        }
        if let Some(value) = override_config.fetch_models {
            self.fetch_models = value;
        }
        if let Some(value) = &override_config.base_url {
            self.base_url = Some(value.clone());
        }
        if let Some(value) = &override_config.api_key {
            self.api_key = Some(value.clone());
        }
        if let Some(value) = &override_config.approval_policy {
            self.approval_policy = Some(value.clone());
        }
        if let Some(value) = &override_config.thread_sandbox {
            self.thread_sandbox = Some(value.clone());
        }
        if let Some(value) = &override_config.turn_sandbox_policy {
            self.turn_sandbox_policy = Some(value.clone());
        }
        if let Some(value) = override_config.turn_timeout_ms {
            self.turn_timeout_ms = value;
        }
        if let Some(value) = override_config.read_timeout_ms {
            self.read_timeout_ms = value;
        }
        if let Some(value) = override_config.stall_timeout_ms {
            self.stall_timeout_ms = Some(value);
        }
        if let Some(value) = &override_config.credits_command {
            self.credits_command = Some(value.clone());
        }
        if let Some(value) = &override_config.spending_command {
            self.spending_command = Some(value.clone());
        }
        if let Some(value) = override_config.use_tmux {
            self.use_tmux = value;
        }
        if let Some(value) = &override_config.tmux_session_prefix {
            self.tmux_session_prefix = Some(value.clone());
        }
        if let Some(value) = &override_config.interaction_mode {
            self.interaction_mode = Some(value.clone());
        }
        if let Some(value) = &override_config.prompt_mode {
            self.prompt_mode = Some(value.clone());
        }
        if let Some(value) = override_config.idle_timeout_ms {
            self.idle_timeout_ms = value;
        }
        if let Some(value) = &override_config.completion_sentinel {
            self.completion_sentinel = Some(value.clone());
        }
        if let Some(value) = &override_config.env {
            self.env = value.clone();
        }
    }
}

impl AgentProfileOverride {
    pub(crate) fn merge(&mut self, other: Self) {
        if other.description.is_some() {
            self.description = other.description;
        }
        if other.kind.is_some() {
            self.kind = other.kind;
        }
        if other.transport.is_some() {
            self.transport = other.transport;
        }
        if other.command.is_some() {
            self.command = other.command;
        }
        if other.fallbacks.is_some() {
            self.fallbacks = other.fallbacks;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
        if other.reasoning_level.is_some() {
            self.reasoning_level = other.reasoning_level;
        }
        if other.models.is_some() {
            self.models = other.models;
        }
        if other.models_command.is_some() {
            self.models_command = other.models_command;
        }
        if other.fetch_models.is_some() {
            self.fetch_models = other.fetch_models;
        }
        if other.base_url.is_some() {
            self.base_url = other.base_url;
        }
        if other.api_key.is_some() {
            self.api_key = other.api_key;
        }
        if other.approval_policy.is_some() {
            self.approval_policy = other.approval_policy;
        }
        if other.thread_sandbox.is_some() {
            self.thread_sandbox = other.thread_sandbox;
        }
        if other.turn_sandbox_policy.is_some() {
            self.turn_sandbox_policy = other.turn_sandbox_policy;
        }
        if other.turn_timeout_ms.is_some() {
            self.turn_timeout_ms = other.turn_timeout_ms;
        }
        if other.read_timeout_ms.is_some() {
            self.read_timeout_ms = other.read_timeout_ms;
        }
        if other.stall_timeout_ms.is_some() {
            self.stall_timeout_ms = other.stall_timeout_ms;
        }
        if other.credits_command.is_some() {
            self.credits_command = other.credits_command;
        }
        if other.spending_command.is_some() {
            self.spending_command = other.spending_command;
        }
        if other.use_tmux.is_some() {
            self.use_tmux = other.use_tmux;
        }
        if other.tmux_session_prefix.is_some() {
            self.tmux_session_prefix = other.tmux_session_prefix;
        }
        if other.interaction_mode.is_some() {
            self.interaction_mode = other.interaction_mode;
        }
        if other.prompt_mode.is_some() {
            self.prompt_mode = other.prompt_mode;
        }
        if other.idle_timeout_ms.is_some() {
            self.idle_timeout_ms = other.idle_timeout_ms;
        }
        if other.completion_sentinel.is_some() {
            self.completion_sentinel = other.completion_sentinel;
        }
        if other.env.is_some() {
            self.env = other.env;
        }
    }
}

pub(crate) fn expand_path_like(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    let expanded = if let Some(name) = value.strip_prefix('$') {
        env::var(name).unwrap_or_default()
    } else {
        value.to_string()
    };
    if expanded.starts_with('~')
        && let Some(home) = dirs::home_dir()
    {
        return home.join(expanded.trim_start_matches("~/"));
    }
    PathBuf::from(expanded)
}

pub(crate) fn workflow_root_dir(path: &Path) -> Result<PathBuf, Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    match parent {
        Some(parent) => Ok(parent.to_path_buf()),
        None => env::current_dir().map_err(|error| {
            Error::Config(format!("resolving workflow directory failed: {error}"))
        }),
    }
}

pub(crate) fn config_error(error: config::ConfigError) -> Error {
    Error::Config(error.to_string())
}

pub(crate) const fn default_true() -> bool {
    true
}
