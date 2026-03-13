---
# Destination: copy to WORKFLOW.md in the repository root.
tracker:
  kind: none
polling:
  interval_ms: 2000
workspace:
  root: .polyphony/workspaces
  checkout_kind: directory
  sync_on_reuse: true
  transient_paths:
    - tmp
    - .elixir_ls
agent:
  max_concurrent_agents: 3
  max_turns: 4
  max_retry_backoff_ms: 60000
agents:
  default: codex
  by_state:
    todo: claude
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      fallbacks:
        - kimi_fast
      fetch_models: true
      models_command: codex models --json
    kimi_fast:
      kind: kimi
      transport: openai_chat
      model: kimi-2.5
      api_key: $KIMI_API_KEY
      fetch_models: true
    claude:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
      fetch_models: true
      models_command: claude models --json
    openai:
      kind: openai
      transport: openai_chat
      model: gpt-5.1
      api_key: $OPENAI_API_KEY
      fetch_models: true
---
# Multi-Agent Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}
Attempt: {{ attempt }}

Execution rules:

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave the issue in a non-active handoff state when work is complete.
