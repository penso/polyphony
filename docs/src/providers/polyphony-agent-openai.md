# polyphony-agent-openai

`polyphony-agent-openai` is a root workspace member and the HTTP runtime for OpenAI-compatible
providers.

## Responsibility

It implements `AgentProviderRuntime` for `transport: openai_chat` profiles and supports:

- `/models` discovery, or `models_command` when configured
- streamed `/chat/completions` requests
- usage and rate-limit extraction
- a basic tool-call loop that rejects unsupported tool requests and continues the turn

## Relationship to the registry

`polyphony-agents` registers this runtime behind the `openai` feature.
