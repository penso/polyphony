# polyphony-issue-mock

`polyphony-issue-mock` provides test-only mock implementations.

## Responsibility

This crate ships:

- a seeded mock tracker
- a mock agent runtime

## Why it matters

It is used heavily by tests and internal smoke coverage, without forcing the normal CLI startup
path to render demo data.
