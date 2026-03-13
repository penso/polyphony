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

## Symphony References

- For orchestration architecture and workflow-contract reference, inspect the upstream Symphony project at [github.com/openai/symphony](https://github.com/openai/symphony).
- Read the Symphony service specification in [SPEC.md](https://github.com/openai/symphony/blob/main/SPEC.md) and the local checkout at `/Users/penso/code/symphony/SPEC.md`.
- Prefer the local Symphony checkout at `/Users/penso/code/symphony` when comparing implementation details or reading larger docs offline.
- For a concrete implementation reference, inspect `/Users/penso/code/symphony/elixir/README.md` and `/Users/penso/code/symphony/elixir/WORKFLOW.md`.
- Treat Symphony as a reference for single-repo, repository-owned workflow orchestration, not as proof that Polyphony already supports one daemon managing many repos or projects.
