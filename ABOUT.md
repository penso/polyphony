# Polyphony

Polyphony is a long-running daemon that automates coding agent orchestration for a single repository.

## What it does

1. **Polls an issue tracker** (GitHub Issues, Linear, or mock) on a configurable cadence
2. **Provisions isolated workspaces** (git worktrees, clones, or plain directories) per issue
3. **Dispatches AI coding agents** (Codex, Claude CLI, Copilot CLI, OpenAI-compatible) to work each issue inside its workspace
4. **Orchestrates the full lifecycle** — retries with backoff, fallback agent chains, rate-limit throttling, budget tracking, saved context handoff between attempts
5. **Renders live state** in a ratatui TUI dashboard

## Workflow policy

The workflow policy lives in a repo-owned `WORKFLOW.md` with YAML front matter for config and a Handlebars prompt template for the agent. Configuration merges in layers: global user config → workflow front matter → repo-local overrides → environment variables.

## Runtime snapshot model

The core data exposed to the TUI is a `RuntimeSnapshot` containing:

- **Visible issues** — id, title, state, priority, labels from the tracker
- **Running agents** — issue, agent name, model, turn count, tokens, timing
- **Retry queue** — issue, attempt number, backoff due time, error
- **Budgets** — credits remaining, spend, limits per provider
- **Agent model catalogs** — discovered models per agent profile
- **Throttle windows** — rate-limit cooldowns
- **Recent events** — scoped by workflow, dispatch, agent, tracker, retry, etc.
- **Cadence info** — last poll times, polling intervals
- **Loading state** — flags for fetching issues/budgets/models, reconciling

## Architecture

The codebase is split into focused subcrates so the orchestration loop stays independent from tracker, agent, persistence, and UI implementations. Trait seams define boundaries between components: `IssueTracker`, `AgentRuntime`, `WorkspaceProvisioner`, `StateStore`, `FeedbackSink`, and others.

Think of it as a self-hosted CI-like runner, but instead of build steps it runs LLM agents against issue tracker tickets, with full observability.

## Vision

### Issues → Tasks → Agents

Issues are one source of work, but users can also manually queue work. When an issue is picked up, it gets decomposed into one or more **tasks**. For example, an issue might produce tasks for research, coding, testing, and review. Each task has a typed category (research, coding, testing, documentation, review, etc.) and is assigned to a specific agent profile — one task might use `opus-4.6` as a coding agent while another uses `kimi2.5` as a documentation researcher. Each agent profile carries its own provider, model, system prompt, and role.

The TUI should give a clear view of tasks: how many exist, their completion status, which agent is working each one, and how they roll up to the parent issue.

### Runs (issue → workspace → deliverable)

In music, a symphony's runs are self-contained sections that form a larger whole. In Polyphony, a **run** is the lifecycle of a single issue: from intake through task decomposition, workspace provisioning, agent execution, to a delivered artifact. That artifact is typically a GitHub pull request or GitLab merge request, but the output target is modular. Runs have their own status lifecycle, independent of any single provider or tracker.

### Automatic dispatch

The dispatcher that converts issues into tasks should support automatic mode, driven by criteria such as:

- Issue priority or urgency labels
- Available token credits across providers — if a provider has remaining budget, use it to make progress on queued issues
- Configurable policies (e.g., always auto-dispatch P0/P1, require manual approval for P3+)

This lets Polyphony opportunistically consume spare capacity rather than sitting idle when credits are available.
