# polyphony-core

`polyphony-core` is the shared language of the system.

## Responsibility

This crate defines the domain types and trait contracts that the rest of the workspace uses:

- issue models such as `Issue`, `IssueComment`, and `IssueAuthor`
- agent models such as `AgentDefinition`, `AgentRunSpec`, and `AgentRunResult`
- runtime snapshot types such as `RuntimeSnapshot`, `RunningRow`, and `RetryRow`
- rate-limit, budget, and persistence records
- trait seams for `IssueTracker`, `AgentRuntime`, `WorkspaceProvisioner`, `PullRequestCommenter`,
  and `StateStore`

## Why it matters

The rest of the workspace depends on this crate to stay decoupled. Trackers, persistence adapters,
workspace provisioners, and runtimes can change independently as long as they keep satisfying these
contracts.

## Notable types

- `AgentTransport` selects `mock`, `app_server`, `local_cli`, or `openai_chat`
- `AgentInteractionMode` and `AgentPromptMode` let local and provider runtimes describe how they
  expect to be driven
- `RateLimitSignal` is used to convert provider `429`-style responses into orchestrator throttles
