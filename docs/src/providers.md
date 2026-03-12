# Provider Crates

This repository contains a second group of crate directories that are not currently members of the
root Cargo workspace.

## Why they exist

These directories implement or reserve a more provider-specific architecture built around
`polyphony_core::AgentProviderRuntime`. They are not root workspace members, but some of them are
still compiled through path dependencies from `polyphony-agents`.

## Current state

- `polyphony-agent-common`, `polyphony-agent-local`, and `polyphony-agent-codex` contain real code
- `polyphony-agent-claude`, `polyphony-agent-copilot`, `polyphony-agent-openai`, and
  `factoryrs-runtime` currently exist only as empty directories

The nested chapters describe what is implemented today and what is only reserved for future work.
