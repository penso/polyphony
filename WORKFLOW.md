---
tracker:
  kind: none
polling:
  interval_ms: 60000
workspace:
  root: .polyphony/workspaces
  checkout_kind: worktree
  clone_url: https://github.com/penso/polyphony.git
  default_branch: main
  sync_on_reuse: true
  transient_paths:
    - tmp
    - .elixir_ls
agent:
  max_concurrent_agents: 3
  max_turns: 4
  max_retry_backoff_ms: 60000
---
# Polyphony Repository Workflow

You are working on the Polyphony Rust workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

Execution rules:

- Stay inside the assigned workspace and keep changes focused on the issue.
- Prefer simple, typed Rust changes. Do not introduce panics in non-test code.
- Run `just format`, `just lint`, and `just test` when code changes.
- Run `just docs-build` when docs or user-facing behavior changes.
- Keep `README.md` and `docs/` in sync with behavior changes.
- Prefer unit or end-to-end coverage over mock-only tests.
- Leave the issue in a clear handoff state with changed files, tests, and follow-up notes.

This checked-in `WORKFLOW.md` is the real workflow policy for the Polyphony repository.
Use `.polyphony/config.toml` for local tracker selection, project identity, and machine-specific
workspace overrides without editing the tracked workflow.
