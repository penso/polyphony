# polyphony-httpd

`polyphony-httpd` provides a web-based interface for the Polyphony runtime, complementing the terminal dashboard.

## Responsibility

It consumes `RuntimeSnapshot` values and exposes them through two interfaces:

- **SSR pages** — server-rendered HTML using Jinja2 templates (via `minijinja`), served by `axum`
- **GraphQL API** — full query, mutation, and subscription support via `async-graphql`

## SSR Pages

| Route | Description |
|-------|-------------|
| `/` | Dashboard with counts, status, and summaries |
| `/triggers` | Pending triggers table |
| `/movements` | Movements with status and deliverables |
| `/agents` | Running agents and execution history |
| `/tasks` | Task breakdown across movements |
| `/logs` | Reverse-chronological runtime events |

Templates live in `crates/httpd/templates/` and extend a shared `layout.html` base. Pages auto-refresh via a WebSocket connection to the GraphQL subscription endpoint.

## GraphQL API

| Endpoint | Description |
|----------|-------------|
| `GET /graphql` | Interactive playground |
| `POST /graphql` | Query and mutation endpoint |
| `/graphql/ws` | WebSocket subscriptions |

### Queries

- `triggers` — pending trigger list
- `movements` / `movement(id)` — movement list and detail
- `tasks(movementId?)` — task list, optionally filtered by movement
- `runningAgents` — currently executing agents
- `recentEvents(limit?)` — recent runtime events
- `counts` — summary counts (running, retrying, movements, tasks, worktrees)
- `dispatchMode` — current dispatch mode

### Mutations

- `setMode(mode)` — change dispatch mode
- `dispatchIssue(issueId, agentName?, directives?)` — dispatch an issue to an agent
- `refresh` — request a tracker refresh

### Subscriptions

- `snapshotUpdated` — emits a summary on every state change
- `events` — streams new runtime events as they arrive

## Usage

The httpd feature is enabled by default. When `daemon.listen_port` is configured, the web UI starts alongside the TUI automatically. The httpd endpoint URL appears in the TUI log output at startup.

```bash
just httpd          # TUI + web UI on port 8080
just httpd 3000     # TUI + web UI on custom port
just httpd-only     # web UI only (no TUI)
```

Or set `daemon.listen_port` in your workflow configuration to always enable the web UI when running `polyphony`.

## Templates

Templates use Jinja2 syntax via `minijinja`. The full `RuntimeSnapshot` is available as the template context, so any field from the snapshot can be rendered directly. Templates are loaded from disk at startup, making customization straightforward.
