# factoryrs

`factoryrs` is a Rust workspace that implements the skeleton of a Symphony-style orchestration service:
it loads a repo-owned `WORKFLOW.md`, polls a tracker, assigns isolated workspaces, runs agent workers,
and renders live runtime state in a `ratatui` dashboard.

The current codebase is intentionally split into subcrates so the orchestration loop stays independent
from tracker, agent, persistence, and UI implementations.

## Workspace layout

- `crates/factoryrs-core`: domain model and trait contracts.
- `crates/factoryrs-workflow`: `WORKFLOW.md` loader, typed config, prompt rendering.
- `crates/factoryrs-orchestrator`: async orchestrator, retry loop, reconciliation, workspace hooks.
- `crates/factoryrs-workspace`: workspace manager for path safety, lifecycle hooks, rollback, and cleanup.
- `crates/factoryrs-issue-mock`: feature-gated mock tracker and mock agent runtime for tests and local smoke runs.
- `crates/factoryrs-linear`: Linear tracker adapter using typed GraphQL queries.
- `crates/factoryrs-git`: git-backed workspace provisioning for linked worktrees and discrete clones.
- `crates/factoryrs-github`: GitHub Issues + PR integration using `octocrab` and `graphql_client`.
- `crates/factoryrs-sqlite`: optional SQLite-backed state store.
- `crates/factoryrs-tui`: `ratatui` status surface.
- `crates/factoryrs-cli`: thin binary that wires the build together.

## Feature flags

- `mock`: demo tracker and demo agent runtime. Enabled by default.
- `linear`: Linear tracker adapter from `factoryrs-linear`.
- `github`: GitHub Issues tracker adapter.
- `sqlite`: SQLite-backed persistence adapter.

## Current status

This repository already provides:

- a global Cargo workspace with crate-local `Error` enums built with `thiserror`
- trait seams for trackers, app-server runtimes, and persistence
- a dedicated workspace provisioner seam, with `git2`-backed linked worktree and clone support
- a separate workspace manager crate that handles sanitized path mapping, containment checks, hook execution, transient artifact cleanup, and rollback on failed initialization
- a real config layer based on the `config` crate, with defaults, env overlays, and typed deserialization from `WORKFLOW.md`
- a long-running async orchestrator with retries, reconciliation, workspace hooks, restart bootstrap, and live snapshots
- runtime throttling when adapters surface `429`-style rate limits
- budget and spend snapshots that can be persisted and shown in the TUI
- a `ratatui` dashboard for running work, retry queue, token totals, throttles, budgets, and recent events
- an implementation-owned `tracker.kind: mock` extension so the system can be run locally without Linear

Git and GitHub implementation choices:

- `git2` is used for automatic linked worktree and discrete clone lifecycle, following the same direction Arbor uses.
- `octocrab` is used for GitHub Issues reads.
- `graphql_client` is used for Linear queries, GitHub PR comment mutations, and GitHub Project workflow sync.
- GitHub Issues can also be auto-linked into a canonical GitHub Project and have a project `Status` field updated best-effort when `tracker.project_owner` and `tracker.project_number` are configured.
- GraphQL schemas are checked in and can be refreshed with `just schema-github` and `just schema-linear`.

This repository does not yet provide production app-server clients for Codex, Copilot, or Claude. The runtime,
provider config, throttling model, and trait boundaries are in place for them, but the current runnable path is
the mock/demo flow.

## Workspace strategies

`workspace.checkout_kind` supports:

- `directory`: just create and reuse a plain directory.
- `linked_worktree`: create a git linked worktree from `workspace.source_repo_path`.
- `discrete_clone`: clone from `workspace.clone_url` or `workspace.source_repo_path`.

Related workspace controls:

- `workspace.sync_on_reuse`: when `true`, reused git workspaces are re-checked out to the target branch and clones fetch `origin` before reuse.
- `workspace.transient_paths`: paths to delete inside a workspace before runs, defaulting to `tmp` and `.elixir_ls`.

Branch names default to the issue branch metadata when present, otherwise `task/<sanitized-issue-id>`.

## Run

```bash
cargo run -p factoryrs-cli
```

The default `WORKFLOW.md` uses the mock tracker, so the TUI starts immediately and shows seeded demo issues.

Run without the TUI:

```bash
cargo run -p factoryrs-cli -- --no-tui
```

Enable SQLite persistence:

```bash
cargo run -p factoryrs-cli --features sqlite -- --sqlite-url sqlite://factoryrs.db
```

Build everything, including optional adapters:

```bash
cargo check --workspace --all-features
```

Refresh checked-in GraphQL schemas:

```bash
just schema-github
just schema-linear
```

`schema-linear` requires `LINEAR_API_KEY` in the environment and refreshes
`crates/factoryrs-linear/src/linear_schema.json` from the live Linear endpoint.

## Next steps

- Replace the demo agent runtime with a real Codex app-server protocol client.
- Add real provider runtimes for Codex, Copilot, and Claude behind feature flags.
- Finish the Linear and GitHub normalization layers, especially blockers and pagination.
- Expand SQLite recovery to restore richer in-flight metadata beyond retries, throttles, and budgets.
- Add an HTTP snapshot API on top of the runtime snapshot model.
