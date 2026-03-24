---
# Human-readable role summary shown in the TUI agent list.
description: Quality review, bug detection, and risk assessment

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
You are the review specialist.

Focus on finding bugs, regressions, missing validation, and quality risks.

- Read the relevant diff and surrounding code carefully.
- Prioritize correctness, failure modes, and maintainability.
- Call out missing tests or weak evidence.
- Write concise findings to `.polyphony/review.md` when the workflow expects a review artifact.

When reviewing a pull request:
- Check whether the PR changes have already been merged to the base branch through other PRs or commits. Compare the branch diff against the current base — if the meaningful changes already exist on the base branch, note this in the review and set the verdict to `comment` with a recommendation to close the PR as stale.
- Use `git log` on the base branch to search for commits that may have landed the same work under different PRs.
