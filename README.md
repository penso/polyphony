# polyphony

[![CI](https://github.com/penso/polyphony/actions/workflows/ci.yml/badge.svg)](https://github.com/penso/polyphony/actions/workflows/ci.yml)
[![Rust nightly-2025-11-30](https://img.shields.io/badge/rust-nightly--2025--11--30-orange?logo=rust)](justfile)
[![Edition 2024](https://img.shields.io/badge/edition-2024-blue)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE.md)

`polyphony` is a repo-native AI orchestration tool.

It watches work from your tracker, creates an isolated workspace for each issue, selects the right
agent profile, runs the agent, and shows the whole thing live in a terminal UI.

If you want a single place to coordinate Codex, Claude, Pi, OpenAI-compatible models, or bridge
providers like `acpx`, this is what the project is for.

## What It Does

- loads shared workflow policy from [`WORKFLOW.md`](WORKFLOW.md)
- merges user config from `~/.config/polyphony/config.toml` and repo-local overrides from `polyphony.toml`
- polls GitHub, Linear, Beads, or runs trackerless in `none` mode
- provisions per-issue workspaces with directory, linked-worktree, or clone strategies
- dispatches agents with retries, fallback chains, throttling, and saved context handoff
- runs local CLI agents inside managed terminal sessions, with a PTY backend by default and optional `tmux` sessions when you want manual attach
- exposes an allowlisted built-in tool registry to tool-capable runtimes for workspace reads, tracker updates, PR comments, and GraphQL escape hatches
- exposes terminal-aware ACP client capabilities, including `terminal/*` operations for compatible ACP agents
- renders the current state in a `ratatui` dashboard

## Quick Start

Install and run:

```bash
brew install penso/polyphony/polyphony
polyphony
```

Or run it directly from the workspace:

```bash
cargo run -p polyphony-cli
```

On first start, Polyphony creates `~/.config/polyphony/config.toml`. In repos with a generic
workflow, it also seeds `polyphony.toml` and these repo-local specialist prompts:

- `.polyphony/agents/router.md`
- `.polyphony/agents/implementer.md`
- `.polyphony/agents/researcher.md`
- `.polyphony/agents/tester.md`
- `.polyphony/agents/reviewer.md`

The default workflow is ready to use those agent types immediately. The simplest setup is usually:

1. configure the tracker in `polyphony.toml`
2. start `polyphony`
3. edit `.polyphony/agents/*.md` only when you want to change the role prompt or underlying model

Simple repo-local configuration:

```toml
# polyphony.toml
[tracker]
kind = "github"
repository = "owner/repo"

[orchestration]
router_agent = "router"

[agents]
default = "implementer"
reviewer = "reviewer"
```

Example specialist prompt with provider/model settings in front matter:

```md
---
kind: codex
transport: app_server
command: codex --dangerously-bypass-approvals-and-sandbox app-server
approval_policy: auto
thread_sandbox: workspace-write
turn_sandbox_policy: workspace-write
---
You are the routing agent for this movement.
Decide whether to keep the work as one implementation task or split it into specialist tasks.
```

Polyphony is set up for unattended agent runs. Use the CLI's "don't ask me again" flags where the
provider supports them. Today that means Claude with `--dangerously-skip-permissions` and Codex
with `--dangerously-bypass-approvals-and-sandbox`. Pi's CLI does not expose a separate approval
flag in its help, so there is no extra bypass switch to add there.

Agent-specific prompt files live outside TOML:

- `~/.config/polyphony/agents/<name>.md` for reusable personal defaults
- `.polyphony/agents/<name>.md` for repo-local overrides

Each file may include YAML front matter to override profile fields such as `kind`, `model`, or
`command`, plus a Markdown body that is appended as agent-specific instructions whenever that agent
is selected.

Dispatch mode now has an `idle` option in the TUI. It behaves like opportunistic auto-dispatch,
but only when no other agent is currently running and the selected agent's budget snapshot still
shows headroom. Polyphony treats `credits_remaining > 0` or positive spend headroom as usable
budget. If your budget command also returns raw JSON with `weekly_deficit`, `weekly.credit_deficit`,
`weekly_remaining`, or `weekly.credits_remaining`, `idle` mode will refuse to dispatch while the
weekly budget is underwater.

```toml
# ~/.config/polyphony/config.toml
[agents]
default = "codex"

[agents.profiles.codex]
kind = "codex"
transport = "app_server"
command = "codex --dangerously-bypass-approvals-and-sandbox app-server"
approval_policy = "auto"
thread_sandbox = "workspace-write"
turn_sandbox_policy = "workspace-write"
```

For local CLI agents, `use_tmux` is the switch you want. Leave it `false` to use Polyphony's
built-in PTY terminal backend, or set it to `true` to run the agent inside a tmux session you can
attach to manually.

## Built-In Tools

Polyphony can expose a small built-in tool registry to tool-capable models. The first built-in
tool set now includes:

- workspace read tools: `workspace_list_files`, `workspace_read_file`, `workspace_search`
- tracker mutation tools: `issue_update`, `issue_comment`
- PR feedback tool: `pr_comment`
- Linear escape hatch: `linear_graphql`

Tool exposure is explicit and allowlisted:

```yaml
tools:
  enabled: true
  allow:
    - workspace_list_files
    - workspace_read_file
    - workspace_search
    - issue_update
    - issue_comment
    - linear_graphql
  by_agent:
    reviewer:
      allow:
        - workspace_read_file
        - workspace_search
        - pr_comment
        - linear_graphql
```

The orchestrator does not execute tools itself. The registry is shared runtime state, while
provider runtimes such as Codex app-server and OpenAI-compatible chat handle the actual tool call
loop. See [docs/src/tools.md](docs/src/tools.md) for the feature overview and
[docs/src/workflow.md](docs/src/workflow.md) for config details.

Polyphony can also create review-only work when new commits land on open GitHub pull requests.
Enable `review_triggers.pr_reviews` to poll PR heads, debounce fresh pushes, run a review agent in
the PR workspace, and post the result back as a PR comment instead of opening another PR.

```toml
# ~/.config/polyphony/config.toml
[agents]
default = "pi"

[agents.profiles.pi]
kind = "pi"
transport = "rpc"
command = "pi"
model = "anthropic/claude-sonnet-4-5"
```

```toml
# ~/.config/polyphony/config.toml
[review_triggers.pr_reviews]
enabled = true
provider = "github"
agent = "codex"
debounce_seconds = 180
include_drafts = false
only_labels = ["ready-for-review"]
ignore_labels = ["wip"]
ignore_bot_authors = true
comment_mode = "summary"
```

Use `comment_mode = "inline"` if you want the review agent to optionally emit
`.polyphony/review-comments.json` and have Polyphony submit file-level GitHub
review comments in addition to the summary body. `only_labels`, `ignore_labels`,
`ignore_authors`, and `ignore_bot_authors` let you suppress noisy PRs without
turning the trigger off entirely.

```toml
# polyphony.toml
[tracker]
kind = "github"
repository = "owner/repo"

[workspace]
source_repo_path = "/path/to/repo"
```

Then make sure the repo has a [`WORKFLOW.md`](WORKFLOW.md), start `polyphony`, and dispatch work
from the TUI.

Useful variants:

```bash
polyphony --no-tui
cargo run -p polyphony-cli --features sqlite -- --sqlite-url sqlite://polyphony.db
```

Starter templates live in [`templates/`](templates) and full examples in
[`templates/examples/`](templates/examples).

## Supported Agent Styles

- `kind = "codex"` via app-server transport
- `kind = "claude"` and `kind = "copilot"` via local CLI transport, with PTY-backed terminal control and optional `tmux` attach flows
- `kind = "pi"` via Pi's native RPC mode
- `transport = "acpx"` for ACPX bridge-backed agents such as `claude`, `codex`, or `pi`
- `transport = "acp"` for stdio ACP agents, including ACP terminal client support
- `transport = "openai_chat"` for OpenAI-compatible HTTP providers, including OpenRouter and Kimi

## Documentation

The README is intentionally short. The reference material lives in `docs/`.

- [Introduction](docs/src/introduction.md)
- [Getting Started](docs/src/getting-started.md)
- [Releases](docs/src/releases.md)
- [Workflow Configuration](docs/src/workflow.md)
- [Built-In Tools](docs/src/tools.md)
- [Provider Runtimes](docs/src/providers.md)
- [Architecture](docs/src/architecture.md)
- [Runtime Flow](docs/src/runtime-flow.md)

Build the docs locally:

```bash
just docs-build
just docs-serve
```

## Development

The workspace is pinned to the toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) and the
commands in [`justfile`](justfile).

Common commands:

```bash
just format
just lint
just test
```
