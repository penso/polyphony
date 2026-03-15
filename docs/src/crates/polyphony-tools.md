# polyphony-tools

`polyphony-tools` owns the built-in LLM tool registry used by provider runtimes.

## Responsibility

This crate sits between workflow config and provider execution:

- reads the merged workflow tool policy
- registers built-in Rust-backed tools
- filters tool visibility per agent
- executes supported tool calls through a shared `ToolExecutor`

It does not own orchestration, tracker polling, or provider transport logic.

## Current Built-Ins

- `workspace_list_files`
- `workspace_read_file`
- `workspace_search`
- `issue_update`
- `issue_comment`
- `pr_comment`
- `linear_graphql`

The crate only registers tools that make sense for the active workflow. For example,
`linear_graphql` is only exposed when the tracker is Linear and the required auth is configured,
and `pr_comment` only appears when a pull request commenter is available.

## Why It Exists

The core crate defines the common tool contracts, but it should not know which tools are
available. Provider crates know how to speak their transport, but they should not each reinvent
tool policy and registration. `polyphony-tools` keeps that boundary boring and explicit, which is
rare and therefore suspicious.
