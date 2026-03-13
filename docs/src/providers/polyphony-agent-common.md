# polyphony-agent-common

`polyphony-agent-common` is a root workspace member and the shared helper crate for provider
runtimes.

## Responsibility

It provides shared utilities for provider-specific runtimes, including:

- prompt file creation
- standardized environment variable injection
- stdout and stderr forwarding into `AgentEvent`
- shell command helpers
- budget parsing and model-list parsing helpers

## Intended role

This crate is the common layer for provider-specific runtimes such as the local CLI, Codex, and
OpenAI-compatible providers found elsewhere in this repository.
