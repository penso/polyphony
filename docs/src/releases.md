# Releases

Polyphony release tags follow Arbor's date-based scheme:

- `YYYYMMDD.NN`
- example: `20260315.01`

The release workflow lives in `.github/workflows/release.yml`. Pushing a matching tag runs:

1. format, lint, test, and docs checks
2. changelog and release-note generation via `git-cliff`
3. packaged release archives for macOS, Linux, and Windows
4. GitHub Release publishing with bundled archives, `CHANGELOG.md`, and `SHA256SUMS.txt`
5. a Homebrew tap update for macOS when `HOMEBREW_TAP_TOKEN` is configured

## Tagging

Preview the generated release section locally:

```bash
just changelog-release 20260315.01
```

Create the next local release tag:

```bash
just release
```

Create and push the next release tag:

```bash
just release-push
```

## Release Packages

The workflow publishes these artifacts to GitHub Releases:

- `polyphony-<tag>-universal2-apple-darwin.tar.gz`
- `polyphony-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `polyphony-<tag>-aarch64-unknown-linux-gnu.tar.gz`
- `polyphony-<tag>-x86_64-pc-windows-msvc.zip`
- `CHANGELOG.md`
- `SHA256SUMS.txt`

Each archive contains:

- the `polyphony` binary in `bin/`
- `README.md`
- `LICENSE`
- the generated `CHANGELOG.md`

## Homebrew

The release workflow renders `homebrew/Formula/polyphony.rb` and pushes it to the tap repository
configured by `HOMEBREW_TAP_REPOSITORY`. The default is:

```text
penso/homebrew-polyphony
```

To enable automatic tap updates, add this repository secret:

- `HOMEBREW_TAP_TOKEN`, a token with push access to the tap repository

Without that secret, releases still publish GitHub assets and skip the Homebrew update step.
