# Workflow Configuration

`polyphony` starts from a repository-owned `WORKFLOW.md` file. The file contains YAML front matter
followed by the worker prompt template.

## Shape

The current workspace configuration covers:

- `tracker`: tracker kind, repository or project identifiers, API settings, and state mapping
- `polling`: tracker polling interval
- `workspace`: root path, checkout strategy, reuse behavior, and transient cleanup paths
- `hooks`: optional shell hooks around workspace lifecycle events
- `agent`: global concurrency, turn, and retry limits
- `codex`: optional single-agent shorthand for one Codex app-server profile
- `agents`: named agent profiles and routing rules
- `automation`: optional post-run git and PR handoff settings
- `feedback`: outbound notification sink configuration
- `server`: optional server settings

## Example

```yaml
---
tracker:
  kind: mock
polling:
  interval_ms: 2000
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
automation:
  enabled: false
feedback:
  offered: []
agent:
  max_concurrent_agents: 3
agents:
  default: mock
  profiles:
    mock:
      kind: mock
      transport: mock
---
# Worker Prompt
```

## Single-Agent Shorthand

For a simple single-agent workflow, `codex` can stand in for a one-profile `agents` section:

```yaml
---
tracker:
  kind: mock
codex:
  command: codex app-server
  approval_policy: auto
---
# Worker Prompt
```

That shorthand is normalized into a default `codex` agent internally. The legacy top-level
`provider` block is still accepted as a deprecated alias for the same single-agent mode.

## Agent Routing

The `agents` section supports:

- `default`: fallback profile name
- `by_state`: profile overrides keyed by issue state
- `by_label`: profile overrides keyed by issue label
- `profiles`: named transport definitions

Current transport styles in the codebase are:

- `mock`
- `app_server`
- `local_cli`
- `openai_chat`

Each agent profile can also control:

- `model`, `models`, and `models_command` for single-model or discovered-model setups
- `fetch_models` to enable automatic model discovery
- `approval_policy`, `thread_sandbox`, and `turn_sandbox_policy` for app-server-backed agents
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

## Prompt Rendering

The Markdown body of `WORKFLOW.md` is treated as a template. At runtime, the workflow crate renders
prompt text with issue and execution context before handing control to the selected agent.

The template has access to issue data and the current attempt value. `attempt` is `nil` on the
first run, and an integer on retry or continuation runs. Unknown variables and unknown filters fail
rendering instead of silently producing empty output. The parsed workflow is then normalized into
`AgentDefinition` values that the rest of the runtime consumes.
