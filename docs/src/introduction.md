# Introduction

`polyphony` is a Rust workspace for repo-native AI agent orchestration.

At a high level, the system:

- loads a repository-owned `WORKFLOW.md`
- polls an issue tracker such as GitHub, Linear, or the built-in mock tracker
- provisions an isolated workspace for each issue
- selects an agent profile based on workflow rules
- runs the agent and tracks retries, budgets, and runtime state
- renders live snapshots in a `ratatui` dashboard

The codebase is split into focused crates so that orchestration logic stays separate from
workflow parsing, workspace lifecycle, tracker integrations, persistence, and the TUI.

Use this book as the project-level map. The crate source remains the most precise reference
for implementation details.
