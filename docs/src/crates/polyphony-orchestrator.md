# polyphony-orchestrator

`polyphony-orchestrator` is the scheduler and state machine for the runtime.

## Responsibility

It owns:

- the polling loop
- running and retry queues
- claim tracking
- stall detection
- budget and model refresh intervals
- throttle registration from provider rate limits
- snapshot publication for the TUI and persistence

## Main entrypoints

- `RuntimeService::new` assembles the service and exposes a `RuntimeHandle`
- `RuntimeService::run` drives the main loop
- `spawn_workflow_watcher` reloads `WORKFLOW.md` and triggers refreshes

## Why it is separate

The orchestrator does not parse workflows, render UI, or implement tracker logic. It coordinates
those pieces through the traits defined in `polyphony-core`.
