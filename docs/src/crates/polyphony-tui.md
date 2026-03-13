# polyphony-tui

`polyphony-tui` renders the live terminal dashboard.

## Responsibility

It consumes `RuntimeSnapshot` values and displays:

- overall running and retry counts
- token and runtime totals
- budgets and throttle windows
- running sessions
- retry queue entries
- discovered agent models
- recent events
- buffered tracing logs forwarded from `polyphony-cli`

## Interaction model

The UI is intentionally thin. It only listens for:

- `q` to quit
- `r` to request a refresh

All business logic remains in the orchestrator.
