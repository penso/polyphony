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
- pipeline dispatch, task sequencing, and movement tracking

## Main entrypoints

- `RuntimeService::new` assembles the service and exposes a `RuntimeHandle`
- `RuntimeService::run` drives the main loop
- `spawn_workflow_watcher` watches `WORKFLOW.md` plus the repo-local `.polyphony/config.toml` when present and nudges the runtime to reload

The orchestrator owns the authoritative reload path. It re-reads the workflow and repo-local config
on watcher nudges and also re-checks them defensively on poll ticks, then rebuilds hot-reloadable
runtime components for future dispatch without restarting in-flight agent sessions.

## Why it is separate

The orchestrator does not parse workflows, render UI, or implement tracker logic. It coordinates
those pieces through the traits defined in `polyphony-core`.

## Pipeline orchestration

When `pipeline.enabled = true`, the orchestrator decomposes each issue into a Movement with
sequential Tasks instead of running a single agent. The pipeline path adds:

- `dispatch_pipeline()` — creates a Movement and routes to planner or static stages
- `handle_planner_finished()` — reads `.polyphony/plan.json` and creates Tasks
- `dispatch_next_task()` — finds the next pending task and dispatches its agent
- `handle_task_finished()` — updates task status, dispatches next or re-plans on failure
- `complete_pipeline()` — marks the Movement as delivered and runs automation handoff

Movements and tasks are stored via `StateStore` and restored on startup. The standard
`finish_running()` path detects pipeline workers and routes to the appropriate handler.
