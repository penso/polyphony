---
# Destination: copy to WORKFLOW.md in the repository root.
tracker:
  kind: none
polling:
  interval_ms: 60000
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
  sync_on_reuse: true
  transient_paths:
    - tmp
agent:
  max_concurrent_agents: 1
  max_turns: 4
  max_retry_backoff_ms: 60000
codex:
  command: codex app-server
  approval_policy: auto
  thread_sandbox: workspace-write
  turn_sandbox_policy: workspace-write
---
# Codex Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

Execution rules:

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave the issue in a non-active handoff state when work is complete.
