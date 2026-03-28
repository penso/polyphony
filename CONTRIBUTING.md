# Contributing to Polyphony

Thanks for your interest in contributing! Polyphony is in early alpha and we welcome bug reports, feature ideas, and pull requests.

## Getting Started

1. Fork and clone the repo
2. Install [Rust](https://rustup.rs/) (stable toolchain)
3. Install [just](https://github.com/casey/just) (task runner)
4. Run `cargo build` to verify everything compiles

## Development Workflow

### Commits

We use [conventional commits](https://www.conventionalcommits.org/):

```
feat|fix|docs|style|refactor|test|chore(scope): description
```

### Before Submitting a PR

```sh
just format   # rustfmt
just lint      # clippy
cargo test     # run the test suite
```

Fix any issues before pushing — this avoids wasted CI round-trips.

### Code Guidelines

- **No `unwrap()` or `expect()` in non-test code.** Use typed errors (`thiserror`/`anyhow`).
- **No shelling out** to external CLIs (`gh`, `git` via `Command::new`) for operations that Rust crates can handle. Use `octocrab`, `reqwest`, etc.
- **Tests are required** for every feature, bug fix, and behavior change.
- **Workspace dependencies** — all versions live in the root `Cargo.toml` under `[workspace.dependencies]`. Subcrates use `{ workspace = true }`.
- Keep files under ~800 lines. Split by domain when they grow.

### Changelog

Don't add manual `CHANGELOG.md` entries. The changelog is generated from commit history via `git-cliff`.

## Reporting Issues

Open a [GitHub issue](https://github.com/penso/polyphony/issues). Include steps to reproduce, expected vs actual behavior, and your OS/Rust version.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE.md).
