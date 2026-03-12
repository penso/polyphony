# polyphony-agent-common

`polyphony-agent-common` is a repository-local helper crate and is not currently a member of the
root Cargo workspace.

## Responsibility

It provides shared utilities for provider-specific runtimes, including:

- prompt file creation
- standardized environment variable injection
- stdout and stderr forwarding into `AgentEvent`
- shell command helpers
- budget parsing and model-list parsing helpers

## Intended role

This crate is the common layer for provider-specific runtimes such as the local CLI and Codex
providers found elsewhere in this repository.
