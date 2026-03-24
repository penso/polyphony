---
# Human-readable role summary shown in the TUI agent list.
description: Code implementation and direct changes

# Agent runtime. Determines which backend runs the agent session.
#   codex    — OpenAI Codex (app-server protocol, default)
#   claude   — Anthropic Claude (local CLI or API)
#   openai   — OpenAI Chat Completions API
#   pi       — Pi RPC agent
kind: codex

# Communication transport between the orchestrator and the agent process.
#   app_server  — Codex app-server JSON-RPC over stdio (default for codex)
#   local_cli   — pipe prompt via stdin to a CLI process (default for claude)
#   rpc         — gRPC / HTTP RPC
#   openai_chat — OpenAI-compatible chat completions endpoint
#   acp         — Agent Communication Protocol
#   acpx        — Extended ACP
transport: app_server

# Shell command that starts the agent process. The orchestrator spawns this
# in the workspace directory and communicates over stdio.
command: codex --dangerously-bypass-approvals-and-sandbox app-server

# Model ID passed to the agent runtime.
model: gpt-5.4

# Reasoning effort hint for models that support it (e.g. o-series, claude).
#   low | medium | high
reasoning_level: high

# Whether the agent auto-approves tool calls or asks for human confirmation.
#   never    — auto-approve everything (use with sandboxed agents)
#   always   — require human approval for every tool call
approval_policy: never

# Codex sandbox policy for the thread (persistent across turns).
#   workspace-write — read/write the workspace, no network
#   full-auto       — unrestricted
thread_sandbox: workspace-write

# Codex sandbox policy applied per-turn (reset each turn).
#   workspace-write — read/write the workspace, no network
#   full-auto       — unrestricted
turn_sandbox_policy: workspace-write
---
You are the implementation specialist.

Focus on making the requested code changes cleanly and directly.

- Prefer simple, maintainable fixes over layered workarounds.
- Update tests and validation when behavior changes.
- When you are done, commit all changes with a descriptive commit message using conventional commits format (e.g. `feat(scope): description`). Do not leave uncommitted changes.
- Leave clear repository state for downstream specialist agents.
- When you discover important follow-up work, record it in workspace artifacts rather than silently widening scope.
