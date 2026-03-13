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
reusable agent profiles, and personal defaults. Treat `WORKFLOW.md` as the shared repo-owned
workflow policy and prompt. Use `.polyphony/config.toml` for local repository wiring such as
`tracker.repository`, `tracker.project_slug`, `workspace.source_repo_path`, and `workspace.clone_url`
when you do not want to edit the checked-in workflow.

The current workspace configuration covers:

- `tracker`: tracker kind, repository or project identifiers, API settings, and state mapping
- `polling`: tracker polling interval
- `workspace`: root path, checkout strategy, reuse behavior, and transient cleanup paths
- `hooks`: optional shell hooks around workspace lifecycle events, with captured stdout/stderr logged in truncated form
- `agent`: global concurrency, turn, and retry limits
- `codex`: optional single-agent shorthand for one Codex app-server profile
- `agents`: named agent profiles and routing rules
- `automation`: optional post-run git and PR handoff settings
- `feedback`: outbound notification sink configuration
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

Current transport styles in the codebase are:

- `app_server`
- `local_cli`
- `openai_chat`

Each agent profile can also control:

- `model`, `models`, and `models_command` for single-model or discovered-model setups
- `fetch_models` to enable automatic model discovery
- `api_key` for `openai_chat` profiles when they are actually used, while missing keys only disable `/models` discovery during startup
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

## Prompt Rendering

The Markdown body of `WORKFLOW.md` is treated as a template. At runtime, the workflow crate renders
prompt text with issue and execution context before handing control to the selected agent.

The template has access to issue data and the current attempt value. `attempt` is `nil` on the
first run, and an integer on retry or continuation runs. Turn rendering also exposes
`turn_number`, `max_turns`, and `is_continuation`, which are especially useful for
`agent.continuation_prompt`. Unknown variables and unknown filters fail rendering instead of
silently producing empty output. The parsed workflow is then normalized into `AgentDefinition`
values that the rest of the runtime consumes.
