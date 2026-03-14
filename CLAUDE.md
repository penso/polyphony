# AGENTS.md

This file provides Codex-specific repository guidance.

For general repository rules, Rust workflow, testing expectations, and release hygiene, also follow [CLAUDE.md](/Users/penso/code/polyphony/CLAUDE.md).

## TUI And Ratatui

- Before changing [`crates/tui`](/Users/penso/code/polyphony/crates/tui), read the official ratatui tutorials at [ratatui.rs/tutorials](https://ratatui.rs/tutorials/).
- Use the widget and layout API docs at [docs.rs/ratatui/latest/ratatui](https://docs.rs/ratatui/latest/ratatui/).
- Prefer local source and examples from `/Users/penso/code/ratatui` when you need real patterns from the same project, especially:
  - `/Users/penso/code/ratatui/examples/apps/table/src/main.rs`
  - `/Users/penso/code/ratatui/examples/apps/demo/src/ui.rs`
  - `/Users/penso/code/ratatui/examples/apps/demo2/src/tabs/traceroute.rs`
  - `/Users/penso/code/ratatui/examples/apps/scrollbar/src/main.rs`
- For terminal UX inspiration, inspect `/Users/penso/code/lazygit`.
- Prefer stateful tables, scrollbars for long panes, sparklines for short history trends, and gauges or progress bars for cadence, retry, or completion state.
- Design for both wide and narrow terminals. Do not assume a large screen.
- Check the ratatui version pinned in the workspace `Cargo.toml` and `Cargo.lock` before copying APIs from the website or local checkout. The local `~/code/ratatui` clone may be ahead of the version this repo actually builds against.
- **Never block the TUI loop.** The draw → input → update cycle must stay instant. After handling a keypress, loop back to draw immediately — do not sleep, await network, or fall into a timed select. Network fetches and other async work belong in the orchestrator; the TUI only reads the latest snapshot.

## Type System Conventions

- Prefer enums over `String` for fields with a fixed set of values. Enums catch invalid values at compile time (or deserialization time), enable exhaustive `match`, and eliminate manual string validation.
- Derive `Copy` on fieldless enums (`AgentTransport`, `FeedbackChannelKind`, `AgentEventKind`, etc.) so they can be passed by value without `.clone()`.
- Use `#[serde(rename_all = "snake_case")]` on enums whose serialized form must be lowercase/snake_case.
- **Config crate limitation:** `AgentProfileConfig` and `FeedbackConfig` are deserialized through the `config` crate, which does not honor serde `rename_all` on enum variants. Fields in these structs that accept enum-like values must stay as `String` with manual parsing (see `infer_agent_transport()` and the `feedback.offered` validation in `ServiceConfig::validate()`). Only use typed enums in structs deserialized directly by serde (serde_yaml, serde_json).
- When replacing a `String` field with an enum, update all construction sites, match arms, format strings (use `{:?}` for Debug output of enums where the old code printed the string), and test assertions.

## Symphony References

- For orchestration architecture and workflow-contract reference, inspect the upstream Symphony project at [github.com/openai/symphony](https://github.com/openai/symphony).
- Read the Symphony service specification in [SPEC.md](https://github.com/openai/symphony/blob/main/SPEC.md) and the local checkout at `/Users/penso/code/symphony/SPEC.md`.
- Prefer the local Symphony checkout at `/Users/penso/code/symphony` when comparing implementation details or reading larger docs offline.
- For a concrete implementation reference, inspect `/Users/penso/code/symphony/elixir/README.md` and `/Users/penso/code/symphony/elixir/WORKFLOW.md`.
- Treat Symphony as a reference for single-repo, repository-owned workflow orchestration, not as proof that Polyphony already supports one daemon managing many repos or projects.

## Remote API Calls

- Minimize network round-trips to trackers (GitHub, GitLab, Linear). Batch and deduplicate requests wherever possible.
- GitHub's GraphQL API can fetch issues with full data (body, labels, author metadata, timestamps, comments) in a single paginated query — prefer it over multiple REST calls when adding new fetch paths.
- Avoid per-issue REST fetches in loops; use bulk list endpoints or GraphQL instead.
- Cache results locally (via `NetworkCache` / `CachedSnapshot`) and only re-fetch on poll intervals or explicit refresh.

## Git Workflow

Conventional commits: `feat|fix|docs|style|refactor|test|chore(scope): description`
**No `Co-Authored-By` trailers.** Update `README.md` features list with `feat` commits.

## Changelog

- Do **not** add manual `CHANGELOG.md` entries in normal PRs.
- `CHANGELOG.md` entries are generated from commit history via `git-cliff` (`cliff.toml`).
- Use conventional commits and preview unreleased notes with `just changelog-unreleased`.
