#!/usr/bin/env bash
# Verifies the provider-pinned config reaches the gateway container:
# compose files must forward MODAL_BASE_URL / BASETEN_BASE_URL /
# GATEWAY_UI, the deploy env file must define them (deploy.sh ships
# the repo-root .env as the remote .env), and deploy.sh must copy it.
# Exit 0 = all present, 1 = gap still open.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
fail=0

for f in compose.yaml compose.dev.yaml compose.prod.yaml; do
  for v in MODAL_BASE_URL BASETEN_BASE_URL GATEWAY_UI; do
    if grep -q "$v" "$ROOT/$f"; then
      echo "PASS: $f forwards $v"
    else
      echo "FAIL: $f does not forward $v"; fail=1
    fi
  done
done

ENV_SRC="$ROOT/.env"
[[ -f "$ENV_SRC" ]] || ENV_SRC="$ROOT/.env.example"
for v in MODAL_BASE_URL BASETEN_BASE_URL GATEWAY_UI FETCH_OPENROUTER; do
  if grep -Eq "^$v=" "$ENV_SRC"; then
    echo "PASS: $(basename "$ENV_SRC") defines $v"
  else
    echo "FAIL: $(basename "$ENV_SRC") does not define $v"; fail=1
  fi
done

if grep -q 'DEPLOY_ENV_FILE:-$ROOT/.env' "$ROOT/scripts/deploy.sh"; then
  echo "PASS: scripts/deploy.sh ships the repo-root .env"
else
  echo "FAIL: scripts/deploy.sh does not ship the repo-root .env"; fail=1
fi

exit "$fail"
