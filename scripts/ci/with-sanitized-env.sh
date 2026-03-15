#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 64
fi

unset_vars=(
  ACP_TOKEN
  ANTHROPIC_API_KEY
  GH_TOKEN
  GITHUB_TOKEN
  HANDOFF_WEBHOOK_TOKEN
  KIMI_API_KEY
  LINEAR_API_KEY
  MOONSHOT_API_KEY
  OPENAI_API_KEY
  OPENROUTER_API_KEY
  TELEGRAM_BOT_TOKEN
  TRACKER_API_KEY
)

env_args=()
for name in "${unset_vars[@]}"; do
  env_args+=("-u" "$name")
done

exec env "${env_args[@]}" "$@"
