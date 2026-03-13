---
# Destination: copy to WORKFLOW.md in the repository root.
tracker:
  kind: none
  # Keep shared repository policy here.
  # Keep local tracker identity in `.polyphony/config.toml` when you do not want to edit
  # the checked-in workflow.
  # GitHub:
  # kind: github
  # repository: owner/repo
  # project_owner: owner-or-org
  # project_number: 7
  # project_status_field: Status
  # Linear:
  # kind: linear
  # project_slug: ENG
polling:
  interval_ms: 2000
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
  sync_on_reuse: true
  transient_paths:
    - tmp
    - .elixir_ls
  # Keep shared workspace policy here.
  # Put local repo wiring such as `source_repo_path` in `.polyphony/config.toml`
  # when the checked-in workflow is a shared template.
  # checkout_kind: linked_worktree
  # source_repo_path: /abs/path/to/this/repo
  # default_branch: main
  # checkout_kind: discrete_clone
  # clone_url: git@github.com:owner/repo.git
agent:
  max_concurrent_agents: 3
  max_turns: 4
  max_retry_backoff_ms: 60000
---
# Polyphony Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

Execution rules:

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave the issue in a non-active handoff state when work is complete.

Shared workflow policy belongs in this file.
Shared credentials and reusable agent profiles belong in `~/.config/polyphony/config.toml`.
Local tracker identity and repo wiring can live in `.polyphony/config.toml`.
