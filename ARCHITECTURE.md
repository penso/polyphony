# Architecture

This workspace maps the Symphony specification into seven layers.

## Policy

`WORKFLOW.md` stays in the repository and acts as the system of record for prompt text, polling cadence,
workspace hooks, concurrency, and tracker selection.

## Configuration

`polyphony-workflow` parses YAML front matter plus the Markdown body into a typed `ServiceConfig` and
`WorkflowDefinition`. The crate now uses the `config` crate to apply defaults, `POLYPHONY__...` env overlays,
typed deserialization, `$VAR` secret indirection, path expansion, and prompt rendering.

## Coordination

`polyphony-orchestrator` owns all mutable scheduling state. Running sessions, claims, retry timers, runtime totals,
and recent events live in one async orchestrator loop and are surfaced through a snapshot channel.

## Execution

`polyphony-workspace` owns `WorkspaceManager`, which enforces workspace-root containment, sanitized
directory names, hook execution, transient artifact cleanup, configurable reuse behavior, and
rollback on failed initialization.
`polyphony-agents` now provides the registry runtime, and delegates to provider crates:
- `polyphony-agent-codex`
- `polyphony-agent-claude`
- `polyphony-agent-copilot`
- `polyphony-agent-openai`
- `polyphony-agent-local` as the local CLI fallback
Those runtimes cover app-server over stdio, local CLI/tmux automation, and OpenAI-compatible chat HTTP.
Automatic model discovery now lives at the provider layer:
- `/models` probing for OpenAI-compatible providers
- `models_command` probing for CLI/app-server-backed agents
The orchestrator also keeps a saved per-issue context snapshot from streamed agent events and hands
that state forward into retries or agent fallbacks, so provider switches are prompt- and env-aware
instead of starting blind.
`AgentRuntime` remains the orchestrator-facing trait boundary, while `AgentProviderRuntime`
is the provider plug-in seam used by the registry.
`WorkspaceProvisioner` is a separate trait so the scheduler can choose between plain directories,
linked git worktrees, and discrete clones without entangling git lifecycle with orchestrator state.

## Integration

`IssueTracker` is the tracker seam. `polyphony-issue-mock` currently ships the demo `MockTracker`.
`polyphony-linear` provides the feature-gated Linear implementation. `polyphony-github` provides the GitHub Issues implementation.
The mock path is what makes the TUI runnable now.
`polyphony-github` owns GitHub-specific integrations built on `octocrab` and `graphql_client`,
including PR comment mutations and best-effort GitHub Project workflow syncing.
Both the Linear and GitHub GraphQL integrations use checked-in schemas plus checked-in `.graphql`
operation files so query shapes stay compile-checked.

## Persistence

The spec does not require a database, but the workspace exposes `StateStore` and an optional SQLite adapter.
That allows durable snapshots, retries, throttles, run records, and budget samples without coupling SQLite
into the core runtime.

## Observability

The `ratatui` surface renders only orchestrator snapshots. It is intentionally a consumer, not a controller,
except for a best-effort manual refresh command. The snapshot model now includes budget and throttle state so
operators can see 429 backoff and remaining credits/spend in one place.

## What remains

- richer Linear normalization for blockers, pagination, and state refresh details
- restart recovery from SQLite
- optional HTTP dashboard/API layer
