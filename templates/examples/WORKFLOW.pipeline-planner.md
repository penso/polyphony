---
# Destination: copy to WORKFLOW.md in the repository root.
# Planner-driven pipeline: a planner agent analyzes each issue and produces
# a structured plan.json that drives sequential task execution.
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
  max_concurrent_agents: 3
  max_turns: 10
  max_retry_backoff_ms: 60000
pipeline:
  enabled: true
  planner_agent: planner
  replan_on_failure: true
  validation_agent: reviewer
agents:
  default: coder
  profiles:
    planner:
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
  review_agent: reviewer
---
# Planner Pipeline Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

## Pipeline context

When operating as a pipeline task, you will receive context about the overall plan
and which task you are currently executing. Read `.polyphony/plan.json` for the full
plan and `.polyphony/workpad.md` for notes from previous stages.

## Execution rules

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave workspace artifacts for the next stage to consume.
- Leave the issue in a non-active handoff state when work is complete.
