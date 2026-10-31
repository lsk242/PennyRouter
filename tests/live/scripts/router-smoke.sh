#!/usr/bin/env bash
# Smoke-check a deployed router for the live agent test.
#
# Usage:
#   set -a; . tests/live/.env; set +a
#   ./tests/live/scripts/router-smoke.sh
#
# Needs ROUTER_URL. If SMOKE_API_KEY + SMOKE_MODEL are set, also sends
# one tiny non-streaming chat completion; otherwise that probe is
# skipped and no key is used.
set -euo pipefail

ROUTER_URL="${ROUTER_URL:-}"
if [[ -z "$ROUTER_URL" ]]; then
  echo "error: ROUTER_URL is not set (source tests/live/.env first)" >&2
  exit 1
fi
BASE="${ROUTER_URL%/}"
fail=0

ok()  { echo "PASS: $1"; }
bad() { echo "FAIL: $1"; fail=1; }

# 1. Health.
if [[ "$(curl -fsS --max-time 10 "$BASE/health")" == '{"status":"ok"}' ]]; then
  ok "/health"
else
  bad "/health did not return {\"status\":\"ok\"}"
fi

# 2. Curated model menu must list the free virtual id.
if curl -fsS --max-time 10 "$BASE/v1/models" | grep -q "openrouter/pair/free"; then
  ok "/v1/models lists openrouter/pair/free"
else
  bad "/v1/models is missing openrouter/pair/free"
fi

# 3. Debug UI presence (informational; controlled by GATEWAY_UI=1).
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$BASE/ui")"
echo "INFO: GET /ui -> HTTP $code (200 means GATEWAY_UI=1 is active)"

# 4. Optional tiny chat probe (spends backend quota — off by default).
if [[ -n "${SMOKE_API_KEY:-}" && -n "${SMOKE_MODEL:-}" ]]; then
  if curl -fsS --max-time 60 "$BASE/v1/chat/completions" \
    -H "Authorization: Bearer $SMOKE_API_KEY" \
    -H "Content-Type: application/json" \
    -d "{\"model\":\"$SMOKE_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"reply with the word ok\"}],\"max_tokens\":5}" \
    | grep -q '"choices"'; then
    ok "chat probe via model $SMOKE_MODEL"
  else
    bad "chat probe via model $SMOKE_MODEL returned no choices"
  fi
else
  echo "SKIP: chat probe (set SMOKE_API_KEY + SMOKE_MODEL to enable)"
fi

exit "$fail"
