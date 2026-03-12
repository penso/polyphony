# Getting Started

## Prerequisites

The workspace is pinned to the toolchain declared in `rust-toolchain.toml` and the `justfile`.

Common commands:

```bash
just format
just lint
just test
just docs-build
```

## Running polyphony

The default `WORKFLOW.md` uses the mock tracker and mock agent, so the CLI can start without
external services:

```bash
cargo run -p polyphony-cli
```

Run without the TUI:

```bash
cargo run -p polyphony-cli -- --no-tui
```

Enable SQLite persistence:

```bash
cargo run -p polyphony-cli --features sqlite -- --sqlite-url sqlite://polyphony.db
```

## Building this book

This documentation uses `mdBook`.

Build the static site:

```bash
just docs-build
```

Serve it locally with live reload:

```bash
just docs-serve
```
