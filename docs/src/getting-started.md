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

On first start, the CLI creates `~/.config/polyphony/config.toml` if it does not exist. The
generated default config keeps `tracker.kind = "none"` and no dispatch agents, so the real CLI can
start without external services or mock data:

```bash
cargo run -p polyphony-cli
```

If the repo already ships a generic shared `WORKFLOW.md`, the CLI also seeds
`.polyphony/config.toml` so you can point workspaces back at the current repository without
editing the checked-in workflow.

Configure GitHub or Linear in `.polyphony/config.toml` when you want the dashboard to show real
issues for the current repo. Leave `agents.profiles` empty in `~/.config/polyphony/config.toml`
for tracker-only mode.

If you work across multiple repos with different trackers, keep shared tracker credentials in
`~/.config/polyphony/config.toml` under `trackers.profiles.<name>`, then select one in
`.polyphony/config.toml` with `tracker.profile = "<name>"`.

For public GitHub repositories, issue polling can work without `GITHUB_TOKEN`, but authenticated
requests are still recommended to avoid low anonymous rate limits.

Starter references for the generated files live in `templates/WORKFLOW.md`,
`templates/config.toml`, and `templates/repo-config.toml`. Copyable full-file examples live under
`templates/examples/`.

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
