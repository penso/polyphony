# Introduction

`polyphony` is a Rust workspace for repo-native AI agent orchestration.

At a high level, the system:

- loads user-local config from `~/.config/polyphony/config.toml`, shared workflow policy from
  `WORKFLOW.md`, and optional repo-local overrides from `.polyphony/config.toml`
- polls an issue tracker such as GitHub or Linear, or starts in a trackerless `none` mode while you wire it up
- provisions an isolated workspace for each issue
- selects an agent profile based on workflow rules
- runs the agent and tracks retries, budgets, and runtime state
- renders live snapshots in a `ratatui` dashboard

The codebase is split into focused crates so that orchestration logic stays separate from
workflow parsing, workspace lifecycle, tracker integrations, persistence, and the TUI.

Use this book as the project-level map. The crate source remains the most precise reference
for implementation details.
