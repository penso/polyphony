#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: release-tag.sh [--print-only] [--push]

Creates the next local YYYYMMDD.NN release tag for the current commit.
Pass --print-only to only print the next tag without creating it.
Pass --push to also push the created tag to origin.
EOF
}

print_only=false
push_tag=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --print-only)
      print_only=true
      ;;
    --push)
      push_tag=true
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 1
      ;;
  esac
  shift
done

today="$(date +%Y%m%d)"
latest_today_tag="$(git tag --list "${today}.*" --sort=-v:refname | head -n 1)"

if [[ -z "${latest_today_tag}" ]]; then
  sequence="01"
else
  latest_sequence="${latest_today_tag##*.}"
  next_sequence=$((10#"${latest_sequence}" + 1))
  sequence="$(printf '%02d' "${next_sequence}")"
fi

tag="${today}.${sequence}"
echo "${tag}"

if [[ "${print_only}" == "true" ]]; then
  exit 0
fi

git tag -a "${tag}" -m "Release ${tag}"

if [[ "${push_tag}" == "true" ]]; then
  git push origin "${tag}"
fi
