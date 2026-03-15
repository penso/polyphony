default:
    @just --list

nightly_toolchain := "nightly-2025-11-30"

format:
    cargo +{{nightly_toolchain}} fmt --all

format-check:
    cargo +{{nightly_toolchain}} fmt --all -- --check

lockfile-check:
    cargo metadata --locked --format-version=1 > /dev/null

lint: lockfile-check
    cargo +{{nightly_toolchain}} clippy --workspace --all-features --all-targets -- -D warnings

test:
    bash ./scripts/ci/with-sanitized-env.sh cargo +{{nightly_toolchain}} test --workspace

test-raw:
    cargo +{{nightly_toolchain}} test --workspace

install:
    mkdir -p "$HOME/.local/bin"
    cargo +{{nightly_toolchain}} build --release -p polyphony-cli
    install -m 755 target/release/polyphony "$HOME/.local/bin/polyphony"

docs-build:
    mdbook build docs

docs-serve:
    mdbook serve docs --open

schema-github:
    ./scripts/refresh_github_schema.sh

schema-linear:
    ./scripts/refresh_linear_schema.sh

schema-refresh:
    just schema-github
    just schema-linear

changelog:
    git-cliff --config cliff.toml --output CHANGELOG.md

changelog-unreleased:
    git-cliff --config cliff.toml --unreleased

changelog-release version:
    git-cliff --config cliff.toml --unreleased --tag "{{version}}" --strip all

release:
    ./scripts/ci/release-tag.sh

release-push:
    ./scripts/ci/release-tag.sh --push
