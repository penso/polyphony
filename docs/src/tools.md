# Built-In Tools

Polyphony can expose a small built-in tool registry to LLM runtimes that support tool or function
calling. This is intentionally narrow. The orchestrator remains a scheduler, while provider
runtimes execute tool calls against a shared allowlisted registry.

## Current Scope

Today the registry ships these built-in tools:

- `workspace_list_files`, to inspect workspace structure
- `workspace_read_file`, to read UTF-8 text files from the workspace
- `workspace_search`, to run bounded substring search across UTF-8 workspace files
- `issue_update`, to update tracker issue fields
- `issue_comment`, to post tracker issue comments
- `pr_comment`, to post pull request summary comments through the configured PR commenter
- `linear_graphql`, which executes raw GraphQL queries or mutations against the configured Linear
  endpoint using the same auth Polyphony already uses for tracker access

This is meant for high-leverage tracker operations that belong in the runtime layer, not for
general shell or browser automation.

## Configuration

Enable tools in `WORKFLOW.md` front matter or a merged TOML config layer:

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
  deny: []
  by_agent:
    reviewer:
      allow:
        - workspace_read_file
        - workspace_search
        - pr_comment
        - linear_graphql
```

Policy fields are:

- `tools.enabled`: turns the registry on
- `tools.allow`: global allowlist, supports exact names plus `*` suffix wildcards
- `tools.deny`: global denylist, evaluated before allow rules
- `tools.by_agent.<name>.allow`: per-agent allow override
- `tools.by_agent.<name>.deny`: per-agent deny additions

Tool names are normalized to lowercase during config resolution.

## Runtime Flow

The feature is split across three layers:

1. `polyphony-core` defines tool specs, requests, results, and tool-related agent events.
2. `polyphony-tools` builds the built-in registry and applies workflow policy.
3. Provider runtimes advertise the visible tools for the selected agent and execute tool calls.

Right now the Codex app-server runtime and the OpenAI-compatible chat runtime both use this path.

## Current Design Constraints

- Tool execution does not live in `polyphony-orchestrator`.
- Tools are built-in Rust integrations, not prompt-defined shell aliases.
- The initial policy surface is intentionally small.
- Unsupported tools fail closed.

## Current Fit

The best tools for Polyphony are the ones that already map to trusted integrations or common issue
workflow mutations:

- tracker reads and writes
- PR review and comment submission
- structured issue transitions
- narrowly scoped repo metadata queries

That keeps the runtime boundary clean and avoids turning Polyphony into a generic agent sandbox.

## Good Next Candidates

If you want this feature to help the whole orchestrator, add tools in roughly this order:

1. `pr_review_sync`, for inline GitHub review submission that matches the existing review flow
2. `github_graphql`, as the GitHub-side escape hatch matching `linear_graphql`
3. `workspace_apply_patch`, for controlled file writes once the read path feels solid
4. `issue_create`, for creating follow-up issues from the same runtime path
5. `pull_request_create`, for structured PR handoff without reaching for generic shell tools

Only add `workspace_apply_patch` or broader file-write tools after the read path and tracker or PR
mutations feel solid. Once you hand an HTTP model write access to the repo, the blast radius stops
being theoretical and starts being your afternoon.
