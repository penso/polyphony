# polyphony-issue-mock

`polyphony-issue-mock` provides the local development path.

## Responsibility

This crate ships:

- a seeded mock tracker
- a mock agent runtime

## Why it matters

The default repository `WORKFLOW.md` uses this crate so the application can start immediately
without external credentials, trackers, or provider APIs. It is also used heavily by tests and
local smoke runs.
