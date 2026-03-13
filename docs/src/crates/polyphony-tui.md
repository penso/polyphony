# polyphony-tui

`polyphony-tui` renders the live terminal dashboard.

It also owns the startup initializer modal that appears when a repo-local `WORKFLOW.md` has not
been created yet.

## Responsibility

It consumes `RuntimeSnapshot` values and displays:

- an `Overview` tab with metric cards, issue queues, and inspector panels
- an `Activity` tab with recent events, network cadence gauges, and budgets or throttles
- a full-height `Logs` tab with sticky tail-follow, chronological scrolling, and a live scrollbar
- an `Agents` tab with discovered model catalogs and budget gauges
- sparkline histories for visible issues, running work, retries, token deltas, and event bursts
- cadence and budget progress bars backed by live runtime state
- a best-effort terminal palette probe so Ghostty and other OSC-aware terminals can tint the UI to
  match the active foreground and background colors

## Interaction model

The UI remains thin, but it is now stateful enough to navigate:

- `1-4` or `Tab` / `Shift-Tab` switch tabs
- `j` / `k` or arrow keys move table selection
- `PgUp` / `PgDn` and `g` / `G` scroll logs and event history
- `r` requests a refresh
- `q` quits

All business logic still remains in the orchestrator.
