#!/usr/bin/env bash
# Verifies the provider-pinned config reaches the gateway container.
# Both compose files split config the same way: generic values via
# env_file .env.dev, secrets + custom URLs via environment
# interpolation. The committed .env.dev base must define the generic
# ones, .env.example must document the custom-URL slot, and
# deploy.sh must ship base + secrets overlay merged (plus the base
# as .env.dev when referenced). The server file (compose.dev.yaml)
# additionally publishes no ports: gateway + catalog-ui sit on the
# front-proxy network behind the reverse proxy.
# Exit 0 = all present, 1 = gap still open.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
fail=0

for f in compose.yaml compose.dev.yaml; do
  if grep -Eq '^[[:space:]]+-[[:space:]]+\.env\.dev\s*$' "$ROOT/$f"; then
    echo "PASS: $f loads generic config via env_file .env.dev"
  else
    echo "FAIL: $f does not reference env_file .env.dev"; fail=1
  fi
  for v in MODAL_BASE_URL MODAL_MODELS BASETEN_MODELS \
      BASETEN_API_KEY MODAL_KEY_ID MODAL_KEY_SECRET MODAL_SCORE_URL RANKINGS_API_KEY; do
    if grep -Eq "^[[:space:]]+$v: " "$ROOT/$f"; then
      echo "PASS: $f takes $v from the stack env"
    else
      echo "FAIL: $f environment misses $v"; fail=1
    fi
  done
done

if grep -Eq '^[[:space:]]*ports:' "$ROOT/compose.dev.yaml"; then
  echo "FAIL: compose.dev.yaml publishes ports (proxy only)"; fail=1
else
  echo "PASS: compose.dev.yaml publishes no ports"
fi
if [ "$(grep -cE '^[[:space:]]+-[[:space:]]+"8080"' "$ROOT/compose.dev.yaml")" -ge 2 ]; then
  echo "PASS: compose.dev.yaml exposes 8080 (gateway + catalog-ui)"
else
  echo "FAIL: compose.dev.yaml misses expose 8080 entries"; fail=1
fi
if grep -Eq '^[[:space:]]*ports:' "$ROOT/compose.yaml"; then
  echo "PASS: compose.yaml publishes ports for local access"
else
  echo "FAIL: compose.yaml publishes no ports"; fail=1
fi

for v in BASETEN_BASE_URL GATEWAY_UI FETCH_OPENROUTER PROXY_NETWORK; do
  if grep -Eq "^$v=" "$ROOT/.env.dev"; then
    echo "PASS: .env.dev defines $v"
  else
    echo "FAIL: .env.dev does not define $v"; fail=1
  fi
done

# MODAL_BASE_URL is a per-deployment custom URL: documented in the
# template, real value in the gitignored .env overlay (never committed).
if grep -Eq "MODAL_BASE_URL" "$ROOT/.env.example"; then
  echo "PASS: .env.example documents MODAL_BASE_URL"
else
  echo "FAIL: .env.example does not document MODAL_BASE_URL"; fail=1
fi
if [[ -f "$ROOT/.env" ]]; then
  if grep -Eq "^MODAL_BASE_URL=.+" "$ROOT/.env"; then
    echo "PASS: local .env sets MODAL_BASE_URL"
  else
    echo "FAIL: local .env leaves MODAL_BASE_URL unset"; fail=1
  fi
else
  echo "SKIP: no local .env overlay present"
fi

if grep -q 'DEPLOY_ENV_FILE:-$ROOT/.env.dev' "$ROOT/scripts/deploy.sh" \
  && grep -q 'DEPLOY_SECRETS_FILE:-$ROOT/.env' "$ROOT/scripts/deploy.sh"; then
  echo "PASS: scripts/deploy.sh ships merged .env.dev + .env"
else
  echo "FAIL: scripts/deploy.sh does not merge .env.dev + .env"; fail=1
fi
if grep -q 'DEPLOY_DIR/.env.dev' "$ROOT/scripts/deploy.sh"; then
  echo "PASS: scripts/deploy.sh ships the base as .env.dev when referenced"
else
  echo "FAIL: scripts/deploy.sh does not ship .env.dev"; fail=1
fi

exit "$fail"
