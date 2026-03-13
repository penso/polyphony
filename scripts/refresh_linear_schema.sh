#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
query_file="${repo_root}/scripts/graphql/linear_introspection_query.json"
out="${repo_root}/crates/linear/src/linear_schema.json"

api_key="${LINEAR_API_KEY:-}"
if [[ -z "${api_key}" ]]; then
  echo "LINEAR_API_KEY is required" >&2
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "${tmp}"' EXIT

curl --fail --silent --show-error \
  "https://api.linear.app/graphql" \
  -H "Authorization: ${api_key}" \
  -H "Content-Type: application/json" \
  --data @"${query_file}" \
  --output "${tmp}"

if grep -q '"errors"' "${tmp}"; then
  cat "${tmp}" >&2
  exit 1
fi

mv "${tmp}" "${out}"
trap - EXIT

echo "wrote ${out}"
