# polyphony-agent-codex

`polyphony-agent-codex` is a repository-local provider runtime crate and is not currently a member
of the root Cargo workspace.

## Responsibility

It implements a Codex-focused `AgentProviderRuntime` that speaks the app-server JSON protocol,
including:

- initialization
- thread creation
- turn creation
- event forwarding
- budget and model discovery helpers

## Relationship to the main runtime

Today the shipping CLI still uses `polyphony-agents` for app-server transport. This crate is the
more specialized version of that same direction.
