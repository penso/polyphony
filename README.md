# polyphony

[![Rust nightly-2025-11-30](https://img.shields.io/badge/rust-nightly--2025--11--30-orange?logo=rust)](justfile)
[![Edition 2024](https://img.shields.io/badge/edition-2024-blue)](Cargo.toml)

`polyphony` is a Rust workspace for repo-native AI agent orchestration:
it loads a repo-owned `WORKFLOW.md`, polls GitHub or Linear, provisions isolated workspaces,
runs provider-specific agent runtimes, and renders live runtime state in a `ratatui` dashboard.

Workspace formatting, linting, and test commands are pinned to `nightly-2025-11-30` in [`justfile`](justfile).
Project documentation lives in [`docs/`](docs) as an `mdBook`.

The current codebase is intentionally split into subcrates so the orchestration loop stays independent
from tracker, agent, persistence, and UI implementations.

## Workspace layout

| Crate | Role | Enabled by |
| --- | --- | --- |
| [`polyphony-core`](crates/core) | Domain model and trait contracts used across the workspace. | Always |
| [`polyphony-workflow`](crates/workflow) | `WORKFLOW.md` loader, typed config, and prompt rendering. | Always |
| [`polyphony-agent-common`](crates/agent-common) | Shared shell/model/budget helpers used by provider runtimes. | Always |
| [`polyphony-agent-local`](crates/agent-local) | Local CLI and tmux execution engine for membership-backed providers. | `agent-local` |
| [`polyphony-agent-codex`](crates/agent-codex) | Codex app-server runtime. | `agent-codex` |
| [`polyphony-agent-openai`](crates/agent-openai) | OpenAI-compatible HTTP runtime with streaming/tool-loop handling. | `agent-openai` |
| [`polyphony-agent-claude`](crates/agent-claude) | Claude provider wrapper on top of the local CLI runtime. | `agent-claude` |
| [`polyphony-agent-copilot`](crates/agent-copilot) | Copilot provider wrapper on top of the local CLI runtime. | `agent-copilot` |
| [`polyphony-agents`](crates/agents) | Agent registry that wires provider-specific runtimes into the workspace build. | Always |
| [`polyphony-orchestrator`](crates/orchestrator) | Async orchestrator loop, retries, reconciliation, and workspace hooks. | Always |
| [`polyphony-workspace`](crates/workspace) | Workspace manager for path safety, lifecycle hooks, rollback, and cleanup. | Always |
| [`polyphony-git`](crates/git) | Git-backed workspace provisioning for linked worktrees and discrete clones. | Always |
| [`polyphony-feedback`](crates/feedback) | Generic outbound feedback sinks such as Telegram and webhooks. | Always |
| [`polyphony-tui`](crates/tui) | `ratatui` status surface for live runtime snapshots. | Always |
| [`polyphony-cli`](crates/cli) | Thin binary that wires the build together. | Always |
| [`polyphony-issue-mock`](crates/issue-mock) | Mock tracker and mock agent runtime for tests and internal smoke coverage. | `mock` |
| [`polyphony-linear`](crates/linear) | Linear tracker adapter using typed GraphQL queries. | `linear` |
| [`polyphony-github`](crates/github) | GitHub Issues, PR, and Project integrations via `octocrab` and `graphql_client`. | `github` |
| [`polyphony-sqlite`](crates/sqlite) | Optional SQLite-backed state store. | `sqlite` |

## Feature flags

- `mock`: mock tracker and mock agent runtime support used by tests and internal smoke coverage. Not enabled by default.
- `linear`: Linear tracker adapter from `polyphony-linear`. Enabled by default in `polyphony-cli`.
- `github`: GitHub Issues tracker adapter. Enabled by default in `polyphony-cli`.
- `sqlite`: SQLite-backed persistence adapter.
- `agent-codex`: Codex provider runtime.
- `agent-claude`: Claude provider runtime.
- `agent-copilot`: Copilot provider runtime.
- `agent-openai`: OpenAI-compatible `/chat/completions` provider runtime.
- `agent-local`: local CLI fallback runtime.

## Current status

This repository already provides:

- a global Cargo workspace with crate-local `Error` enums built with `thiserror`
- trait seams for trackers, app-server runtimes, and persistence
- a dedicated workspace provisioner seam, with `git2`-backed linked worktree and clone support
- a separate workspace manager crate that handles sanitized path mapping, containment checks, hook execution, transient artifact cleanup, and rollback on failed initialization
- a real config layer based on the `config` crate, with built-in defaults, `~/.config/polyphony/config.toml`, repo-owned `WORKFLOW.md`, repo-local `.polyphony/config.toml`, and `POLYPHONY__...` env overlays
- a long-running async orchestrator with retries, reconciliation, workspace hooks, restart bootstrap, and live snapshots
- a top-level `codex:` workflow shorthand for simple single-agent Codex runs, with legacy `provider:` compatibility
- named agent profiles with state/label-based selection in `WORKFLOW.md`
- fallback agent chains so retries or throttled runs can hand off to another provider profile
- workflow-configured continuation prompts for live multi-turn agent sessions, with turn context such as `turn_number` and `max_turns`
- an agent registry runtime that delegates to provider-specific runtimes
- automatic model discovery for agents, via `/models` for OpenAI-compatible providers or `models_command` for CLI/app-server-backed agents
- saved per-issue agent context snapshots, built from streamed runtime events and reused on retries/handoffs
- runtime throttling when adapters surface `429`-style rate limits
- budget and spend snapshots that can be persisted and shown in the TUI
- a multi-tab `ratatui` dashboard for overview, activity, logs, and agent catalogs, with sparklines, progress gauges, scrollable panes, live tracing logs, and best-effort terminal theme matching
- a `tracker.kind: none` startup mode so the real runtime can boot before any tracker or agent is configured
- optional post-run GitHub handoff automation that can commit, push, open a draft PR, and post a review summary
- generic outbound feedback sinks for human handoff notifications, with Telegram and webhook implementations today

Git and GitHub implementation choices:

- `git2` is used for automatic linked worktree and discrete clone lifecycle, following the same direction Arbor uses.
- `octocrab` is used for GitHub Issues reads.
- `graphql_client` is used for Linear queries, GitHub PR comment mutations, and GitHub Project workflow sync.
- GitHub Issues can also be auto-linked into a canonical GitHub Project and have a project `Status` field updated best-effort when `tracker.project_owner` and `tracker.project_number` are configured.
- GraphQL schemas are checked in and can be refreshed with `just schema-github` and `just schema-linear`.

This repository now provides provider-specific runtime building blocks for:

- Codex-style app-server sessions
- local CLI and tmux-backed providers such as Claude CLI or Copilot CLI
- OpenAI-compatible chat providers exposing `/chat/completions`
- Moonshot/Kimi profiles through the OpenAI-compatible runtime, including `KIMI_API_KEY` / `MOONSHOT_API_KEY` env fallbacks

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
cargo run -p polyphony-cli
```

Install a release build into `~/.local/bin`:

```bash
just install
```

On first start, Polyphony creates `~/.config/polyphony/config.toml` if it does not exist. The
default config keeps `tracker.kind = "none"` and no agent profiles, so the real TUI starts
without mock data. In git repos with a generic shared workflow, Polyphony also seeds
`.polyphony/config.toml` so you can point workspaces back at the current repository without editing
the checked-in workflow. Once you configure GitHub or Linear in the repo-local config, Polyphony
can poll and display real issues even before any LLM provider is set up.

If `WORKFLOW.md` is missing in the current directory, Polyphony offers to create a repo-local
starter workflow in the TUI. In `--no-tui` mode it writes that starter file automatically.

In this repository, the tracked [WORKFLOW.md](WORKFLOW.md) is the real workflow policy for running
Polyphony on Polyphony. Generic starter references now live under [templates/WORKFLOW.md](templates/WORKFLOW.md),
[templates/config.toml](templates/config.toml), and
[templates/repo-config.toml](templates/repo-config.toml). Copyable full-file examples live under
[templates/examples/](templates/examples).

TUI controls:

- `1-4` or `Tab` / `Shift-Tab` switch tabs
- `j` / `k` or arrow keys move selections
- `PgUp` / `PgDn` and `g` / `G` scroll logs and event history
- `r` refreshes the runtime snapshot
- `q` quits

Run without the TUI:

```bash
cargo run -p polyphony-cli -- --no-tui
```

Structured logging and OTLP telemetry:

```bash
RUST_LOG=polyphony=debug cargo run -p polyphony-cli -- --log-json
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
RUST_LOG=info \
cargo run -p polyphony-cli -- --no-tui
```

When `OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set, the CLI
installs an OTLP trace exporter over tonic, flushes the tracer provider on shutdown, and routes
local tracing output into the TUI `Logs` pane while the dashboard is active. The service name
defaults to `polyphony` and can be overridden with `OTEL_SERVICE_NAME`. If OTLP exporter setup
fails, Polyphony prints a warning and continues with local logs only. If the TUI fails to start or
crashes, the service falls back to headless mode, flushes buffered logs back to stderr, and keeps
running until `Ctrl-C`. The `Logs` tab follows the active `RUST_LOG` filter, keeps the newest
lines pinned at the bottom by default, and shows a live scrollbar when you scroll back through
older output. A quiet setting such as `RUST_LOG=warn` will intentionally hide the normal startup
`info` lines. When the terminal replies to OSC color queries, Polyphony also derives the dashboard
palette from the active terminal foreground and background colors instead of forcing the built-in
blue slate theme.

Enable SQLite persistence:

```bash
cargo run -p polyphony-cli --features sqlite -- --sqlite-url sqlite://polyphony.db
```

Build everything, including optional adapters:

```bash
cargo check --workspace --all-features
```

Build the documentation book:

```bash
just docs-build
```

Serve the documentation locally:

```bash
just docs-serve
```

Refresh checked-in GraphQL schemas:

```bash
just schema-github
just schema-linear
```

`schema-linear` requires `LINEAR_API_KEY` in the environment and refreshes
`crates/linear/src/linear_schema.json` from the live Linear endpoint.

## Configuration

Polyphony uses three config layers:

- `~/.config/polyphony/config.toml`: user-local credentials, reusable agent profiles, and optional personal defaults. The CLI creates this file on first start.
- `WORKFLOW.md`: repo-owned prompt text plus shared workflow policy.
- `.polyphony/config.toml`: repo-local untracked tracker identity and workspace wiring overrides. The CLI creates this automatically in git repos when the checked-in workflow is still generic.

Merge order is:

1. built-in defaults
2. `~/.config/polyphony/config.toml`
3. `WORKFLOW.md` front matter
4. `.polyphony/config.toml`
5. `POLYPHONY__...` environment variables

Any string value that starts with `$` is resolved from the environment, so values such as
`api_key = "$OPENAI_API_KEY"` or `api_key = "$GITHUB_TOKEN"` keep secrets out of the file.

Tracker polling defaults to once per minute. Override `polling.interval_ms` in `WORKFLOW.md` only
when you actually need a faster loop, otherwise you are mostly just paying extra API tax for the
same issue list.

Keep tracker identity and repo or project selection out of the global config.
That includes:

- `tracker.kind`
- `tracker.profile`
- `tracker.repository`
- `tracker.project_slug`
- `tracker.project_owner`
- `tracker.project_number`
- `tracker.project_status_field`
- `workspace.source_repo_path`
- `workspace.clone_url`

Prefer `.polyphony/config.toml` for those local repo settings when the checked-in `WORKFLOW.md` is
shared policy or template text.

Shared tracker credentials can live in `~/.config/polyphony/config.toml` under
`trackers.profiles.<name>`. Repo-local config can then select one with
`tracker.profile = "<name>"`.

Provider setup belongs in `~/.config/polyphony/config.toml`. Supported profiles in the default
CLI build are:

- Codex app-server: `kind = "codex"`, `transport = "app_server"`, `command = "codex app-server"`
- Claude CLI: `kind = "claude"` or `kind = "anthropic"`, usually `transport = "local_cli"` and `command = "claude"`
- GitHub Copilot CLI: `kind = "copilot"` or `kind = "github-copilot"`, usually `transport = "local_cli"` and `command = "copilot"`
- OpenAI-compatible HTTP providers: `kind = "openai"`, `kind = "openai-compatible"`, or `kind = "openrouter"` with `transport = "openai_chat"`
- Kimi / Moonshot: `kind = "kimi"` or `kind = "moonshotai"` with `transport = "openai_chat"`, defaulting to `https://api.moonshot.ai/v1`

For a shared multi-provider, multi-tracker user config, copy
[templates/examples/config.multi-provider.toml](templates/examples/config.multi-provider.toml)
into `~/.config/polyphony/config.toml`.

The generated `~/.config/polyphony/config.toml` template includes every supported top-level option
plus commented provider examples for Codex, Claude, Copilot, OpenAI-compatible providers,
OpenRouter, and Kimi. The checked-in reference copies live in [templates/config.toml](templates/config.toml)
and [templates/repo-config.toml](templates/repo-config.toml).

For repo-local GitHub setup in `.polyphony/config.toml`, with no LLM providers yet, copy
[templates/examples/repo-config.github.toml](templates/examples/repo-config.github.toml).

For repo-local Linear setup, copy
[templates/examples/repo-config.linear.toml](templates/examples/repo-config.linear.toml).

For a repo-owned workflow with multiple named agents and shared policy, copy
[templates/examples/WORKFLOW.multi-agent.md](templates/examples/WORKFLOW.multi-agent.md)
into `WORKFLOW.md`.

For a simple single-agent Codex workflow, copy
[templates/examples/WORKFLOW.codex-shorthand.md](templates/examples/WORKFLOW.codex-shorthand.md)
into `WORKFLOW.md`.

The legacy top-level `provider:` block remains accepted as a deprecated alias for the same
single-agent shorthand.

For automated PR handoff and feedback sinks, copy
[templates/examples/WORKFLOW.automation-feedback.md](templates/examples/WORKFLOW.automation-feedback.md)
into `WORKFLOW.md`.

When `fetch_models` is enabled:

- `openai_chat` agents query the provider’s `/models` endpoint automatically
- `openai_chat` agents without an `api_key` skip `/models` discovery until credentials are configured, so optional profiles do not block startup
- `local_cli` and `app_server` agents can run `models_command` and parse either JSON model lists or newline-delimited model IDs
- interactive `local_cli` agents default to `stdin` prompt injection, or `tmux_paste` when `use_tmux: true`
- `kimi` / `moonshotai` profiles default to `https://api.moonshot.ai/v1` and resolve `KIMI_API_KEY` or `MOONSHOT_API_KEY`

Saved context and handoff behavior:

- Polyphony keeps a per-issue context snapshot from streamed agent events, usage, status, and recent transcript lines.
- Retries can rotate to fallback agents while appending that saved context into the next prompt.
- Local CLI and app-server commands also receive `POLYPHONY_CONTEXT_FILE`, `POLYPHONY_CONTEXT_JSON`, and `POLYPHONY_PRIOR_AGENT` so wrappers can consume structured handoff state directly.

## Next steps

- Finish the Linear and GitHub normalization layers, especially blockers and pagination.
- Expand SQLite recovery to restore richer in-flight metadata beyond retries, throttles, and budgets.
- Add an HTTP snapshot API on top of the runtime snapshot model.
