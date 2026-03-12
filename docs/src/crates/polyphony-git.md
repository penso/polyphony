# polyphony-git

`polyphony-git` provides the default `WorkspaceProvisioner` used by the CLI.

## Responsibility

It implements git-backed workspace creation and cleanup for:

- plain directories
- linked worktrees
- discrete clones
- branch commit and push for automated review handoff

## Important behavior

The provisioner understands:

- `workspace.source_repo_path`
- `workspace.clone_url`
- `workspace.default_branch`
- `workspace.sync_on_reuse`

That logic stays separate from the orchestrator so git lifecycle concerns do not leak into
scheduling state.
