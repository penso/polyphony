# polyphony-agent-codex

`polyphony-agent-codex` is a root workspace member and the dedicated Codex app-server runtime.

## Responsibility

It implements a Codex-focused `AgentProviderRuntime` that speaks the app-server JSON protocol,
including:

- initialization
- thread creation
- live session startup
- repeated turn creation on the same thread
- approval auto-responses and optional built-in tool execution
- event forwarding
- usage and rate-limit extraction, including absolute thread totals and `total_token_usage` wrapper payloads
- budget and model discovery helpers

When built-in tools are enabled in workflow config, the runtime advertises the allowed tool specs
on `thread/start` and executes supported `item/tool/call` requests through the shared tool
executor.

When the orchestrator chooses to continue work after a successful turn, the Codex runtime keeps the
same app-server process and `threadId` alive and issues another `turn/start` instead of starting a
fresh session. Those continuation turns can carry a workflow-configured `agent.continuation_prompt`
instead of a hardcoded follow-up message.

The runtime now emits structured live-session metadata on its upstream events so the orchestrator
can surface and persist the active `session_id`, `thread_id`, `turn_id`, and `codex_app_server_pid`
for debugging.

For token accounting, it prefers absolute totals and intentionally ignores delta-only payloads such
as `last_token_usage` so runtime aggregates do not drift upward from double-counting.

Handshake and stream failures are normalized into stable adapter categories such as
`response_timeout`, `turn_timeout`, `codex_not_found`, `port_exit`, and `response_error` so the
orchestrator can preserve more accurate retry and status behavior.

## Relationship to the main runtime

`polyphony-agents` registers this runtime behind the `codex` feature, and `polyphony-cli` reaches
it through `AgentRegistryRuntime`.
