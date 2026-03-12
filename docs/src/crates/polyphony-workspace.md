# polyphony-workspace

`polyphony-workspace` owns safe workspace lifecycle management above the lower-level provisioner.

## Responsibility

`WorkspaceManager` handles:

- sanitized workspace path mapping
- containment checks so workspaces cannot escape the configured root
- `after_create`, `before_run`, `after_run`, and `before_remove` hooks
- transient artifact cleanup before runs
- rollback when workspace initialization fails

## Relationship to polyphony-git

This crate does not create linked worktrees or clones itself. Instead, it builds a
`WorkspaceRequest` and delegates the physical checkout lifecycle to a `WorkspaceProvisioner`
implementation such as `polyphony-git`.
