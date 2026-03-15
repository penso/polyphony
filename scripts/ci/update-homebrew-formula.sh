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

formula_content=$(
  sed -e "s/TAG_PLACEHOLDER/${TAG}/g" \
      -e "s/SHA256_PLACEHOLDER/${SHA256}/g" \
      "${FORMULA_TEMPLATE}"
)

if grep -q 'TAG_PLACEHOLDER\|SHA256_PLACEHOLDER' <<<"${formula_content}"; then
  echo "error: formula placeholders were not fully replaced" >&2
  exit 1
fi

printf '%s\n' "${formula_content}"
