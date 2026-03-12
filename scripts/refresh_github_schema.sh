#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${repo_root}/crates/factoryrs-github/src/schema.graphql"

curl --fail --silent --show-error --location \
  "https://docs.github.com/public/ghec/schema.docs.graphql" \
  --output "${out}"

echo "wrote ${out}"
