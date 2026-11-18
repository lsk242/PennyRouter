#!/usr/bin/env bash
# Validate provider keys from the repo-root .env against free
# auth-check endpoints (no spend, no chat calls).
#
# Usage (from the repo root):
#   ./tests/live/scripts/validate-keys.sh [path-to-env]
#
# Checks MODAL_API_KEY (Bearer) against the documented Modal-Key /
# Modal-Secret pair: both must authenticate the same deployments.
# Prints statuses and safe metadata only — never key values.
# Exit 0 = all present keys valid, 1 = any failure.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ENV_FILE="${1:-$ROOT/.env}"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: env file not found: $ENV_FILE" >&2
  exit 1
fi
get() { grep -E "^$1=" "$ENV_FILE" | tail -1 | cut -d= -f2- || true; }

OPENROUTER_API_KEY="$(get OPENROUTER_API_KEY)"
BASETEN_API_KEY="$(get BASETEN_API_KEY)"
MODAL_API_KEY="$(get MODAL_API_KEY)"
MODAL_KEY_ID="$(get MODAL_KEY_ID)"
MODAL_KEY_SECRET="$(get MODAL_KEY_SECRET)"
MODAL_SCORE_URL="$(get MODAL_SCORE_URL)"
MODAL_BASE_URL="$(get MODAL_BASE_URL)"

fail=0
OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT

have() { [ -n "$1" ]; }
api_root() { # deployment root -> chat API root (append /v1 unless present)
  local u="${1%/}"
  case "$u" in */v1) printf '%s' "$u" ;; *) printf '%s/v1' "$u" ;; esac
}
GET() { curl -s -m 20 -o "$OUT" -w '%{http_code}' "$@"; }

echo "=== 1. OpenRouter key -> GET /api/v1/auth/key ==="
if have "$OPENROUTER_API_KEY"; then
  code="$(GET -H "Authorization: Bearer $OPENROUTER_API_KEY" \
    https://openrouter.ai/api/v1/auth/key)"
  echo "http=$code"
  if [[ "$code" == "200" ]]; then
    python3 -c "
import json
d = json.load(open('$OUT')).get('data', {})
print('label:', d.get('label'), '| usage: \$%.4f' % (d.get('usage') or 0), '| limit:', d.get('limit'))
" 2>/dev/null || head -c 200 "$OUT"
  else
    fail=1
  fi
else
  echo "SKIP: OPENROUTER_API_KEY empty"
fi

echo "=== 2. Baseten key -> GET /v1/models ==="
if have "$BASETEN_API_KEY"; then
  code="$(GET -H "Authorization: Bearer $BASETEN_API_KEY" \
    https://inference.baseten.co/v1/models)"
  echo "http=$code"
  if [[ "$code" == "200" ]]; then
    python3 -c "
import json
ms = json.load(open('$OUT')).get('data', [])
print('models:', len(ms), '| e.g.:', ', '.join(m.get('id','?') for m in ms[:3]))
" 2>/dev/null || head -c 200 "$OUT"
  else
    fail=1
  fi
else
  echo "SKIP: BASETEN_API_KEY empty"
fi

modal_models() { # $1=url $2=auth header args... -> prints model ids
  local url="$1"; shift
  local code
  code="$(GET "$@" "$url")"
  echo "http=$code"
  if [[ "$code" == "200" ]]; then
    python3 -c "
import json
d = json.load(open('$OUT'))
print('models:', [m.get('id','?') for m in d.get('data', [])][:5])
" 2>/dev/null || head -c 200 "$OUT"
  else
    fail=1
  fi
  return 0
}

if have "$MODAL_KEY_ID" && have "$MODAL_KEY_SECRET" && have "$MODAL_SCORE_URL"; then
  echo "=== 3a. Modal pair -> score /v1/models ==="
  modal_models "$(api_root "$MODAL_SCORE_URL")/models" \
    -H "Modal-Key: $MODAL_KEY_ID" -H "Modal-Secret: $MODAL_KEY_SECRET"
else
  echo "SKIP 3a: MODAL_KEY_ID/SECRET/SCORE_URL incomplete"
fi
if have "$MODAL_KEY_ID" && have "$MODAL_KEY_SECRET" && have "$MODAL_BASE_URL"; then
  echo "=== 3b. Modal pair -> serving /v1/models ==="
  modal_models "$(api_root "$MODAL_BASE_URL")/models" \
    -H "Modal-Key: $MODAL_KEY_ID" -H "Modal-Secret: $MODAL_KEY_SECRET"
else
  echo "SKIP 3b: MODAL_KEY_ID/SECRET/BASE_URL incomplete"
fi
if have "$MODAL_API_KEY" && have "$MODAL_SCORE_URL"; then
  echo "=== 4a. Modal Bearer -> score /v1/models ==="
  modal_models "$(api_root "$MODAL_SCORE_URL")/models" \
    -H "Authorization: Bearer $MODAL_API_KEY"
else
  echo "SKIP 4a: MODAL_API_KEY/SCORE_URL incomplete"
fi
if have "$MODAL_API_KEY" && have "$MODAL_BASE_URL"; then
  echo "=== 4b. Modal Bearer -> serving /v1/models ==="
  modal_models "$(api_root "$MODAL_BASE_URL")/models" \
    -H "Authorization: Bearer $MODAL_API_KEY"
else
  echo "SKIP 4b: MODAL_API_KEY/BASE_URL incomplete"
fi

exit "$fail"
