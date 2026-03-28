# Workspace Crates

The root Cargo workspace currently contains fourteen crates. Each one exists to keep orchestration,
workflow parsing, workspace lifecycle, integrations, and presentation separated by clear trait
boundaries.

## Always-on crates

- `polyphony-core`: shared domain types, runtime snapshots, and trait contracts
- `polyphony-workflow`: `WORKFLOW.md` parsing, defaults, validation, and prompt rendering
- `polyphony-agents`: provider registry runtime used by the CLI today
- `polyphony-orchestrator`: long-running scheduler and reconciliation loop
- `polyphony-workspace`: workspace manager and hook executor
- `polyphony-git`: git-backed workspace provisioner
- `polyphony-feedback`: outbound human-feedback registry and sink implementations
- `polyphony-tui`: terminal dashboard for runtime snapshots
- `polyphony-httpd`: web UI with SSR templates, GraphQL API, and WebSocket subscriptions
- `polyphony-cli`: executable entrypoint that assembles the runtime
- `polyphony-tools`: built-in LLM tool registry and workflow policy filtering

## Feature-gated crates

- `polyphony-issue-mock`: seeded mock tracker and mock runtime for tests and internal smoke coverage
- `polyphony-linear`: Linear tracker integration
- `polyphony-github`: GitHub Issues, pull request comments, and GitHub Project syncing
- `polyphony-sqlite`: optional SQLite-backed persistence

## How to read this section

The nested chapters document each crate’s responsibility, the important types or entrypoints it
owns, and how it participates in the overall runtime.
