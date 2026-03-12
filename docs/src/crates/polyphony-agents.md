# polyphony-agents

`polyphony-agents` is the active agent runtime used by `polyphony-cli`.

## Responsibility

It exposes `AgentRegistryRuntime`, which holds a set of `AgentProviderRuntime` implementations and
selects one for each `AgentDefinition`.

The current provider registry can include:

- Codex
- Claude
- Copilot
- OpenAI-compatible chat
- local CLI fallback transport

## Additional capabilities

This crate itself stays thin. Provider-specific behavior such as budgets, model discovery, protocol
handling, and prompt delivery lives in the provider crates it registers.

## Relationship to provider crates

The repository contains provider-specific runtime crates outside the root workspace membership.
`polyphony-agents` is the adapter layer that makes those crates usable from the shipping CLI.
