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

## Before Committing / Pushing

Always run these checks before committing and fix any issues:

1. `just format` — auto-fix formatting
2. `just lint` — must pass with zero warnings (`-D warnings`)
3. `just test` — all tests must pass

If any check fails, fix the issue, then commit the fix.
Never skip `just format` or `just lint` before a commit, even for small changes.
New features are not complete without test coverage. Whenever you add a feature, add or update tests that exercise the new behavior.

## Git Workflow

Conventional commits: `feat|fix|docs|style|refactor|test|chore(scope): description`
**No `Co-Authored-By` trailers.** Update `README.md` features list with `feat` commits.

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
