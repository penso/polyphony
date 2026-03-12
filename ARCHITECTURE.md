# Architecture

This workspace maps the Symphony specification into seven layers.

## Policy

`WORKFLOW.md` stays in the repository and acts as the system of record for prompt text, polling cadence,
workspace hooks, concurrency, and tracker selection.

## Configuration

`factoryrs-workflow` parses YAML front matter plus the Markdown body into a typed `ServiceConfig` and
`WorkflowDefinition`. The crate now uses the `config` crate to apply defaults, `FACTORYRS__...` env overlays,
typed deserialization, `$VAR` secret indirection, path expansion, and prompt rendering.

## Coordination

`factoryrs-orchestrator` owns all mutable scheduling state. Running sessions, claims, retry timers, runtime totals,
and recent events live in one async orchestrator loop and are surfaced through a snapshot channel.

## Execution

`WorkspaceManager` enforces workspace-root containment, sanitized directory names, and hook execution.
`AgentRuntime` is a trait so the worker lifecycle can host different app-server integrations later
without changing orchestrator logic.
`WorkspaceProvisioner` is a separate trait so the scheduler can choose between plain directories,
linked git worktrees, and discrete clones without entangling git lifecycle with orchestrator state.

## Integration

`IssueTracker` is the tracker seam. `factoryrs-issue-mock` currently ships the demo `MockTracker`.
`factoryrs-linear` provides the feature-gated Linear implementation. `factoryrs-github` provides the GitHub Issues implementation.
The mock path is what makes the TUI runnable now.
`factoryrs-github` owns GitHub-specific integrations built on `octocrab` and `graphql_client`,
including PR comment mutations.

## Persistence

The spec does not require a database, but the workspace exposes `StateStore` and an optional SQLite adapter.
That allows durable snapshots, retries, throttles, run records, and budget samples without coupling SQLite
into the core runtime.

## Observability

The `ratatui` surface renders only orchestrator snapshots. It is intentionally a consumer, not a controller,
except for a best-effort manual refresh command. The snapshot model now includes budget and throttle state so
operators can see 429 backoff and remaining credits/spend in one place.

## What remains

- Codex app-server handshake and streaming protocol integration
- richer Linear normalization for blockers, pagination, and state refresh details
- restart recovery from SQLite
- optional HTTP dashboard/API layer
