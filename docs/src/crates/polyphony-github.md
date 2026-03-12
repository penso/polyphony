# polyphony-github

`polyphony-github` is the GitHub integration crate.

## Responsibility

It provides:

- an `IssueTracker` implementation for GitHub Issues
- pull request comment support through GitHub GraphQL mutations
- best-effort GitHub Project item creation and status field updates

## Implementation notes

The crate combines:

- `octocrab` for GitHub REST issue reads
- `graphql_client` for typed GraphQL operations
- explicit translation of GitHub `403` and `429` responses into runtime throttles

This crate is feature-gated behind `github` in the root CLI.
