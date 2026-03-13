# Runtime Flow

The runtime starts in `polyphony-cli` and then hands control to a small set of long-lived
components.

## Startup

At startup the CLI:

1. loads `WORKFLOW.md`
2. initializes tracing, routing local logs into the TUI when active and falling back to local stderr logs if OTLP exporter setup fails
3. builds the selected tracker and agent registry runtime
4. creates the git-backed workspace provisioner
5. optionally connects the SQLite state store
6. starts `RuntimeService`
7. starts the workflow file watcher
8. launches the TUI unless `--no-tui` is set, and falls back to headless mode if the TUI fails

The file watcher is only a nudge. The orchestrator still re-reads `WORKFLOW.md` defensively on poll
ticks so missed filesystem events do not leave the runtime on stale config.

## Scheduling loop

`polyphony-orchestrator` owns the main loop. On each tick it:

- validates the currently loaded workflow
- defensively re-loads `WORKFLOW.md` and rebuilds hot-reloadable runtime components when it changed
- refreshes running issue state
- polls tracker candidates
- respects throttles and concurrency limits
- provisions a workspace for eligible issues
- renders the workflow prompt
- dispatches the selected agent

## Execution path

When an issue is dispatched:

1. `polyphony-workspace` creates or reuses the workspace
2. `polyphony-workspace` runs `after_create` or `before_run` hooks when configured
3. `polyphony-workflow` chooses the agent profile and renders the prompt
4. `polyphony-agents` selects the provider runtime and starts the agent
5. Codex app-server sessions can stay alive across multiple `turn/start` calls on the same thread
6. after each successful live turn, the orchestrator re-checks tracker state before deciding whether to continue
7. continuation turns can render a workflow-configured `agent.continuation_prompt`, with turn context such as `turn_number` and `max_turns`
8. agent events stream back into the orchestrator with live session metadata such as `session_id`, `thread_id`, `turn_id`, and the app-server PID when available
9. the orchestrator updates snapshots, saved context, retry state, and budgets from those streamed events
10. optional handoff automation can commit the branch, open a PR, run a review pass, and notify humans

When `WORKFLOW.md` changes successfully, future dispatch, retry handling, model discovery, budget
polling, and feedback/automation surfaces use the rebuilt runtime components. In-flight agent
sessions are not restarted automatically.

## Completion and recovery

After an attempt finishes, the orchestrator:

- records run metadata when persistence is enabled
- preserves distinct attempt outcomes such as `TimedOut` and `Stalled` instead of flattening them into generic failures
- updates workflow status on the tracker when supported
- schedules retries for non-terminal failures
- cleans up workspaces for terminal issues during reconciliation or startup cleanup

`polyphony-tui` remains a consumer of snapshots rather than a source of orchestration logic.
