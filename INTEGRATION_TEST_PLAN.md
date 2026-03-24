# Polyphony Integration Test Plan

## Goal

Add full end-to-end integration coverage for Polyphony using a real temporary git repository, a real Beads tracker, and the real `polyphony` binary. The harness should be deterministic, isolated from the developer machine, and able to validate both headless runtime behavior and a narrow set of TUI interactions.

## Non-goals

- Do not depend on live LLM providers.
- Do not make TUI PTY screen-scraping the primary testing strategy.
- Do not start with one giant scenario that tries to cover everything at once.

## Test strategy

Use two layers:

1. Headless daemon/data integration tests as the main coverage layer.
2. A small number of TUI smoke tests for real keyboard-driven behavior.

The headless path is the stable surface already exposed by the CLI:

- `polyphony daemon run|start|stop|refresh|mode|dispatch|approve`
- `polyphony data snapshot|issues|triggers|running|history|tasks|workspaces|events`

That gives broad runtime coverage without the flakiness of full-screen terminal scraping.

## Why this shape fits the codebase

- CLI startup already cleanly separates TUI vs headless flow in `crates/cli/src/main.rs`.
- Headless operator behavior is centralized in `crates/cli/src/tracing_support.rs`.
- The daemon control surface is already explicit in `crates/cli/src/daemon.rs`.
- The TUI is a thin interactive layer over the same runtime snapshot stream in `crates/tui/src/event_loop.rs`.
- Beads is already supported as a real tracker, but it shells out to `bd`, so tests must account for that.
- Local CLI agents already run under PTY/tmux via `portable-pty`, which gives us a deterministic fake-agent path without live providers.

## Repo layout to add

Primary location:

- `crates/cli/tests/`

Suggested structure:

- `crates/cli/tests/test_support/mod.rs`
- `crates/cli/tests/test_support/repo.rs`
- `crates/cli/tests/test_support/process.rs`
- `crates/cli/tests/test_support/json.rs`
- `crates/cli/tests/test_support/fixtures.rs`
- `crates/cli/tests/e2e_headless.rs`
- `crates/cli/tests/e2e_daemon_control.rs`
- `crates/cli/tests/e2e_tui_smoke.rs`

Fixture scripts created by tests inside the temp repo:

- `.polyphony-fixtures/agent-success.sh`
- `.polyphony-fixtures/agent-fail.sh`
- `.polyphony-fixtures/agent-stall.sh`
- `.polyphony-fixtures/agent-write-file.sh`

## Harness requirements

### Temp repo bootstrap

Each test should create its own isolated temp directory and:

1. Run `git init`.
2. Configure local test identity:
   - `git config user.name "Polyphony Test"`
   - `git config user.email "polyphony-test@example.com"`
3. Create an initial commit so worktree and clone flows have a valid git base.
4. Run `bd init --quiet`.
5. Write `WORKFLOW.md`.
6. Write `polyphony.toml`.
7. Create a temp home/config directory and override:
   - `HOME`
   - `XDG_CONFIG_HOME`
   - any Polyphony env needed for isolation

### Workflow config

Write a minimal deterministic workflow:

- `tracker.kind = beads`
- short polling interval
- explicit workspace root inside the temp repo
- explicit agent profile using `transport = local_cli`
- low timeouts to keep failures fast

Use a local CLI profile instead of real Codex/Claude/OpenAI. The fake agent command should be a script under the temp repo so the test fully controls behavior.

### Agent fixture behavior

The scripted local agent should support multiple modes through env vars or per-script files:

- succeed and exit 0
- fail and exit non-zero
- sleep long enough to trigger timeout or stall logic
- write files into the workspace
- emit recognizable output for assertions
- optionally print a completion sentinel for fast success

The point is to exercise orchestrator behavior, not model behavior.

## Phased implementation

## Phase 1: Test support crate code

Build reusable helpers for:

- creating temp repos
- writing workflow/config fixtures
- running `bd` commands
- creating issues
- spawning `polyphony`
- polling JSON output until a condition becomes true
- capturing stdout, stderr, exit status, and logs

Recommended helper API shape:

- `TestRepo::new()`
- `TestRepo::write_workflow(...)`
- `TestRepo::write_repo_config(...)`
- `TestRepo::create_beads_issue(...)`
- `PolyphonyProcess::start_daemon(...)`
- `PolyphonyProcess::run_no_tui(...)`
- `wait_for_snapshot(...)`
- `run_polyphony_json(...)`

## Phase 2: Headless e2e coverage first

Add deterministic tests that do not require the TUI.

### Initial scenarios

1. `beads_issues_appear_in_data_issues`
   - bootstrap temp repo with Beads
   - create issues with `bd`
   - run daemon
   - assert `polyphony data issues` returns visible issues

2. `manual_dispatch_runs_local_agent_and_records_history`
   - create an open issue
   - start daemon
   - dispatch issue manually
   - assert workspace exists
   - assert running/history/events reflect the run
   - assert fake agent side effects exist in workspace

3. `automatic_dispatch_picks_up_ready_issue`
   - set automatic mode
   - create ready issue
   - wait for dispatch
   - assert it runs without manual intervention

4. `failed_agent_run_is_visible_in_history`
   - fake agent exits non-zero
   - assert retry/history/error state is represented

5. `stall_or_timeout_is_visible`
   - fake agent hangs
   - assert timeout or stalled outcome is captured

6. `issue_cli_round_trips_against_beads`
   - `polyphony issue create`
   - `polyphony issue show`
   - `polyphony issue update`
   - assert Beads state and CLI JSON agree

7. `daemon_refresh_and_mode_commands_work`
   - start daemon
   - send `refresh`
   - send `mode automatic|manual`
   - assert tracker metadata in snapshot changes as expected

8. `daemon_shutdown_is_clean`
   - stop daemon
   - assert process exits and control socket is no longer active

## Phase 3: Expand scenario matrix

Once the core harness is stable, add coverage for:

- blocked issues not dispatching
- terminal issues being ignored
- multiple issues in priority order
- workspace reuse behavior
- transient path cleanup
- linked worktree and discrete clone modes
- supplemental Beads tracker behavior when primary tracker is not Beads

This phase should only happen after the basic harness is reliable.

## Phase 4: TUI testability refactor

Before adding real TUI integration coverage, improve test seams in the TUI loop.

Target refactor:

- extract event source behind a small trait or injected stream
- extract terminal setup/teardown behind a small adapter
- keep render assertions on `ratatui::TestBackend`
- keep key-routing tests outside a real PTY where possible

That should make it possible to test:

- initial refresh behavior
- tab switching
- modal open/close
- dispatch key flow
- quit flow

without depending on brittle alternate-screen scraping for every test.

## Phase 5: Minimal PTY-backed TUI smoke tests

After the refactor, add only a few real PTY smoke tests:

1. `tui_starts_and_renders_issue_list`
2. `tui_can_quit_cleanly`
3. `tui_can_open_detail_modal_for_selected_row`

Do not try to validate every widget in a PTY test. Use PTY tests only to prove that the compiled binary and terminal control path work end-to-end.

## PTY tooling choice

Preferred order:

1. Use existing `portable-pty` first.
2. Parse captured output with `vt100` where needed.
3. Only add an expect-style helper crate if the local wrapper becomes too painful.

Reasoning:

- `portable-pty` is already in the workspace.
- Polyphony already uses it in the local CLI runtime.
- Reusing the existing PTY dependency is lower risk than introducing another one immediately.

If a higher-level interaction layer is needed later, `expectrl` is the first extra crate to evaluate.

## CI requirements

These tests need explicit CI support for Beads.

### Required changes

1. Install `bd` in CI before running the new test target.
2. Keep the tests isolated from user config and host state.
3. Run the integration test target in a separate job if runtime becomes noticeable.

### Likely rollout

Start by gating the new integration tests behind a dedicated `cargo test -p polyphony-cli --test ...` job. Once stable, decide whether to fold them into the main test job.

## Acceptance criteria

The work is done when:

1. A temp repo with `.git` and `.beads` can be created from the harness with no manual setup.
2. The real `polyphony` binary can run against that repo in CI.
3. Headless tests cover issue ingestion, dispatch, runtime state, success, and failure paths.
4. No test depends on a real external model provider.
5. TUI coverage exists, but only a small amount depends on a real PTY.
6. The suite is deterministic enough to run repeatedly without timing roulette.

## Nice-to-have follow-ups

- Snapshot assertion helpers that print useful diffs on failure.
- A tiny fixture DSL for building Beads issues with dependencies.
- A helper that dumps daemon logs automatically on test failure.
- A dedicated test-only fake local agent binary if shell scripts become messy.

## Suggested execution order for the branch

1. Add `test_support` helpers.
2. Add fake local-agent fixture scripts.
3. Add headless Beads + daemon e2e tests.
4. Wire CI installation for `bd`.
5. Refactor TUI loop for injectable events/backend.
6. Add non-PTY TUI interaction tests.
7. Add one to three PTY smoke tests.

## Notes

- Keep tests focused. A failing integration suite that takes forever to debug is just performance art.
- Prefer polling snapshot state over sleeping fixed durations.
- When a test fails, dump the most recent daemon logs and runtime snapshot automatically.
