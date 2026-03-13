# CLAUDE.md

This file provides instructions for Claude Code when working in this repository.

## Priorities

1. Keep code simple, explicit, and maintainable.
2. Fix root causes, avoid temporary band-aids.
3. Preserve user changes, never revert unrelated edits.

## Commands

- Format: `just format`
- Format check: `just format-check`
- Lint (clippy): `just lint`
- Test: `just test`
- Docs build: `just docs-build`
- Docs serve: `just docs-serve`

## Before Committing / Pushing

Always run these checks before committing and fix any issues:

1. `just format` — auto-fix formatting
2. `just lint` — must pass with zero warnings (`-D warnings`)
3. `just test` — all tests must pass

If any check fails, fix the issue, then commit the fix.
Never skip `just format` or `just lint` before a commit, even for small changes.
New features are not complete without test coverage. Whenever you add a feature, add or update tests that exercise the new behavior.

## Documentation

- `README.md` and `docs/` must stay in sync with the codebase.
- When code changes affect behavior, commands, configuration, crate layout, features, or architecture, update the relevant documentation in the same change.
- Documentation drift is a bug. Do not defer README or mdBook updates for later follow-up.

## Git Workflow

Conventional commits: `feat|fix|docs|style|refactor|test|chore(scope): description`
**No `Co-Authored-By` trailers.** Update `README.md` features list with `feat` commits, and update `docs/` when the feature changes user-facing or architectural behavior.

## Rust Rules

- Do not use `unwrap()` or `expect()` in non-test code. In test modules, use `#[allow(clippy::unwrap_used, clippy::expect_used)]` on the module.
- Use clear error handling with typed errors (`thiserror`/`anyhow` where appropriate).
- Use `arbor_core::ResultExt` and `arbor_core::OptionExt` for `.context()` on `Result<T, E>` and `Option<T>` — prefer these over ad-hoc `.map_err()` with format strings.
- Use `SessionId` and `WorkspaceId` newtypes from `arbor_core::id` instead of raw `String` for session/workspace identifiers. These are `#[serde(transparent)]` for wire compatibility.
- Leverage Rust's type system directly: prefer typed structs and semantic helper methods over accessor-object wrappers or blanket getter layers.
- Remove any other accessor object patterns that do not buy real semantics. If a method only mirrors a field, prefer direct field access; add methods only for derived behavior, invariants, or domain meaning.
- Keep modules focused and delete dead code instead of leaving it around.
- Collapse nested `if` / `if let` statements when possible (clippy `collapsible_if`).
- **Never shell out to external CLIs** (`gh`, `git` via `Command::new`, etc.) for GitHub API calls or operations that can be done with Rust crates. Use `octocrab`, `reqwest`, or other Rust HTTP/API crates instead. The only acceptable use of `std::process::Command` is where no Rust crate equivalent exists.

## Module Organization

- Split large files by domain: types, constants, helpers, actions. Keep files under ~800 lines where practical.
- Use `pub(crate)` visibility for items shared within a crate but not exported. Apply to struct fields, methods, and free functions in submodules.
- Use `pub(crate) use module::*` glob re-exports in parent modules to keep call sites clean after extraction.
- When splitting `impl` blocks across files, the struct definition stays in `types.rs` and method impls go in the relevant domain file.

## Feature Flags

Optional functionality is gated behind Cargo features:

- When adding a new optional dependency, use `dep:crate_name` syntax in the feature definition and mark the dependency as `optional = true`.
- Feature flags should be hierarchical: `mosh` implies `ssh`.

## Workspace Dependencies

All third-party dependency versions are centralized in the root `Cargo.toml` under `[workspace.dependencies]`. Crate-level `Cargo.toml` files must use `{ workspace = true }` (with optional extra keys like `features` or `optional`). Never hardcode a version in a subcrate — add it to the workspace root first.

## Git Rules

- Treat `git status` / `git diff` as read-only context.
- Do not run destructive git commands.
- Do not amend commits unless explicitly asked.
- Only create commits when the user asks.

## Changelog

- Use `git-cliff` for changelog generation (config: `cliff.toml`).
- `just changelog` / `just changelog-unreleased` / `just changelog-release <version>`

<!-- BEGIN BEADS INTEGRATION -->
## Issue Tracking with bd (beads)

**IMPORTANT**: This project uses **bd (beads)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why bd?

- Dependency-aware: Track blockers and relationships between issues
- Git-friendly: Dolt-powered version control with native sync
- Agent-optimized: JSON output, ready work detection, discovered-from links
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**

```bash
bd ready --json
```

**Create new issues:**

```bash
bd create "Issue title" --description="Detailed context" -t bug|feature|task -p 0-4 --json
bd create "Issue title" --description="What this issue is about" -p 1 --deps discovered-from:bd-123 --json
```

**Claim and update:**

```bash
bd update <id> --claim --json
bd update bd-42 --priority 1 --json
```

**Complete work:**

```bash
bd close bd-42 --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `bd ready` shows unblocked issues
2. **Claim your task atomically**: `bd update <id> --claim`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `bd create "Found bug" --description="Details about what was found" -p 1 --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "Done"`

### Auto-Sync

bd automatically syncs via Dolt:

- Each write auto-commits to Dolt history
- Use `bd dolt push`/`bd dolt pull` for remote sync
- No manual export/import needed!

### Important Rules

- ✅ Use bd for ALL task tracking
- ✅ Always use `--json` flag for programmatic use
- ✅ Link discovered work with `discovered-from` dependencies
- ✅ Check `bd ready` before asking "what should I work on?"
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use external issue trackers
- ❌ Do NOT duplicate tracking systems

For more details, see README.md and docs/QUICKSTART.md.

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt pull
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

<!-- END BEADS INTEGRATION -->
