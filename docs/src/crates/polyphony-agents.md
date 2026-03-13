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

The provider-specific runtime crates are root workspace members compiled behind feature flags such
as `codex`, `claude`, `copilot`, `openai`, and `local`. `polyphony-agents` is the integration layer
that wires them together for the shipping CLI.
