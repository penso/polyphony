# Architecture

The workspace is organized around a small set of stable boundaries.

## Policy and Configuration

Runtime configuration is layered. `polyphony-workflow` merges:

- built-in defaults
- `~/.config/polyphony/config.toml`
- repository-owned `WORKFLOW.md` front matter
- repo-local `.polyphony/config.toml`
- `POLYPHONY__...` environment overrides

That merged config becomes:

- typed runtime configuration
- workflow prompt text
- agent routing rules
- tracker and workspace settings

The workflow loader also supports defaults, environment overlays, and template rendering.

## Coordination

`polyphony-orchestrator` owns mutable runtime state. It is responsible for:

- polling trackers
- claiming and scheduling work
- retry and backoff handling
- budget and throttle bookkeeping
- pipeline dispatch and multi-task sequencing
- publishing snapshots for consumers such as the TUI

## Execution

Two distinct concerns are separated here:

- `polyphony-workspace` manages directory lifecycle, safety checks, hooks, cleanup, and reuse
- `polyphony-agents` runs agent transports such as app-server stdio, local CLI automation, and OpenAI-compatible chat requests

The CLI wires these pieces together in `polyphony-cli`.

## Integration

Trackers are abstracted behind `IssueTracker`, with current implementations for:

- mock local runs
- Linear
- GitHub Issues and related GitHub Project synchronization

Workspace provisioning is abstracted behind `WorkspaceProvisioner`, allowing directory-only,
linked worktree, and discrete clone strategies.

Post-run handoff is split across three seams:

- `WorkspaceCommitter` for git commit and push behavior
- `PullRequestManager` and `PullRequestCommenter` for GitHub PR automation
- `FeedbackSink` for outbound human notifications

## Persistence and Observability

Persistence is optional. `polyphony-sqlite` can store runtime state without coupling SQLite into
the rest of the architecture.

Observability is snapshot-driven. The TUI reads orchestrator snapshots and renders runtime state
without owning business logic, while feedback sinks can fan out review-ready handoff notifications
to external channels.
