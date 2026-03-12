---
tracker:
  kind: mock
  project_owner: null
  project_number: null
  project_status_field: Status
  active_states:
    - Todo
    - In Progress
  terminal_states:
    - Human Review
    - Done
    - Closed
    - Cancelled
polling:
  interval_ms: 2000
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
  sync_on_reuse: true
  transient_paths:
    - tmp
    - .elixir_ls
  source_repo_path: null
  clone_url: null
  default_branch: main
agent:
  max_concurrent_agents: 3
  max_turns: 4
  max_retry_backoff_ms: 60000
agents:
  default: mock
  by_state: {}
  by_label: {}
  profiles:
    mock:
      kind: mock
      transport: mock
      turn_timeout_ms: 120000
      read_timeout_ms: 5000
      stall_timeout_ms: 120000
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      fallbacks:
        - kimi_fast
      fetch_models: true
      models_command: codex models --json
      turn_timeout_ms: 3600000
      read_timeout_ms: 5000
      stall_timeout_ms: 300000
      credits_command: null
      spending_command: null
    claude:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
      fetch_models: true
      models_command: claude models --json
      turn_timeout_ms: 3600000
      read_timeout_ms: 5000
      stall_timeout_ms: 300000
    kimi_fast:
      kind: kimi
      api_key: $KIMI_API_KEY
      model: kimi-2.5
      fetch_models: true
      turn_timeout_ms: 3600000
      read_timeout_ms: 5000
      stall_timeout_ms: 300000
    openai:
      kind: openai
      transport: openai_chat
      fetch_models: true
      model: gpt-4.1
      turn_timeout_ms: 3600000
      read_timeout_ms: 5000
      stall_timeout_ms: 300000
server:
  port: 0
---
# Symphony-style Worker Prompt

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}
Attempt: {{ attempt }}

Execution rules:

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Leave the issue in a non-active handoff state when work is complete.
- Prefer tests, logs, and explicit status over hidden work.

Git-backed workspace examples:

- `checkout_kind: linked_worktree` with `source_repo_path: /abs/path/to/repo`
- `checkout_kind: discrete_clone` with `clone_url: git@github.com:owner/repo.git`
- `sync_on_reuse: false` to preserve an existing checkout without re-checking out the target branch

GitHub Project parity example:

- `tracker.kind: github`
- `tracker.repository: owner/repo`
- `tracker.project_owner: owner-or-org`
- `tracker.project_number: 7`
- `tracker.project_status_field: Status`

When configured, `polyphony` will add dispatched GitHub issues to that Project and best-effort sync the
project status field for workflow visibility.

Multi-agent examples:

- `agents.default: codex` for a generic app-server-backed Codex session
- `agents.by_state.todo: claude` to route `Todo` work to a local Claude CLI profile
- `agents.by_label.risky: claude` to override by label
- `agents.profiles.<name>.fallbacks` to rotate to another agent profile after failures or throttles while preserving saved context
- `agents.profiles.<name>.transport: local_cli` with `use_tmux: true` to drive local membership-backed CLIs through tmux
- `agents.profiles.<name>.interaction_mode: interactive` to inject prompts over stdin or tmux paste instead of requiring a one-shot wrapper command
- `agents.profiles.<name>.transport: openai_chat` to call an OpenAI-compatible `/chat/completions` endpoint directly
- `agents.profiles.<name>.fetch_models: true` to auto-discover model catalogs
- `agents.profiles.<name>.models_command` for CLI/app-server wrappers that can print a JSON or line-based model list
- `kind: kimi` / `kind: moonshotai` to target Kimi 2.5 through Moonshot's OpenAI-compatible endpoint

Structured handoff environment for CLI/app-server commands:

- `POLYPHONY_CONTEXT_FILE`
- `POLYPHONY_CONTEXT_JSON`
- `POLYPHONY_PRIOR_AGENT`
