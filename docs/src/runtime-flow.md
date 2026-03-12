# Runtime Flow

The runtime starts in `polyphony-cli` and then hands control to a small set of long-lived
components.

## Startup

At startup the CLI:

1. loads `WORKFLOW.md`
2. initializes tracing
3. builds the selected tracker and agent registry runtime
4. creates the git-backed workspace provisioner
5. optionally connects the SQLite state store
6. starts `RuntimeService`
7. starts the workflow file watcher
8. launches the TUI unless `--no-tui` is set

## Scheduling loop

`polyphony-orchestrator` owns the main loop. On each tick it:

- validates the currently loaded workflow
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
4. `polyphony-agents` selects the provider runtime and runs the agent
5. agent events stream back into the orchestrator
6. the orchestrator updates snapshots, retry state, and budgets

## Completion and recovery

After an attempt finishes, the orchestrator:

- records run metadata when persistence is enabled
- updates workflow status on the tracker when supported
- schedules retries for non-terminal failures
- cleans up workspaces for terminal issues during reconciliation or startup cleanup

`polyphony-tui` remains a consumer of snapshots rather than a source of orchestration logic.
