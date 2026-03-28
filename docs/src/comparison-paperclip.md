# Polyphony vs Paperclip

A feature-level comparison between Polyphony and [Paperclip](https://paperclip.ing) (~40k GitHub stars), an open-source control plane for autonomous AI companies.

Both projects orchestrate AI coding agents around issue-driven workflows, but they differ significantly in scope, architecture, and philosophy.

## Core Identity

| | **Paperclip** | **Polyphony** |
|---|---|---|
| Pitch | Control plane for autonomous AI companies | Repo-native AI orchestration engine |
| Mental model | You're the board of directors running a company of AI employees | You're a tech lead with a WORKFLOW.md that drives agent pipelines |
| Scope | Multi-company, multi-project, org-wide | Single-repo, single-workflow |
| Interface | Web dashboard (React) | Terminal TUI (Ratatui) |
| Tech stack | TypeScript, Node, Express, React, Postgres | Rust, Tokio, Ratatui, SQLite |
| License | MIT | MIT |

## Orchestration & Pipelines

| Feature | **Paperclip** | **Polyphony** |
|---|---|---|
| Issue → agent dispatch | Heartbeat-based | Poll + claim loop |
| Multi-stage pipelines | Flat task lifecycle (backlog → done) | Planning → Executing → Completing with task decomposition |
| Task decomposition | Agent breaks down goals manually | Router agent auto-decomposes issues into parallel tasks |
| Agent routing/fallback | Single assignee per task | Workflow-defined agent chains with retry + fallback |
| Retry with backoff | Not built-in | Configurable per-agent retry backoff |
| PR review workflows | Not a focus | First-class run type (PR review, PR comment review) |
| Dispatch modes | Heartbeat + event triggers | Manual, Automatic, Nightshift, Idle |
| Hot-reload config | No (DB-driven) | Yes (WORKFLOW.md file watcher) |

## Agent Support

| Feature | **Paperclip** | **Polyphony** |
|---|---|---|
| Agent model | "Hire" agents into an org chart | Register agents in WORKFLOW.md |
| Claude | Yes (claude_local adapter) | Yes |
| Codex | Yes (codex_local adapter) | Yes |
| Copilot | No | Yes |
| Cursor | Yes (adapter) | No |
| OpenAI-compatible HTTP | Yes (http adapter) | Yes (OpenRouter, Kimi, etc.) |
| Gemini | Yes (gemini_local) | No |
| Pi (Warp) | No | Yes |
| ACP protocol | No | Yes (ACP + ACPX) |
| OpenClaw | Yes (gateway adapter) | No |
| Bring-your-own | HTTP webhook | LocalCli fallback |
| Budget caps | Monthly hard-stop with auto-pause | Real-time budget-aware throttling |
| Agent org chart | Hierarchies, roles, reporting lines | Flat agent pool |

## Issue Tracker Integrations

| Tracker | **Paperclip** | **Polyphony** |
|---|---|---|
| GitHub Issues | No native sync (work products reference PRs) | Full GraphQL integration + Project automation |
| Linear | No | Yes (GraphQL, checked-in schemas) |
| GitLab | No | Yes (issues + merge requests) |
| Jira | No | No |
| Built-in tracker | Full internal ticket system | Beads (Dolt-backed local tracker) |
| Mock/test tracker | No | Yes |

## Workspace & Execution

| Feature | **Paperclip** | **Polyphony** |
|---|---|---|
| Workspace isolation | Control plane only; agents handle their own | Three strategies: directory, linked worktree, discrete clone |
| Git integration | Agents manage git themselves | Built-in git2 (branch creation, commits, pushes) |
| PR creation | Agents create PRs via their own tooling | Orchestrator creates PRs via octocrab |
| Workspace hooks | No | Pre/post init and cleanup hooks |
| Workspace reuse | N/A | Configurable with optional sync |

## Governance & Visibility

| Feature | **Paperclip** | **Polyphony** |
|---|---|---|
| Approval gates | Board approval for hires, strategy | No formal approval gates |
| Audit log | Immutable activity log | Event stream in TUI |
| Agent pause/resume/terminate | Manual board overrides | TUI controls |
| Live agent output | Web dashboard | Streaming terminal logs with braille spinners |
| Cost visibility | Per-agent, per-task, per-project | Per-agent budget snapshots in TUI |
| Feedback routing | No | Telegram + webhook channels |
| Multi-company isolation | Complete data isolation | Single-repo scope |

## Configuration

| Feature | **Paperclip** | **Polyphony** |
|---|---|---|
| Config lives in | Database + UI forms | Checked-in WORKFLOW.md (YAML frontmatter) |
| Prompt templating | Agent instructions in DB | Liquid templates with `{{ issue.* }}` variables |
| Layered config | Single DB source | 5 layers: defaults → user global → repo-local → WORKFLOW.md → env vars |
| Portable export | Company template export/import with secret scrubbing | Config is already in the repo (git-native) |

## Deployment

| Feature | **Paperclip** | **Polyphony** |
|---|---|---|
| Install | `npx paperclipai onboard --yes` | Rust binary (cargo install / release binary) |
| Database | Embedded Postgres or external | Optional SQLite |
| Web UI | React SPA | TUI only |
| Mobile access | Responsive web | No |
| Auth | Better Auth (sessions + API keys) | N/A (local process) |
| Multi-user | Coming soon | Single-user |

## Storytelling & Positioning

| Dimension | **Paperclip** | **Polyphony** |
|---|---|---|
| Narrative | "You're running an autonomous company" | "Your repo drives the workflow" |
| Metaphor | Corporate: CEO, board, hires, org chart, budgets | Orchestra: orchestrator, runs, pipelines |
| Emotional hook | Aspiration — "build a company, not use a tool" | Competence — "your workflow, codified and reliable" |
| Fear addressed | "You'll lose control" → "You're the board" | "Agents are unreliable" → "Retry, fallback, budget-aware" |
| Tone | Confident, aspirational, startup-energy | Engineering-focused, precision-oriented |
| Social proof | 40k stars, named testimonials, Discord community | Developer-facing, no marketing site |

Paperclip sells identity transformation ("you're not using a tool, you're running a company"). The landing page follows a classic aspiration arc — from chaos (20 Claude Code tabs) to control (you're the board). They explicitly list what they're NOT (not a chatbot, not a framework, not single-agent) to preempt dismissal. The `npx onboard` one-liner removes activation energy.

Polyphony sells engineering reliability — multi-stage pipelines, retry chains, workspace isolation, repo-native config. The story is implicit in the architecture rather than told on a landing page.

## Summary

**Where Paperclip leads:** storytelling, web UI, multi-company scope, governance (board approvals, immutable audit), onboarding friction, community size.

**Where Polyphony leads:** pipeline sophistication (multi-stage, task decomposition, routing, retry+fallback), tracker integrations (GitHub/Linear/GitLab vs internal-only), workspace engineering (3 isolation strategies, hooks, git2), agent provider coverage (8+ backends including ACP, Copilot, Pi), repo-native config (WORKFLOW.md, layered, hot-reloadable), PR review as a first-class workflow, performance (Rust, no web server overhead).

The core gap is narrative, not features. Paperclip's 40k stars come from storytelling and positioning. Feature-for-feature, Polyphony has deeper orchestration, broader integrations, and more execution-layer control — but the story isn't being told yet.
