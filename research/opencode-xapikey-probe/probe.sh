#!/bin/bash
# Probe: does https://opencode.ai/zen/go/v1/messages accept x-api-key auth?
# The key is read from ~/.config/probe-keys/opencode-key into an env var
# inside this script; it is never echoed, logged, or written to a file.
# One request only, per the ticket.
set -euo pipefail
cd "$(dirname "$0")"
KEY="$(cat ~/.config/probe-keys/opencode-key)"
mkdir -p raw
STATUS="$(curl -s -o raw/messages.json -w '%{http_code}' \
  --request POST 'https://opencode.ai/zen/go/v1/messages' \
  --header "x-api-key: $KEY" \
  --header 'anthropic-version: 2023-06-01' \
  --header 'content-type: application/json' \
  --data '{"model":"claude-haiku-5-5","max_tokens":16,"messages":[{"role":"user","content":"Reply with the word ok."}]}')"
printf 'HTTP %s\n' "$STATUS"
