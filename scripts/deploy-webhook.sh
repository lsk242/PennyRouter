#!/usr/bin/env bash
# Trigger a redeploy through a generic HTTP endpoint (platform track).
#
# Just POSTs a JSON payload to a configured URL — e.g. a stack
# manager's sync endpoint with {"pull":true,"forceRecreate":true}.
# No git interaction: pushing still deploys through the repo webhook
# when one is configured; this is the manual/CI force path.
#
# Configuration (all via environment, or the gitignored `.env.deploy`
# file — see `.env.deploy.sample` — which loads automatically when
# present; explicit environment wins over file values):
#   DEPLOY_URL        Full endpoint URL, e.g.
#                     http://<host>:3000/api/git/stacks/<id>/deploy-stream
#                     (required).
#   DEPLOY_API_TOKEN  Optional bearer token. Empty/unset = no
#                     Authorization header is sent.
#   DEPLOY_PAYLOAD    JSON request body. Default: '{}'.
#   DEPLOY_TIMEOUT    Max seconds for the call. Default: 60.
#   DRY_RUN           Set to 1 to print what would run (token
#                     redacted) without calling anything.
#
# Usage:
#   DEPLOY_URL=http://<host>:3000/api/git/stacks/<id>/deploy-stream \
#     DEPLOY_PAYLOAD='{"pull":true,"forceRecreate":true}' \
#     ./scripts/deploy-webhook.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Local deploy config (gitignored `.env.deploy`, see
# `.env.deploy.sample`): fills variables that are not already set,
# so explicit environment wins over file values. No shell expansion
# and no inline comments in the file; one `KEY=value` per line,
# optional surrounding quotes.
if [[ -f "$ROOT/.env.deploy" ]]; then
  while IFS='=' read -r key val || [[ -n "$key" ]]; do
    key="${key//[[:space:]]/}"
    case "$key" in ''|\#*) continue ;; esac
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    if [[ -z "${!key+x}" ]]; then
      val="${val%$'\r'}"
      val="${val%\"}"; val="${val#\"}"
      val="${val%\'}"; val="${val#\'}"
      printf -v "$key" '%s' "$val"
      export "$key"
    fi
  done < "$ROOT/.env.deploy"
fi

URL="${DEPLOY_URL:-}"
if [[ -z "$URL" ]]; then
  echo "error: DEPLOY_URL is not set (full endpoint URL)" >&2
  exit 1
fi
PAYLOAD='{}'
if [[ -n "${DEPLOY_PAYLOAD:-}" ]]; then
  PAYLOAD="$DEPLOY_PAYLOAD"
fi

ARGS=(curl -fsS --max-time "${DEPLOY_TIMEOUT:-60}" -X POST "$URL"
  -H 'Content-Type: application/json')
if [[ -n "${DEPLOY_API_TOKEN:-}" ]]; then
  ARGS+=(-H "Authorization: Bearer $DEPLOY_API_TOKEN")
fi
ARGS+=(-d "$PAYLOAD")

if [[ "${DRY_RUN:-0}" == "1" ]]; then
  echo "+ POST $URL"
  if [[ -n "${DEPLOY_API_TOKEN:-}" ]]; then
    echo '  Authorization: Bearer <redacted>'
  else
    echo '  (no Authorization header)'
  fi
  echo "  body: $PAYLOAD"
  exit 0
fi
"${ARGS[@]}"
