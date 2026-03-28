# Agent Configuration

Polyphony dispatches work to **agents** — external LLM-backed programs that receive a
prompt and execute tasks inside an isolated workspace. Each agent is defined by a
**profile** that specifies the provider, transport, model, credentials, and execution
constraints. Profiles can be shared across repositories or scoped to a single project.

## Configuration Layers

Agent profiles are merged from multiple sources, lowest priority first:

| Priority | Source | Scope | Typical use |
|----------|--------|-------|-------------|
| 1 | Built-in defaults | Global | Sensible zero-config values |
| 2 | `~/.config/polyphony/config.toml` | User-global | Reusable profiles, credentials, personal defaults |
| 3 | `WORKFLOW.md` front matter | Repository | Shared repo policy and prompt |
| 4 | `.polyphony/config.toml` | Repository-local | Local overrides not checked in |
| 5 | `POLYPHONY__...` environment variables | Process | CI, scripts, one-off overrides |

Agent **prompt files** add a sixth layer that can override individual profile fields
and supply a role-specific prompt template (see [Agent Prompt Files](#agent-prompt-files)
below).

### Where to put what

- **`~/.config/polyphony/config.toml`** — credentials (`api_key`), reusable provider
  profiles, personal defaults. Applies to every repository.
- **`WORKFLOW.md`** — the repo-owned workflow policy and shared prompt template.
  Checked in so every contributor uses the same agent wiring.
- **`.polyphony/config.toml`** — local repository wiring (`tracker.profile`,
  `workspace.source_repo_path`, `workspace.clone_url`) that you do not want in the
  checked-in workflow.
- **Agent prompt files** — per-agent role instructions and optional profile overrides.

## Profile Structure

A profile is defined under `[agents.profiles.<name>]` in any TOML config layer.

### Identity and Transport

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `description` | string | — | Short human-readable description shown in the TUI agent picker. |
| `kind` | string | — | Provider identifier. Determines transport inference, API key resolution, and base URL defaults. |
| `transport` | string | inferred from `kind` | Explicit transport override. Values: `app_server`, `local_cli`, `openai_chat`, `rpc`, `acp`, `acpx`, `mock`. |
| `command` | string | — | CLI command to launch the agent process. Required for `app_server` and `local_cli` transports. |

### Transport Inference

When `transport` is omitted, Polyphony infers it from `kind`:

| `kind` value | Inferred transport |
|---|---|
| `codex` | `app_server` |
| `pi` | `rpc` |
| `acp` | `acp` |
| `acpx` | `acpx` |
| `openai`, `openrouter`, `kimi`, `moonshot`, `mistral`, `deepseek`, `cerebras`, `gemini`, `zai`, `minimax`, `venice`, `groq` | `openai_chat` |
| `claude`, `copilot`, `github-copilot`, and anything else | `local_cli` |

### Model Selection

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | — | Specific model ID to use (e.g. `"gpt-5.4"`, `"claude-opus-4-6"`). |
| `reasoning_level` | string | — | Reasoning effort level: `"low"`, `"medium"`, `"high"`, or `"xhigh"`. Sent as `reasoning_effort` to OpenAI-compatible APIs and `reasoningEffort` to Codex app-server. Also exposed as `POLYPHONY_AGENT_REASONING_LEVEL` env var for local CLI agents. |
| `models` | string[] | `[]` | Static list of available models for the TUI model picker. |
| `models_command` | string | — | Shell command that returns a JSON list of models. |
| `fetch_models` | bool | `true` | Enable automatic model discovery via the provider's `/models` endpoint. |

For `openai_chat` profiles, at least one of `model`, `models`, or `fetch_models = true`
must be set. Discovery is skipped when no `api_key` is configured, so optional profiles
can stay dormant.

### Credentials

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | string | — | API key. Supports `$ENV_VAR` syntax for environment resolution. |
| `base_url` | string | provider default | Custom API endpoint. Auto-set for known providers (see below). |

#### Automatic API Key Resolution

When `api_key` is omitted, Polyphony falls back to well-known environment variables:

| `kind` | Environment variable |
|--------|---------------------|
| `openai` | `OPENAI_API_KEY` |
| `anthropic`, `claude` | `ANTHROPIC_API_KEY` |
| `copilot`, `github-copilot` | `GITHUB_TOKEN` or `GH_TOKEN` |
| `kimi`, `moonshot`, `moonshotai` | `KIMI_API_KEY` or `MOONSHOT_API_KEY` |

#### Default Base URLs

Known providers get a default `base_url` when none is set:

| `kind` | Default base URL |
|--------|-----------------|
| `kimi`, `moonshot` | `https://api.moonshot.ai/v1` |
| `openrouter` | `https://openrouter.ai/api/v1` |
| `mistral` | `https://api.mistral.ai/v1` |
| `deepseek` | `https://api.deepseek.com` |
| `cerebras` | `https://api.cerebras.ai/v1` |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| `zai` | `https://api.z.ai/api/paas/v4` |
| `minimax` | `https://api.minimax.io/v1` |
| `venice` | `https://api.venice.ai/api/v1` |
| `groq` | `https://api.groq.com/openai/v1` |

### Execution Constraints

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `turn_timeout_ms` | u64 | `3600000` (1 hour) | Maximum time per agent turn. |
| `read_timeout_ms` | u64 | `5000` | Read timeout for agent communication. |
| `stall_timeout_ms` | i64 | `300000` (5 min) | Time without output before declaring a stall. Set `<= 0` to disable. |
| `idle_timeout_ms` | u64 | `5000` | Idle timeout for interactive local CLI polling. |

### Sandbox and Approval

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `approval_policy` | string | — | `"auto"` or `"manual"`. Controls whether tool calls need human approval. |
| `thread_sandbox` | string | — | Sandbox policy for the thread (e.g. `"workspace-write"`). |
| `turn_sandbox_policy` | string | — | Per-turn sandbox policy. |

### Interactive Mode

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `interaction_mode` | string | `"one_shot"` | `"one_shot"` or `"interactive"`. Interactive keeps the agent process alive across turns. |
| `prompt_mode` | string | `"env"` | How prompts are delivered: `"env"`, `"stdin"`, or `"tmux_paste"`. |
| `use_tmux` | bool | `false` | Run the agent inside a tmux session. When `false`, uses Polyphony's managed PTY backend. |
| `tmux_session_prefix` | string | agent name | Prefix for tmux session names. Lets you `tmux attach -t <prefix>-...`. |
| `completion_sentinel` | string | — | Explicit string that signals the agent is done (for interactive mode). |

### Fallbacks and Environment

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `fallbacks` | string[] | `[]` | Ordered list of fallback agent profile names. Used when the primary agent fails. |
| `env` | map | `{}` | Extra environment variables passed to the agent process. |
| `credits_command` | string | — | Shell command returning JSON with `credits_remaining` (used by `idle` dispatch mode). |
| `spending_command` | string | — | Shell command returning JSON with spending data. |

## Agent Routing

The `[agents]` section controls which profile handles each issue:

```toml
[agents]
default = "codex"          # fallback for any issue
reviewer = "reviewer"      # dedicated review agent

[agents.by_state]
todo = "implementer"       # issues in "todo" state → implementer profile
"in progress" = "codex"

[agents.by_label]
risky = "claude"           # issues labeled "risky" → claude profile
research = "researcher"
```

Resolution order: `by_label` → `by_state` → `default`. The TUI also allows manual
dispatch to any profile via the agent picker (`D` key on an inbox item).

When `agents.profiles` is empty, Polyphony stays in **tracker-only mode**: it polls and
displays issues but does not dispatch work.

## Agent Prompt Files

Agent prompt files provide per-agent role instructions and optional profile overrides.
They are Markdown files with YAML front matter.

### Locations

| Path | Scope |
|------|-------|
| `~/.config/polyphony/agents/<name>.md` | User-global |
| `.polyphony/agents/<name>.md` | Repository-local |

The filename (without `.md`) becomes the agent name. Repository-local files override
user-global files: front matter fields from the repo file replace global ones, and the
prompt template is replaced if non-empty.

### Format

```markdown
---
description: Code implementation and direct changes
kind: codex
transport: app_server
command: codex --dangerously-bypass-approvals-and-sandbox app-server
approval_policy: auto
thread_sandbox: workspace-write
turn_sandbox_policy: workspace-write
model: gpt-4-turbo
---
You are the implementation specialist.

Focus on making the requested code changes cleanly and directly.

- Prefer simple, maintainable fixes over layered workarounds.
- Update tests and validation when behavior changes.
- Leave clear repository state for downstream specialist agents.
```

The YAML front matter supports every field from `AgentProfileConfig` — only the fields
you set override the base profile. The Markdown body is appended as role-specific
instructions when that agent runs.

### Built-in Templates

Polyphony ships starter templates under `templates/agents/`:

| Template | Role |
|----------|------|
| `router.md` | Orchestration and planning |
| `implementer.md` | Code implementation |
| `researcher.md` | Investigation and root-cause analysis |
| `tester.md` | Verification and testing |
| `reviewer.md` | Quality review and risk assessment |

Copy these to `~/.config/polyphony/agents/` or `.polyphony/agents/` and customize.

## Provider Quick Reference

### Codex (app-server)

```toml
[agents.profiles.codex]
kind = "codex"
transport = "app_server"
command = "codex app-server"
fetch_models = true
models_command = "codex models --json"
approval_policy = "auto"
thread_sandbox = "workspace-write"
turn_sandbox_policy = "workspace-write"
```

Speaks the Codex app-server JSON protocol over stdio. Supports continuation turns on the
same thread, built-in tool execution, and approval auto-responses.

### Claude (local CLI)

```toml
[agents.profiles.claude]
kind = "claude"
transport = "local_cli"
command = "claude -p --verbose --dangerously-skip-permissions"
use_tmux = false
interaction_mode = "interactive"
fetch_models = true
models_command = "claude models --json"
```

Wraps the Claude CLI through the local process transport. Defaults model discovery to
`claude models --json`. Set `use_tmux = true` to run inside tmux for manual attach.

### OpenAI-Compatible HTTP

```toml
[agents.profiles.openai]
kind = "openai"
transport = "openai_chat"
model = "gpt-5.1"
api_key = "$OPENAI_API_KEY"
fetch_models = true
```

Uses streamed `/chat/completions` requests. Works with any OpenAI-compatible API by
setting `kind` and optionally `base_url`.

### OpenRouter

```toml
[agents.profiles.openrouter]
kind = "openrouter"
model = "openai/gpt-5-mini"
api_key = "$OPENROUTER_API_KEY"
fetch_models = true
```

Transport and base URL are inferred automatically from `kind = "openrouter"`.

### Kimi / Moonshot

```toml
[agents.profiles.kimi]
kind = "kimi"
model = "kimi-2.5"
api_key = "$KIMI_API_KEY"
fetch_models = true
```

### GitHub Copilot

```toml
[agents.profiles.copilot]
kind = "github-copilot"
transport = "local_cli"
command = "copilot"
use_tmux = false
fetch_models = false
```

### Pi (RPC)

```toml
[agents.profiles.pi]
kind = "pi"
transport = "rpc"
command = "pi"
model = "anthropic/claude-sonnet-4-5"
```

### ACP Agent

```toml
[agents.profiles.acp_agent]
kind = "custom"
transport = "acp"
command = "your-acp-agent"
env = { ACP_TOKEN = "$ACP_TOKEN" }
```

### ACPX Bridge

```toml
[agents.profiles.claude_acpx]
kind = "claude"
transport = "acpx"
command = "acpx"
```

## Multi-Provider Example

A complete `~/.config/polyphony/config.toml` with multiple providers, fallback chains,
and routing:

```toml
[agents]
default = "codex"

[agents.by_state]
todo = "claude"

[agents.by_label]
risky = "claude"

[agents.profiles.codex]
kind = "codex"
transport = "app_server"
command = "codex app-server"
fallbacks = ["kimi_fast"]
approval_policy = "auto"

[agents.profiles.kimi_fast]
kind = "kimi"
model = "kimi-2.5"
api_key = "$KIMI_API_KEY"

[agents.profiles.claude]
kind = "claude"
transport = "local_cli"
command = "claude"
use_tmux = true
interaction_mode = "interactive"

[agents.profiles.openai]
kind = "openai"
model = "gpt-5.1"
api_key = "$OPENAI_API_KEY"
```

## TUI Interaction

The TUI provides two modals for agent control:

- **Agent picker** (`D` on an inbox item) — choose which profile to dispatch an issue to.
  Lists all configured profiles with their kind, description, and source indicator:
  - `⌂` — defined in `~/.config/polyphony/agents/` (user-global)
  - `⊙` — defined in `.polyphony/agents/` (repository)
  - no indicator — defined in TOML config only
- **Dispatch mode** (`m`) — set the system-wide orchestration mode:
  - **Manual** — you choose which issues to dispatch
  - **Automatic** — issues are dispatched automatically
  - **Nightshift** — auto + code improvements when idle
  - **Idle** — opportunistic dispatch when idle and budgets allow
  - **Stop** — abort all running agents and pause dispatching

## Transport Architecture

Each transport maps to a provider runtime crate:

| Transport | Crate | Protocol |
|-----------|-------|----------|
| `app_server` | `polyphony-agent-codex` | Codex app-server JSON over stdio |
| `local_cli` | `polyphony-agent-local` | One-shot or interactive CLI execution |
| `openai_chat` | `polyphony-agent-openai` | Streamed HTTP `/chat/completions` |
| `rpc` | (built-in) | Pi native RPC |
| `acp` | (built-in) | Stdio ACP |
| `acpx` | (built-in) | ACPX bridge |

`polyphony-agent-claude` and `polyphony-agent-copilot` are thin wrappers around
`polyphony-agent-local` that add provider-specific model discovery and kind matching.

All runtimes are registered behind feature flags in `polyphony-agents` and selected at
runtime through `AgentRegistryRuntime`.
