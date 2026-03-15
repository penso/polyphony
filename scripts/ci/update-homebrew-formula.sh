#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <tag> <sha256>" >&2
  exit 1
fi

TAG="$1"
SHA256="$2"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
FORMULA_TEMPLATE="${SCRIPT_DIR}/../../homebrew/Formula/polyphony.rb"

if [[ ! -f "${FORMULA_TEMPLATE}" ]]; then
  echo "error: formula template not found at ${FORMULA_TEMPLATE}" >&2
  exit 1
fi

sed -e "s/version \"PLACEHOLDER\"/version \"${TAG}\"/" \
    -e "s/sha256 \"PLACEHOLDER\"/sha256 \"${SHA256}\"/" \
    "${FORMULA_TEMPLATE}"
