# Provider Crates

These crates are root workspace members that implement provider-specific runtimes or shared helpers
for them.

## Current state

- `polyphony-agent-common` provides shared prompt, environment, budget, and model helpers
- `polyphony-agent-local` implements local CLI and tmux-backed execution
- `polyphony-agent-codex` implements the Codex app-server transport over stdio
- `polyphony-agent-openai` implements OpenAI-compatible HTTP chat and model discovery
- `polyphony-agent-claude` wraps `polyphony-agent-local` for Claude-family CLIs
- `polyphony-agent-copilot` wraps `polyphony-agent-local` for Copilot-family CLIs

## Relationship to the shipping runtime

`polyphony-agents` is the registry crate used by `polyphony-cli`. It conditionally includes these
provider runtimes behind feature flags and selects a matching runtime for each `AgentDefinition`.
