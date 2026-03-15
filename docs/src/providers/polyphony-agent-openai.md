# polyphony-agent-openai

`polyphony-agent-openai` is a root workspace member and the HTTP runtime for OpenAI-compatible
providers.

## Responsibility

It implements `AgentProviderRuntime` for `transport: openai_chat` profiles and supports:

- `/models` discovery, or `models_command` when configured
- skipping `/models` discovery when no `api_key` is configured yet, so optional profiles can stay dormant until selected
- streamed `/chat/completions` requests
- usage and rate-limit extraction
- a tool-call loop that executes supported built-in tools and rejects unsupported requests

## Relationship to the registry

`polyphony-agents` registers this runtime behind the `openai` feature.
