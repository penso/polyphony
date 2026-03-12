# polyphony-cli

`polyphony-cli` is the executable entrypoint of the workspace.

## Responsibility

It parses command-line flags, loads the workflow, assembles the runtime components, and starts the
service plus optional TUI.

## Current command-line surface

The binary accepts:

- a workflow path, defaulting to `WORKFLOW.md`
- `--no-tui`
- `--log-json`
- `--sqlite-url`

## Build-time composition

This crate owns the feature gates that decide which tracker and persistence adapters are available
in a given build.
