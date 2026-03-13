# polyphony-cli

`polyphony-cli` is the executable entrypoint of the workspace.

## Responsibility

It parses command-line flags, creates `~/.config/polyphony/config.toml` on first start, seeds
`.polyphony/config.toml` when a repo already has a shared workflow but no local repo wiring, loads
the merged runtime config, assembles the runtime components, and starts the service plus optional
TUI. It also owns first-run workflow bootstrap when `WORKFLOW.md` is missing, plus fail-soft
operator-surface behavior such as falling back to local logs when OTLP setup fails, routing
tracing output into the TUI when it is active, and continuing headless when the TUI cannot stay
up. When the TUI is active, buffered log visibility still follows the active `RUST_LOG` filter.

## Current command-line surface

The binary accepts:

- a workflow path, defaulting to `WORKFLOW.md`
- `--no-tui`
- `--log-json`
- `--sqlite-url`

## Build-time composition

This crate owns the feature gates that decide which tracker and persistence adapters are available
in a given build.
