#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <tag> <output-dir>" >&2
  exit 1
fi

TAG="$1"
OUTPUT_DIR="$2"
ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"

mkdir -p "${OUTPUT_DIR}"

git-cliff \
  --workdir "${ROOT_DIR}" \
  --config "${ROOT_DIR}/cliff.toml" \
  --tag "${TAG}" \
  --output "${OUTPUT_DIR}/CHANGELOG.md"

git-cliff \
  --workdir "${ROOT_DIR}" \
  --config "${ROOT_DIR}/cliff.toml" \
  --latest \
  --tag "${TAG}" \
  --strip header \
  --output "${OUTPUT_DIR}/RELEASE_NOTES.md"
