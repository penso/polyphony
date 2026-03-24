---
# Destination: copy to WORKFLOW.md in the repository root.
tracker:
  kind: none
  # Keep shared repository policy here.
  # Keep local tracker identity in `polyphony.toml` when you do not want to edit
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
  interval_ms: 60000
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
  sync_on_reuse: true
  transient_paths:
    - tmp
  # Keep shared workspace policy here.
  # Put local repo wiring such as `source_repo_path` in `polyphony.toml`
  # when the checked-in workflow is a shared template.
  # checkout_kind: linked_worktree
  # source_repo_path: /abs/path/to/this/repo
  # Linked worktrees fetch through libgit2 first and fall back to system git on SSH auth failures.
  # Hardware-backed SSH keys may behave more reliably with discrete_clone or an HTTPS clone_url.
  # default_branch: main
  # checkout_kind: discrete_clone
  # clone_url: git@github.com:owner/repo.git
agent:
  max_concurrent_agents: 3
  max_turns: 4
  max_retry_backoff_ms: 60000
tools:
  enabled: false
  # allow:
  #   - workspace_list_files
  #   - workspace_read_file
  #   - workspace_search
  #   - issue_update
  #   - issue_comment
  #   - linear_graphql
  # by_agent:
  #   reviewer:
  #     allow:
  #       - workspace_read_file
  #       - workspace_search
  #       - pr_comment
  #       - linear_graphql
orchestration:
  router_agent: router
  mode: advisory
pipeline:
  enabled: true
agents:
  default: implementer
  reviewer: reviewer
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
- Let the router split complex work into specialist tasks when that helps quality or speed.

Shared workflow policy belongs in this file.
Shared credentials and reusable agent profiles belong in `~/.config/polyphony/config.toml`.
Local tracker identity, router selection, and repo wiring can live in `polyphony.toml`.
Role-specific prompts live in `.polyphony/agents/`.
