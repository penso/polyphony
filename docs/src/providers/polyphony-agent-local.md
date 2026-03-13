# polyphony-agent-local

`polyphony-agent-local` is a root workspace member and the base runtime for local CLI providers.

## Responsibility

It implements `AgentProviderRuntime` for local CLI tools and supports:

- one-shot stdio execution
- tmux-backed sessions
- interactive prompt delivery modes
- model discovery and budget probes through `polyphony-agent-common`

## Architectural meaning

This crate handles the shared local-process transport for `transport: local_cli`. Provider wrappers
such as Claude and Copilot build on top of it instead of reimplementing shell and tmux behavior.
