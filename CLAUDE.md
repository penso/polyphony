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
- **Never block the TUI loop.** The draw → input → update cycle must stay instant. After handling a keypress, loop back to draw immediately — do not sleep, await network, or fall into a timed select. Network fetches and other async work belong in the orchestrator; the TUI only reads the latest snapshot. Any operation taking more than 100ms must show a loading indicator (e.g. braille spinner) so the user knows something is happening.

### Listing tables

- Every tab is a full-height listing table with a scrollbar when content exceeds the visible area.
- Every listing row must support opening a detail modal (Enter key) that shows expanded information relevant to that row.
- **Column styling:** ID, time, and other secondary/metadata columns use the `theme.muted` color. Primary content (title, name) uses `theme.foreground`.
- **Emoji indicators:** Prefer emoji/Unicode symbols over text for columns that represent an enum with ≤5 values (status, decision, kind). This keeps columns compact. Examples: `●` open, `✓` done, `✕` failed, `◷` waiting, `⊘` cancelled.
- **No highlight arrow symbol.** Row selection uses background highlight only (`row_highlight_style`), not a `▸` prefix — the arrow duplicates the background highlight.
- **First-column activity indicator:** The first column of listing rows can show the item's activity state: a braille spinner (`⠋⠙⠹…`) when actively running, a dot (`●`) for static state like "has workspace", or blank.
- **Footer:** Show selection position (e.g. "3 of 12") and any relevant counts (e.g. retrying agents) in the table block's bottom border.
- **Sorting:** Support sort toggling (e.g. `s` key) where it makes sense. Show the current sort label in the footer.

## Website and Social Image

- The OpenGraph image (`website/assets/og-image.svg` → `og-image.png`) shows the TUI dashboard as a backdrop behind the Polyphony branding. It should visually match what a user actually sees in the product.
- When the TUI layout, columns, or tab structure changes, update the SVG to reflect the new UI. Render the PNG via Playwright (see `og-render.html` pattern) — ImageMagick does not render the SVG faithfully.
- Bump the `?v=` cache-busting parameter on the image URLs in `website/index.html` after regenerating the PNG.

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
**Before committing**, always run `just format` and `just lint` to catch formatting and clippy issues locally. This avoids a wasted CI round trip for trivial failures.

## Changelog

- Do **not** add manual `CHANGELOG.md` entries in normal PRs.
- `CHANGELOG.md` entries are generated from commit history via `git-cliff` (`cliff.toml`).
- Use conventional commits and preview unreleased notes with `just changelog-unreleased`.

## Rust Rules

- Do not use `unwrap()` or `expect()` in non-test code. In test modules, use `#[allow(clippy::unwrap_used, clippy::expect_used)]` on the module.
- Use clear error handling with typed errors (`thiserror`/`anyhow` where appropriate).
- Keep modules focused and delete dead code instead of leaving it around.
- Collapse nested `if` / `if let` statements when possible (clippy `collapsible_if`).
- **Never shell out to external CLIs** (`gh`, `git` via `Command::new`, etc.) for GitHub API calls or operations that can be done with Rust crates. Use `octocrab`, `reqwest`, or other Rust HTTP/API crates instead. The only acceptable use of `std::process::Command` is where no Rust crate equivalent exists.

## Testing Policy

- Every feature, behavior change, and bug fix must ship with tests. If the change is not testable yet, improve the design until it is or stop and discuss the tradeoff.
- Bug fixes need a regression test that fails before the fix and passes after it. Do not rely on manual QA as the only proof.
- TUI changes need coverage for the affected render path and/or key routing path. Runtime and orchestrator changes need coverage for the control-flow path that changed.
- Prefer focused crate-level tests for the touched paths while developing, then run the relevant package suites before handoff.

## Module Organization

- Split large files by domain: types, constants, helpers, actions. Keep files under ~800 lines where practical.
- **Do not add implementation code in `main.rs`, `lib.rs`, or `mod.rs`.** These files are for module declarations, re-exports, and top-level wiring only. Put implementation logic in dedicated modules. A `mod.rs` filename conveys no meaning about what it contains — always use named files (e.g., `render/triggers.rs` instead of putting trigger code in `render/mod.rs`).
- Use `pub(crate)` visibility for items shared within a crate but not exported. Apply to struct fields, methods, and free functions in submodules.
- Use `pub(crate) use module::*` glob re-exports in parent modules to keep call sites clean after extraction.
- When splitting `impl` blocks across files, the struct definition stays in `types.rs` and method impls go in the relevant domain file.

## Workspace Dependencies

All third-party dependency versions are centralized in the root `Cargo.toml` under `[workspace.dependencies]`. Crate-level `Cargo.toml` files must use `{ workspace = true }` (with optional extra keys like `features` or `optional`). Never hardcode a version in a subcrate — add it to the workspace root first.
