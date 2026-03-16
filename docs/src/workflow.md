# Workflow Configuration

`polyphony` starts from a repository-owned `WORKFLOW.md` file, but the CLI first loads
`~/.config/polyphony/config.toml`, then merges the repo workflow, then applies optional
repo-local overrides from `.polyphony/config.toml`. `WORKFLOW.md` contains YAML front matter
followed by the worker prompt template.

If `WORKFLOW.md` is missing, the CLI offers to create a starter file in TUI mode and writes it
automatically in `--no-tui` mode.

Copyable full-file references live under [`templates/`](../../templates) and
[`templates/examples/`](../../templates/examples).

Merge order is:

1. built-in defaults
2. `~/.config/polyphony/config.toml`
3. `WORKFLOW.md` front matter
4. `.polyphony/config.toml`
5. `POLYPHONY__...` environment variables

## Shape

Treat `~/.config/polyphony/config.toml` as user-local shared state, for example credentials,
reusable agent profiles, shared tracker credential profiles, and personal defaults. Treat `WORKFLOW.md` as the shared repo-owned
workflow policy and prompt. Use `.polyphony/config.toml` for local repository wiring such as
`tracker.profile`, `tracker.repository`, `tracker.project_slug`, `workspace.source_repo_path`, and `workspace.clone_url`
when you do not want to edit the checked-in workflow.

The current workspace configuration covers:

- `tracker`: tracker kind, repository or project identifiers, API settings, and state mapping
- `polling`: tracker polling interval, default `60000` ms (60 seconds)
- `workspace`: root path, checkout strategy, reuse behavior, and transient cleanup paths
- `hooks`: optional shell hooks around workspace lifecycle events, with captured stdout/stderr logged in truncated form
- `tools`: optional built-in LLM tool allow/deny policy, with per-agent overrides
- `agent`: global concurrency, turn, and retry limits
- `codex`: optional single-agent shorthand for one Codex app-server profile
- `agents`: named agent profiles and routing rules
- `automation`: optional post-run git and PR handoff settings
- `feedback`: outbound notification sink configuration
- `pipeline`: multi-stage pipeline orchestration settings
- `server`: optional server settings

## Example

- Start from [`templates/WORKFLOW.md`](../../templates/WORKFLOW.md) for the default generated
  `WORKFLOW.md`.
- Use
  [`templates/examples/WORKFLOW.multi-agent.md`](../../templates/examples/WORKFLOW.multi-agent.md)
  for a full multi-agent workflow example.

## Single-Agent Shorthand

For a simple single-agent workflow, `codex` can stand in for a one-profile `agents` section:

- Use
  [`templates/examples/WORKFLOW.codex-shorthand.md`](../../templates/examples/WORKFLOW.codex-shorthand.md)
  as a full-file example.

That shorthand is normalized into a default `codex` agent internally. The legacy top-level
`provider` block is still accepted as a deprecated alias for the same single-agent mode.

## Agent Routing

The `agents` section supports:

- `default`: fallback profile name
- `by_state`: profile overrides keyed by issue state
- `by_label`: profile overrides keyed by issue label
- `profiles`: named transport definitions

When `profiles` is empty, Polyphony stays in tracker-only mode: it polls and displays issues, but
does not dispatch work to an agent yet. That is useful when the repo-local workflow is configured
to read GitHub or Linear issues before any agent profile is wired in.

Full copyable files for the config layers are:

- [`templates/config.toml`](../../templates/config.toml)
- [`templates/repo-config.toml`](../../templates/repo-config.toml)
- [`templates/examples/config.multi-provider.toml`](../../templates/examples/config.multi-provider.toml)
- [`templates/examples/repo-config.github.toml`](../../templates/examples/repo-config.github.toml)
- [`templates/examples/repo-config.linear.toml`](../../templates/examples/repo-config.linear.toml)

Shared tracker credentials can be defined in `~/.config/polyphony/config.toml` under
`trackers.profiles.<name>`, then selected per repo with `tracker.profile = "<name>"`.

Current transport styles in the codebase are:

- `app_server`
- `local_cli`
- `openai_chat`

Each agent profile can also control:

- `model`, `models`, and `models_command` for single-model or discovered-model setups
- `fetch_models` to enable automatic model discovery
- `api_key` for `openai_chat` profiles when they are actually used, while missing keys only disable `/models` discovery during startup
- `sandbox.backend`, `sandbox.profile`, and `sandbox.policy` for backend-selected sandboxing, with current built-ins `host` and `codex`
- `runtime.backend`, `runtime.endpoint`, and `runtime.model_source` for local runtime selection. The config surface accepts `provider`, `openai_compatible`, `llama_cpp`, `ollama`, and `lm_studio`; the built-in dispatch backends currently wire `provider`, `openai_compatible`, `ollama`, and `lm_studio`
- `approval_policy`, `thread_sandbox`, and `turn_sandbox_policy` for app-server-backed agents
- `turn_timeout_ms`, `read_timeout_ms`, and `stall_timeout_ms` for agent timing controls
- `stall_timeout_ms <= 0` to disable orchestrator stall detection
- `agent.continuation_prompt` as an optional Liquid template for later live turns on the same thread
- `interaction_mode` with `one_shot` or `interactive`
- `prompt_mode` with `env`, `stdin`, or `tmux_paste`
- `idle_timeout_ms` for interactive local CLI polling
- `completion_sentinel` for explicit interactive completion detection
- `use_tmux` and `tmux_session_prefix` for local CLI automation under tmux
- `env` for provider-specific environment injection

## Built-In Tools

The optional `tools` section enables a small built-in tool registry that provider runtimes can
advertise to tool-capable models.

Current built-in tools:

- `workspace_list_files`
- `workspace_read_file`
- `workspace_search`
- `issue_update`
- `issue_comment`
- `pr_comment`
- `linear_graphql` for raw Linear GraphQL access using the configured tracker auth

The policy shape is:

- `tools.enabled`
- `tools.allow`
- `tools.deny`
- `tools.by_agent.<name>.allow`
- `tools.by_agent.<name>.deny`

Example:

```yaml
tools:
  enabled: true
  allow:
    - workspace_list_files
    - workspace_read_file
    - workspace_search
    - issue_update
    - issue_comment
    - linear_graphql
  by_agent:
    reviewer:
      allow:
        - workspace_read_file
        - workspace_search
        - pr_comment
        - linear_graphql
```

See [Built-In Tools](./tools.md) for the runtime model and design constraints.

## Workspace Provisioning

`workspace.checkout_kind` currently supports:

- `directory`
- `linked_worktree`
- `discrete_clone`

That separation matters because workspace lifecycle is independent from tracker and agent logic.

## Post-Run Handoff

`automation` currently supports:

- `enabled` to turn on git commit, push, and PR creation after successful runs
- `draft_pull_requests` to create draft PRs by default
- `review_agent` to choose a second-pass reviewer agent
- `commit_message`, `pr_title`, `pr_body`, and `review_prompt` as Liquid templates
- `git.remote_name` plus optional author name/email overrides

`feedback` currently supports:

- `offered` to limit which sink kinds are enabled
- `telegram.<name>` with `bot_token` and `chat_id`
- `webhook.<name>` with `url` and optional `bearer_token`

Template fields include the normal `issue.*` fields plus handoff values such as
`base_branch`, `head_branch`, `commit_sha`, and `pull_request_url` where
relevant.

Use
[`templates/examples/WORKFLOW.automation-feedback.md`](../../templates/examples/WORKFLOW.automation-feedback.md)
as a full-file example that wires automation and feedback together.

## Pipeline Orchestration

When `pipeline.enabled = true`, Polyphony breaks each issue into a sequence of tasks instead
of dispatching a single agent. This creates a Movement that tracks overall progress and
individual Task records that execute sequentially.

### Planner-Driven Pipeline

Set `pipeline.planner_agent` to the name of an agent profile that will analyze each issue
and write a structured plan:

```yaml
pipeline:
  enabled: true
  planner_agent: planner
  replan_on_failure: true
```

The planner agent receives the issue and writes `.polyphony/plan.json` to the workspace:

```json
{
  "tasks": [
    {
      "title": "Research existing auth patterns",
      "category": "research",
      "description": "Investigate current auth middleware and session management",
      "agent": "researcher"
    },
    {
      "title": "Implement OAuth2 flow",
      "category": "coding",
      "description": "Add OAuth2 login, callback, and token refresh endpoints"
    },
    {
      "title": "Write integration tests",
      "category": "testing",
      "description": "Cover login, token refresh, and error scenarios"
    }
  ]
}
```

Valid categories are `research`, `coding`, `testing`, `documentation`, and `review`.
The `agent` field is optional — when omitted, the orchestrator falls back to the
stage config or the default agent.

When `replan_on_failure` is true and a task fails, the planner agent is re-invoked
with error context to produce a revised plan.

### Static Pipeline

Define fixed stages that apply to every issue:

```yaml
pipeline:
  enabled: true
  stages:
    - category: research
      agent: researcher
      max_turns: 4
    - category: coding
      agent: coder
      max_turns: 10
    - category: review
      agent: reviewer
      max_turns: 4
```

Each stage becomes a task. Tasks execute in the order listed.

### Workspace Artifacts

Pipeline tasks share the same workspace directory and communicate through files:

| Artifact | Purpose |
|---|---|
| `.polyphony/plan.json` | Structured plan from the planner agent |
| `.polyphony/workpad.md` | Free-form notes any agent can read and extend |
| `.polyphony/review.md` | Review output from the review pass |

Each task's prompt automatically includes:

- The original issue data
- The full plan (when a planner was used)
- Summaries of completed tasks
- The current task title and description

### Pipeline Configuration Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `pipeline.enabled` | bool | `false` | Enable pipeline dispatch |
| `pipeline.planner_agent` | string | — | Agent profile that generates `plan.json` |
| `pipeline.planner_prompt` | string | — | Custom Liquid template for the planner (uses built-in default) |
| `pipeline.stages` | array | `[]` | Static stage definitions (used when no planner) |
| `pipeline.replan_on_failure` | bool | `false` | Re-run planner if a task fails |
| `pipeline.validation_agent` | string | — | Agent that validates after all tasks |

Stage fields:

| Field | Type | Description |
|---|---|---|
| `stages[].category` | string | Task category: `research`, `coding`, `testing`, `documentation`, `review` |
| `stages[].agent` | string | Agent profile name (falls back to `agents.default`) |
| `stages[].prompt` | string | Custom Liquid prompt template for this stage |
| `stages[].max_turns` | int | Override `agent.max_turns` for this stage |

### Pipeline Examples

- [`templates/examples/WORKFLOW.pipeline-planner.md`](../../templates/examples/WORKFLOW.pipeline-planner.md) —
  planner-driven pipeline with research, coding, and review agents
- [`templates/examples/WORKFLOW.pipeline-static.md`](../../templates/examples/WORKFLOW.pipeline-static.md) —
  static three-stage pipeline without a planner

## Prompt Rendering

The Markdown body of `WORKFLOW.md` is treated as a template. At runtime, the workflow crate renders
prompt text with issue and execution context before handing control to the selected agent.

The template has access to issue data and the current attempt value. `attempt` is `nil` on the
first run, and an integer on retry or continuation runs. Turn rendering also exposes
`turn_number`, `max_turns`, and `is_continuation`, which are especially useful for
`agent.continuation_prompt`. Unknown variables and unknown filters fail rendering instead of
silently producing empty output. The parsed workflow is then normalized into `AgentDefinition`
values that the rest of the runtime consumes.
