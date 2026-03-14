---
# Destination: copy to WORKFLOW.md in the repository root.
# Static pipeline: fixed stages run in order for every issue,
# without a planner agent.
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
    - .elixir_ls
agent:
  max_concurrent_agents: 3
  max_turns: 10
  max_retry_backoff_ms: 60000
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
agents:
  default: coder
  profiles:
    researcher:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
      fetch_models: true
      models_command: claude models --json
    coder:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
      fetch_models: true
      models_command: claude models --json
    reviewer:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
      fetch_models: true
      models_command: claude models --json
automation:
  enabled: true
  draft_pull_requests: true
---
# Static Pipeline Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

## Pipeline context

When operating as a pipeline task, you will receive context about the current stage
and what previous stages have completed. Read `.polyphony/workpad.md` for notes
from previous stages.

## Execution rules

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave workspace artifacts for the next stage to consume.
- Leave the issue in a non-active handoff state when work is complete.
