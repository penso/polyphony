# polyphony-agent-copilot

`polyphony-agent-copilot` is a root workspace member and a thin wrapper around
`polyphony-agent-local`.

## Responsibility

It implements a Copilot-focused `AgentProviderRuntime` that:

- matches `kind: copilot` and `kind: github-copilot`
- delegates execution, budget handling, and model discovery to the local CLI runtime

## Relationship to the registry

`polyphony-agents` registers this runtime behind the `copilot` feature.
