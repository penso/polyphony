# polyphony-agent-local

`polyphony-agent-local` is a repository-local provider runtime crate and is not currently in the
root Cargo workspace.

## Responsibility

It implements `AgentProviderRuntime` for local CLI tools and supports:

- one-shot stdio execution
- tmux-backed sessions
- interactive prompt delivery modes
- model discovery and budget probes through `polyphony-agent-common`

## Architectural meaning

This crate shows the provider-specific direction of the codebase: use one runtime per provider or
provider family instead of keeping all transport logic in a single generic crate.
