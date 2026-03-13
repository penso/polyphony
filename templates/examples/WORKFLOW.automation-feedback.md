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
    - .elixir_ls
agent:
  max_concurrent_agents: 1
  max_turns: 4
  max_retry_backoff_ms: 60000
agents:
  default: codex
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      approval_policy: auto
      thread_sandbox: workspace-write
      turn_sandbox_policy: workspace-write
automation:
  enabled: true
  draft_pull_requests: true
  review_agent: codex
  commit_message: "fix({{ issue.identifier }}): {{ issue.title }}"
feedback:
  offered:
    - telegram
    - webhook
  telegram:
    ops:
      bot_token: $TELEGRAM_BOT_TOKEN
      chat_id: "123456789"
  webhook:
    audit:
      url: https://example.com/polyphony/handoff
      bearer_token: $HANDOFF_WEBHOOK_TOKEN
---
# Automation And Feedback Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

Execution rules:

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave the issue in a non-active handoff state when work is complete.
