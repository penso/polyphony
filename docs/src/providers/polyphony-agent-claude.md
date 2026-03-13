# polyphony-agent-claude

`polyphony-agent-claude` is a root workspace member and a thin wrapper around
`polyphony-agent-local`.

## Responsibility

It implements a Claude-focused `AgentProviderRuntime` that:

- matches `kind: claude` and `kind: anthropic`
- delegates execution and budget handling to the local CLI runtime
- defaults model discovery to `claude models --json` when `fetch_models: true`

## Relationship to the registry

`polyphony-agents` registers this runtime behind the `claude` feature.
