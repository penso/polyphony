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
| [`polyphony-core`](crates/polyphony-core) | Domain model and trait contracts used across the workspace. | Always |
| [`polyphony-workflow`](crates/polyphony-workflow) | `WORKFLOW.md` loader, typed config, and prompt rendering. | Always |
| [`polyphony-agent-common`](crates/polyphony-agent-common) | Shared shell/model/budget helpers used by provider runtimes. | Always |
| [`polyphony-agent-local`](crates/polyphony-agent-local) | Local CLI and tmux execution engine for membership-backed providers. | `agent-local` |
| [`polyphony-agent-codex`](crates/polyphony-agent-codex) | Codex app-server runtime. | `agent-codex` |
| [`polyphony-agent-openai`](crates/polyphony-agent-openai) | OpenAI-compatible HTTP runtime with streaming/tool-loop handling. | `agent-openai` |
| [`polyphony-agent-claude`](crates/polyphony-agent-claude) | Claude provider wrapper on top of the local CLI runtime. | `agent-claude` |
| [`polyphony-agent-copilot`](crates/polyphony-agent-copilot) | Copilot provider wrapper on top of the local CLI runtime. | `agent-copilot` |
| [`polyphony-agents`](crates/polyphony-agents) | Agent registry that wires provider-specific runtimes into the workspace build. | Always |
| [`polyphony-orchestrator`](crates/polyphony-orchestrator) | Async orchestrator loop, retries, reconciliation, and workspace hooks. | Always |
| [`polyphony-workspace`](crates/polyphony-workspace) | Workspace manager for path safety, lifecycle hooks, rollback, and cleanup. | Always |
| [`polyphony-git`](crates/polyphony-git) | Git-backed workspace provisioning for linked worktrees and discrete clones. | Always |
| [`polyphony-feedback`](crates/polyphony-feedback) | Generic outbound feedback sinks such as Telegram and webhooks. | Always |
| [`polyphony-tui`](crates/polyphony-tui) | `ratatui` status surface for live runtime snapshots. | Always |
| [`polyphony-cli`](crates/polyphony-cli) | Thin binary that wires the build together. | Always |
| [`polyphony-issue-mock`](crates/polyphony-issue-mock) | Mock tracker and mock agent runtime for tests and local smoke runs. | `mock` |
| [`polyphony-linear`](crates/polyphony-linear) | Linear tracker adapter using typed GraphQL queries. | `linear` |
| [`polyphony-github`](crates/polyphony-github) | GitHub Issues, PR, and Project integrations via `octocrab` and `graphql_client`. | `github` |
| [`polyphony-sqlite`](crates/polyphony-sqlite) | Optional SQLite-backed state store. | `sqlite` |

## Feature flags

- `mock`: demo tracker and demo agent runtime. Enabled by default.
- `linear`: Linear tracker adapter from `polyphony-linear`.
- `github`: GitHub Issues tracker adapter.
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
- a real config layer based on the `config` crate, with defaults, env overlays, and typed deserialization from `WORKFLOW.md`
- a long-running async orchestrator with retries, reconciliation, workspace hooks, restart bootstrap, and live snapshots
- named agent profiles with state/label-based selection in `WORKFLOW.md`
- fallback agent chains so retries or throttled runs can hand off to another provider profile
- an agent registry runtime that delegates to provider-specific runtimes
- automatic model discovery for agents, via `/models` for OpenAI-compatible providers or `models_command` for CLI/app-server-backed agents
- saved per-issue agent context snapshots, built from streamed runtime events and reused on retries/handoffs
- runtime throttling when adapters surface `429`-style rate limits
- budget and spend snapshots that can be persisted and shown in the TUI
- a `ratatui` dashboard for running work, retry queue, token totals, throttles, budgets, and recent events
- an implementation-owned `tracker.kind: mock` extension so the system can be run locally without Linear
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

The default `WORKFLOW.md` uses the mock tracker, so the TUI starts immediately and shows seeded demo issues.

Run without the TUI:

```bash
cargo run -p polyphony-cli -- --no-tui
```

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
`crates/polyphony-linear/src/linear_schema.json` from the live Linear endpoint.

The repo-owned workflow can now declare multiple named agents:

```yaml
agents:
  default: codex
  by_state:
    todo: claude
  profiles:
    codex:
      kind: codex
      transport: app_server
      command: codex app-server
      fallbacks:
        - kimi_fast
      fetch_models: true
      models_command: codex models --json
    kimi_fast:
      kind: kimi
      api_key: $KIMI_API_KEY
      model: kimi-2.5
      fetch_models: true
    claude:
      kind: claude
      transport: local_cli
      command: claude
      use_tmux: true
      interaction_mode: interactive
      fetch_models: true
      models_command: claude models --json
    openai:
      kind: openai
      transport: openai_chat
      fetch_models: true
```

The workflow can also declare automated PR handoff and feedback sinks:

```yaml
automation:
  enabled: true
  draft_pull_requests: true
  review_agent: codex
  commit_message: "fix({{ issue.identifier }}): {{ issue.title }}"
feedback:
  offered:
    - telegram
    - webhook
  telegram:
    ops:
      bot_token: $TELEGRAM_BOT_TOKEN
      chat_id: "123456789"
  webhook:
    audit:
      url: https://example.com/polyphony/handoff
      bearer_token: $HANDOFF_WEBHOOK_TOKEN
```

When `fetch_models` is enabled:

- `openai_chat` agents query the provider’s `/models` endpoint automatically
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
