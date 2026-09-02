<div align="center">

<a href="https://polyphony.to"><img src="https://polyphony.to/assets/logo.svg" alt="Polyphony" width="64"></a>

# Polyphony

Git-native AI orchestration engine. Turn repository events into orchestrated agent work, from issues, PRs, webhooks, or schedules.

[![CI](https://github.com/penso/polyphony/actions/workflows/ci.yml/badge.svg)](https://github.com/penso/polyphony/actions/workflows/ci.yml)
[![Rust nightly-2025-11-30](https://img.shields.io/badge/rust-nightly--2025--11--30-orange?logo=rust)](justfile)
[![Edition 2024](https://img.shields.io/badge/edition-2024-blue)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE.md)

[Install](#install) - [Events](#events) - [Agents](#agents) - [Terminal UI](#terminal-ui) - [Web UI](#web-ui) - [Documentation](#documentation) - [Development](#development)

![Polyphony TUI](.github/media/screenshot.jpeg)

</div>

---

Polyphony connects your issue trackers to AI coding agents, runs them in isolated workspaces, and shows everything live in a terminal dashboard.

Inspired by [OpenAI Symphony](https://github.com/openai/symphony), Polyphony brings the same workflow-contract orchestration model to local repositories, but with multiple event sources and multiple agent backends.

## Install

```bash
brew install penso/polyphony/polyphony
```

Or build from source:

```bash
cargo install --path crates/cli
```

Then run it inside any git repository:

```bash
polyphony init
polyphony
```

`polyphony init` bootstraps `WORKFLOW.md`, `~/.config/polyphony/config.toml`, repo-local agent
prompts, and a starter `polyphony.toml` when local tracker wiring can be inferred or requested. It
auto-detects GitHub and GitLab remotes, supports explicit pack parameters for tracker/repository
defaults, and prints setup hints for missing tracker or auth wiring.

Starter packs are built in:

```bash
polyphony init --list-packs
polyphony init --pack codex
polyphony init --pack multi-agent
polyphony init --pack pipeline-static --tracker github --repository owner/repo
polyphony init --pack codex --tracker linear --project-slug ENG
```

## Events

Polyphony listens for work events from multiple sources:

- **GitHub** — issues and pull requests
- **GitLab** — issues via GraphQL
- **Linear** — issues via GraphQL
- **Beads** — local Dolt-backed issue tracking

## Agents

Plug in any combination of AI coding agents:

- **Claude** — Anthropic's CLI agent
- **Codex** — OpenAI's Codex CLI via app-server
- **Copilot** — GitHub Copilot CLI
- **Pi** — Warp's Pi agent via native RPC
- **OpenAI Chat** — any OpenAI-compatible API (OpenRouter, Kimi, etc.)
- **ACP / ACPX** — Agent Communication Protocol agents and bridges

Each agent gets its own workspace (worktree, directory, or clone), a shared workflow policy, retries with fallback chains, and budget-aware throttling.

## Terminal UI

The default terminal dashboard provides an issue inbox, detailed session timelines, live agent
activity, issue creation across configured trackers, and controls for dispatching, pausing, and
clearing local sessions. Use `--tui current` to launch the legacy dashboard.

## Web UI

Polyphony includes a web interface that runs alongside the terminal dashboard. When `daemon.listen_port` is set, both the TUI and the web UI are available simultaneously:

- **SSR dashboard** — server-rendered pages for inbox, runs, agents, tasks, and logs
- **GraphQL API** — query and mutate runtime state, with an interactive playground at `/graphql`
- **WebSocket subscriptions** — real-time state updates via GraphQL subscriptions at `/graphql/ws`
- **Jinja templates** — HTML templates in `crates/httpd/templates/`, easy to customize

```bash
just httpd          # TUI + web UI on port 8080
just httpd 3000     # TUI + web UI on custom port
just httpd-only     # web UI only (no TUI), port 8080
```

Or configure `daemon.listen_port` in your workflow config to always enable the web UI.

## Documentation

Full reference material lives in [`docs/`](docs/src):

- [Introduction](docs/src/introduction.md)
- [Getting Started](docs/src/getting-started.md)
- [Workflow Configuration](docs/src/workflow.md)
- [Built-In Tools](docs/src/tools.md)
- [Provider Runtimes](docs/src/providers.md)
- [Architecture](docs/src/architecture.md)

## Development

```bash
just format   # format code
just lint     # clippy + checks
just test     # run tests
just httpd    # run web UI on port 8080
```

## License

MIT
