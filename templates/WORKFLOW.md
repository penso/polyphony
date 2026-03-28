---
# ─── Issue Tracker ───────────────────────────────────────────────────
# Connects Polyphony to your project tracker so it can fetch issues.
tracker:
  # Tracker backend.
  #   none   — no tracker; manual dispatch only
  #   github — GitHub Issues / Projects (requires `repository`)
  #   linear — Linear (requires `project_slug`)
  #   beads  — local beads tracker
  kind: none

  # GitHub: owner/repo to fetch issues from.
  # repository: owner/repo

  # GitHub Projects v2: org or user that owns the project board.
  # project_owner: owner-or-org

  # GitHub Projects v2: project number (visible in the project URL).
  # project_number: 7

  # GitHub Projects v2: name of the status field used to filter issues.
  # project_status_field: Status

  # Linear: team project slug (e.g. "ENG").
  # project_slug: ENG

  # Issue states considered active (will be dispatched to agents).
  # Defaults depend on the tracker kind.
  # active_states:
  #   - Todo
  #   - "In Progress"

  # Issue states considered terminal (agent will be stopped if issue
  # transitions to one of these while running).
  # terminal_states:
  #   - Done
  #   - Cancelled

  # API key for the tracker. Prefer setting this in polyphony.toml or
  # as an environment variable (GITHUB_TOKEN / LINEAR_API_KEY) to avoid
  # checking secrets into the repository.
  # api_key: ghp_...

# ─── Polling ─────────────────────────────────────────────────────────
# How often the orchestrator polls the tracker for new/changed issues.
polling:
  # Poll interval in milliseconds. Lower = more responsive, higher = fewer API calls.
  # Default: 60000 (1 minute).
  interval_ms: 60000

# ─── Workspace ───────────────────────────────────────────────────────
# Controls how Polyphony creates isolated working directories for agents.
workspace:
  # Directory (relative to repo root) where per-issue workspaces are created.
  root: .polyphony/workspaces

  # How workspaces are created.
  #   directory        — plain directory copy (default, simplest)
  #   linked_worktree  — git worktree linked to the source repo (faster, shares objects)
  #   discrete_clone   — full git clone from a remote URL
  checkout_kind: directory

  # Re-sync the workspace (pull latest base branch) when reusing an existing workspace.
  sync_on_reuse: true

  # Paths inside the workspace that are cleaned between agent runs
  # (e.g. build caches, temp dirs).
  transient_paths:
    - tmp

  # For linked_worktree: absolute path to the source repository.
  # source_repo_path: /abs/path/to/this/repo

  # For discrete_clone: clone URL (HTTPS or SSH).
  # clone_url: git@github.com:owner/repo.git

  # Base branch to check out. Defaults to the repo's default branch.
  # default_branch: main

# ─── Hooks ───────────────────────────────────────────────────────────
# Shell commands run at workspace lifecycle events. Each receives the
# workspace path as $WORKSPACE_PATH. Commands run with a timeout.
# hooks:
#   # After a new workspace is created (install deps, seed data, etc.)
#   after_create: "cd $WORKSPACE_PATH && npm install"
#
#   # Before an agent run starts.
#   before_run: ""
#
#   # After an agent run completes (regardless of success/failure).
#   after_run: ""
#
#   # After the orchestrator has processed the agent outcome (review posted, PR created, etc.)
#   after_outcome: ""
#
#   # Before a workspace directory is removed.
#   before_remove: ""
#
#   # Maximum time (ms) a hook is allowed to run before being killed.
#   # Default: 120000 (2 minutes).
#   timeout_ms: 120000

# ─── Agent Defaults ──────────────────────────────────────────────────
# Global limits for agent sessions.
agent:
  # Maximum agents running simultaneously across all issues.
  max_concurrent_agents: 3

  # Maximum turns (prompt→response cycles) per agent session before
  # the session is stopped. Prevents runaway agents.
  max_turns: 4

  # Maximum backoff delay (ms) between retry attempts after a failure.
  max_retry_backoff_ms: 60000

  # Custom prompt appended when the agent is given an additional turn.
  # continuation_prompt: "Continue working on this issue."

# ─── Tools ───────────────────────────────────────────────────────────
# Built-in tools that agents can call (file read, search, issue updates, etc.)
tools:
  # Master switch. Set to true to enable the built-in tool server.
  enabled: false

  # Global allow-list of tool names. If set, only these tools are available.
  # allow:
  #   - workspace_list_files
  #   - workspace_read_file
  #   - workspace_search
  #   - issue_update
  #   - issue_comment
  #   - linear_graphql

  # Per-agent overrides. Restrict specific agents to a subset of tools.
  # by_agent:
  #   reviewer:
  #     allow:
  #       - workspace_read_file
  #       - workspace_search
  #       - pr_comment

# ─── Orchestration ───────────────────────────────────────────────────
# Controls how work is planned and dispatched.
orchestration:
  # Name of the agent used for pipeline planning / task decomposition.
  # Must match a profile defined in `agents.profiles` or `.polyphony/agents/`.
  router_agent: router

  # Orchestration mode.
  #   advisory — orchestrator suggests plans, human confirms
  #   auto     — orchestrator plans and executes autonomously
  mode: advisory

# ─── Pipeline ────────────────────────────────────────────────────────
# Multi-step execution pipeline (router → tasks → agents).
pipeline:
  # Enable pipeline orchestration. When false, issues are dispatched
  # directly to a single agent without planning.
  enabled: true

  # Agent used for planning. Defaults to orchestration.router_agent.
  # planner_agent: router

  # Custom planning prompt (overrides the built-in planner prompt).
  # planner_prompt: "..."

  # Re-run the planner when a task fails, to adjust the remaining plan.
  # replan_on_failure: false

  # Static pipeline stages (alternative to dynamic planning).
  # When set, the planner is skipped and these stages run in order.
  # stages:
  #   - category: coding
  #     agent: implementer
  #   - category: testing
  #     agent: tester

# ─── Agent Routing ───────────────────────────────────────────────────
# Maps issues to agents. Agent profiles are defined in `.polyphony/agents/*.md`.
agents:
  # Default agent for issues that don't match any by_state/by_label rule.
  default: implementer

  # Agent used for PR reviews (review_events config and automation reviews).
  reviewer: reviewer

  # Route issues to agents based on their tracker state.
  # by_state:
  #   "In Review": reviewer
  #   "In Progress": implementer

  # Route issues to agents based on their labels.
  # by_label:
  #   bug: implementer
  #   research: researcher

# ─── Automation ──────────────────────────────────────────────────────
# Controls automatic commit, PR creation, and self-review after agent runs.
# automation:
#   # Enable automatic PR creation from agent commits.
#   enabled: false
#
#   # Create PRs as drafts instead of ready-for-review.
#   draft_pull_requests: true
#
#   # Append "Co-Authored-By: Polyphony <noreply@polyphony.to>" to commits.
#   co_authored_by: true
#
#   # Agent that reviews the automated PR before marking it ready.
#   review_agent: reviewer
#
#   # Conventional commit message template (Liquid syntax).
#   # commit_message: "feat({{ issue.identifier }}): {{ issue.title }}"
#
#   # PR title template.
#   # pr_title: "{{ issue.identifier }}: {{ issue.title }}"
#
#   # Git author for automated commits.
#   git:
#     remote_name: origin
#     author:
#       name: Polyphony Bot
#       email: bot@polyphony.dev

# ─── Review Events ──────────────────────────────────────────────────
# Automatic PR review when new pull requests are opened or updated.
# review_events:
#   pr_reviews:
#     # Enable automatic PR reviews.
#     enabled: true
#
#     # Agent to use for reviews. Defaults to agents.reviewer.
#     agent: reviewer
#
#     # Seconds to wait after the last push before starting a review
#     # (debounce rapid pushes).
#     debounce_seconds: 60
#
#     # Review draft PRs too (default: false).
#     include_drafts: false
#
#     # Only review PRs with at least one of these labels (empty = all PRs).
#     only_labels: []
#
#     # Skip PRs with any of these labels.
#     ignore_labels: [wip, "do not review"]
#
#     # Skip PRs authored by these usernames.
#     ignore_authors: []
#
#     # Skip PRs authored by bot accounts (default: false).
#     ignore_bot_authors: false
#
#     # How review comments are posted.
#     #   ""       — post as a regular PR comment (default)
#     #   inline   — post as a GitHub PR review with inline file comments
#     comment_mode: inline

# ─── Feedback ────────────────────────────────────────────────────────
# Notification channels for agent outcomes (approve/reject prompts, alerts).
# feedback:
#   # Channels offered for user interaction (shown in TUI, sent via webhook).
#   #   telegram — send via Telegram bot
#   #   webhook  — POST to a URL
#   offered: []
#
#   # Base URL for action links in notifications.
#   # action_base_url: https://polyphony.example.com
#
#   # Telegram channel configs (keyed by channel name).
#   # telegram:
#   #   alerts:
#   #     bot_token: "123:ABC..."
#   #     chat_id: "-100..."
#
#   # Webhook channel configs (keyed by channel name).
#   # webhook:
#   #   slack:
#   #     url: https://hooks.slack.com/...
#   #     bearer_token: xoxb-...
---
# Polyphony Workflow

You are operating inside an isolated per-issue workspace.

Issue: {{ issue.identifier }} - {{ issue.title }}
State: {{ issue.state }}

Execution rules:

- Stay inside the assigned workspace.
- Make progress that is observable and incremental.
- Prefer tests, logs, and explicit status over hidden work.
- Leave the issue in a non-active handoff state when work is complete.
- Let the router split complex work into specialist tasks when that helps quality or speed.

Shared workflow policy belongs in this file.
Shared credentials and reusable agent profiles belong in `~/.config/polyphony/config.toml`.
Local tracker identity, router selection, and repo wiring can live in `polyphony.toml`.
Role-specific prompts live in `.polyphony/agents/`.
