# polyphony-agent-codex

`polyphony-agent-codex` is a root workspace member and the dedicated Codex app-server runtime.

## Responsibility

It implements a Codex-focused `AgentProviderRuntime` that speaks the app-server JSON protocol,
including:

- initialization
- thread creation
- live session startup
- repeated turn creation on the same thread
- approval and unsupported-tool auto-responses
- event forwarding
- usage and rate-limit extraction
- budget and model discovery helpers

When the orchestrator chooses to continue work after a successful turn, the Codex runtime keeps the
same app-server process and `threadId` alive and issues another `turn/start` instead of starting a
fresh session.

## Relationship to the main runtime

`polyphony-agents` registers this runtime behind the `codex` feature, and `polyphony-cli` reaches
it through `AgentRegistryRuntime`.
