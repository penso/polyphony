# polyphony-cli

`polyphony-cli` is the executable entrypoint of the workspace.

## Responsibility

It parses command-line flags, loads the workflow, assembles the runtime components, and starts the
service plus optional TUI. It also owns fail-soft operator-surface behavior, such as falling back
to local logs when OTLP setup fails and continuing headless when the TUI cannot stay up.

## Current command-line surface

The binary accepts:

- a workflow path, defaulting to `WORKFLOW.md`
- `--no-tui`
- `--log-json`
- `--sqlite-url`

## Build-time composition

This crate owns the feature gates that decide which tracker and persistence adapters are available
in a given build.
