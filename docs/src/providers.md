# Provider Crates

These crates are root workspace members that implement provider-specific runtimes or shared helpers
for them.

## Current state

- `polyphony-agent-common` provides shared prompt, environment, budget, and model helpers
- `polyphony-agent-acp` implements the stdio ACP transport
- `polyphony-agent-acpx` implements the `acpx` bridge transport
- `polyphony-agent-local` implements local CLI and tmux-backed execution
- `polyphony-agent-pi` implements Pi's native RPC transport
- `polyphony-agent-codex` implements the Codex app-server transport over stdio
- `polyphony-agent-openai` implements OpenAI-compatible HTTP chat, model discovery, and built-in tool execution
- `polyphony-agent-claude` wraps `polyphony-agent-local` for Claude-family CLIs
- `polyphony-agent-copilot` wraps `polyphony-agent-local` for Copilot-family CLIs

## Relationship to the shipping runtime

`polyphony-agents` is the registry crate used by `polyphony-cli`. It conditionally includes these
provider runtimes behind feature flags and selects a matching runtime for each `AgentDefinition`.

In practice, the current transport families are:

- `app_server` for Codex
- `rpc` for Pi
- `local_cli` for local terminal-based CLIs such as Claude and Copilot
- `acp` for stdio ACP agents
- `acpx` for bridge-backed agents routed through the `acpx` CLI
- `openai_chat` for HTTP providers that expose an OpenAI-compatible API

Tool-capable runtimes can also advertise built-in tools from `polyphony-tools` when the workflow
enables them. Today that path is implemented for the Codex app-server runtime and the
OpenAI-compatible runtime.
