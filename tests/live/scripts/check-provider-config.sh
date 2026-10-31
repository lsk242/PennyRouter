#!/usr/bin/env bash
# Verifies the provider-pinned config reaches the gateway container:
# compose files must forward MODAL_BASE_URL / BASETEN_BASE_URL /
# GATEWAY_UI, and scripts/deploy.sh must ship them in its generated
# remote .env. Exit 0 = all present, 1 = gap still open.
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

for v in MODAL_BASE_URL BASETEN_BASE_URL GATEWAY_UI; do
  if grep -q "$v" "$ROOT/scripts/deploy.sh"; then
    echo "PASS: scripts/deploy.sh ships $v"
  else
    echo "FAIL: scripts/deploy.sh does not ship $v"; fail=1
  fi
done

exit "$fail"
