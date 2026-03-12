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
  root: .factoryrs/workspaces
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
provider:
  kind: mock
  command: codex app-server
  stall_timeout_ms: 120000
  credits_command: null
  spending_command: null
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

When configured, `factoryrs` will add dispatched GitHub issues to that Project and best-effort sync the
project status field for workflow visibility.
