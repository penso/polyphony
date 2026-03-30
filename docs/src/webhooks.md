# Webhooks

Polyphony supports inbound webhooks from GitHub, GitLab, Linear, and custom services. When a webhook fires, Polyphony immediately re-polls the tracker instead of waiting for the next polling interval — giving near-instant response to new issues, PR events, and comments.

## How it works

Without webhooks, Polyphony polls your tracker on a configurable interval (default: every few minutes). With webhooks, your tracker pushes events to Polyphony the moment something happens — a new issue, a PR comment, a label change. Polyphony verifies the webhook signature and triggers an immediate refresh.

```
GitHub/GitLab/Linear → POST /webhooks/{provider} → Polyphony → refresh tracker → dispatch agents
```

## Supported providers

| Provider | Auth Strategy | Signature Header | Events |
|----------|--------------|-----------------|--------|
| GitHub | `hmac_sha256` | `X-Hub-Signature-256` | Issues, PR, PR review, comments |
| GitLab | `token_header` | `X-Gitlab-Token` | Issues, merge requests, notes |
| Linear | `hmac_sha256_header` | `Linear-Signature` | Issues, comments, labels |
| Custom | `bearer` | `Authorization: Bearer` | Any |

## Quick setup

The easiest way to configure webhooks is the auto-provisioning CLI:

```bash
polyphony webhook setup --url https://your-public-url.com
```

This command:
1. Detects your configured tracker (GitHub, GitLab, or Linear)
2. Generates a cryptographic secret
3. Creates the webhook on the tracker via API
4. Prints the TOML configuration to add to your config

## Manual configuration

Add to your `config.toml` or `WORKFLOW.md` front matter:

```toml
[daemon.webhooks]
enabled = true

[daemon.webhooks.providers.github]
auth = "hmac_sha256"
secret = "your-shared-secret"
```

Then configure the matching webhook in your tracker's settings, pointing to:

```
POST https://your-polyphony-host:8080/webhooks/github
```

### GitHub

1. Go to your repo → Settings → Webhooks → Add webhook
2. Payload URL: `https://your-host:8080/webhooks/github`
3. Content type: `application/json`
4. Secret: same value as `secret` in your config
5. Events: select "Issues", "Issue comments", "Pull requests", "Pull request reviews", "Pull request review comments"

```toml
[daemon.webhooks.providers.github]
auth = "hmac_sha256"
secret = "$GITHUB_WEBHOOK_SECRET"
```

### GitLab

1. Go to your project → Settings → Webhooks
2. URL: `https://your-host:8080/webhooks/gitlab`
3. Secret token: same value as `secret` in your config
4. Triggers: check "Issues events", "Merge request events", "Note events"

```toml
[daemon.webhooks.providers.gitlab]
auth = "token_header"
secret = "$GITLAB_WEBHOOK_SECRET"
header = "X-Gitlab-Token"
```

### Linear

1. Go to Linear → Settings → API → Webhooks → New webhook
2. URL: `https://your-host:8080/webhooks/linear`
3. Copy the signing secret
4. Resource types: Issue, Comment, IssueLabel

```toml
[daemon.webhooks.providers.linear]
auth = "hmac_sha256_header"
secret = "$LINEAR_WEBHOOK_SECRET"
header = "Linear-Signature"
```

### Custom / generic

For any service that sends a bearer token:

```toml
[daemon.webhooks.providers.my-service]
auth = "bearer"
secret = "$MY_SERVICE_TOKEN"
```

The service should send: `Authorization: Bearer <token>`

## Auth strategies

| Strategy | How it works | Used by |
|----------|-------------|---------|
| `hmac_sha256` | Computes HMAC-SHA256 over the request body, compares with `sha256=<hex>` in the signature header | GitHub |
| `hmac_sha256_header` | Same HMAC but the header contains raw hex (no `sha256=` prefix) | Linear |
| `token_header` | Direct constant-time comparison of the header value with the secret | GitLab |
| `bearer` | Extracts token from `Authorization: Bearer <token>` and compares | Generic |

All comparisons use constant-time algorithms to prevent timing attacks.

## Exposing webhooks publicly

Polyphony's webhook endpoint must be reachable from the internet for your tracker to send events. Options:

- **Reverse proxy**: nginx, Caddy, or Cloudflare Tunnel in front of Polyphony
- **Tailscale Funnel**: expose a local port via Tailscale
- **ngrok**: `ngrok http 8080` for development
- **Direct**: if your server has a public IP, just use that

## Troubleshooting

**Webhook not triggering refresh:**
- Check that `daemon.webhooks.enabled = true` in your config
- Verify the provider name in the URL matches your config (e.g. `/webhooks/github`)
- Check the Polyphony logs for authentication errors
- Ensure the secret matches exactly between your tracker and config

**401 Unauthorized:**
- The HMAC signature or token doesn't match — check the secret value
- For GitHub, ensure the header is `X-Hub-Signature-256` (not the older SHA-1 header)

**404 Not Found:**
- The provider name in the URL doesn't match any configured provider
- Webhook routes are only available when `daemon.webhooks.enabled = true`
