# polyphony-feedback

`polyphony-feedback` owns outbound human-feedback delivery.

## Responsibility

It provides:

- a small feedback registry that fans out one notification to many sinks
- channel descriptors with capability metadata
- Telegram delivery using Bot API `sendMessage`
- generic webhook delivery for external relays, bots, or audit systems

## Why it exists

The orchestration runtime now has a post-run handoff stage. That stage should not
know whether a human sees the result in the TUI, Telegram, Slack, Discord, a
webhook worker, or something else later.

`polyphony-feedback` keeps that boundary explicit so adding sink `N+1` stays
adapter work instead of orchestrator surgery.
