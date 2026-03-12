# polyphony-linear

`polyphony-linear` is the Linear tracker adapter.

## Responsibility

It implements the `IssueTracker` trait using:

- checked-in GraphQL schema files
- typed queries via `graphql_client`
- HTTP requests through `reqwest`

## Runtime role

When the CLI is built with the `linear` feature and `tracker.kind: linear` is selected in
`WORKFLOW.md`, this crate becomes the issue source for candidate dispatch, issue refresh, and
state reconciliation.
