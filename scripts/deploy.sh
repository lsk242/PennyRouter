#!/usr/bin/env bash
# Deploy the stack to a server over ssh (platform track, Docker stage).
#
# Copies the pull-only compose file plus the repo-root `.env` to the
# server, then pulls the CI-built images from GitHub Packages and
# restarts the services. Nothing is built on the server.
#
# Config (including catalog judge keys) ships in that `.env` — the
# no-keys rule covers gateway *serving* keys only, which stay pure
# BYOK inside each request. The remote `.env` is chmod 600.
#
# Configuration (all via environment):
#   DEPLOY_HOST          ssh target, e.g. user@example.com. Default: apps.lan.
#   DEPLOY_DIR           Remote directory. Default: /opt/stacks/pennyrouter.
#   COMPOSE_FILE         Local compose file to ship. Default: compose.dev.yaml
#                        (gateway + catalog-sync). Must be pull-only (no
#                        `build:` section, CI-enforced).
#   REMOTE_COMPOSE       Name on the server. Default: compose.yaml, so plain
#                        `docker compose` finds it without -f.
#   DEPLOY_ENV_FILE      Local env file to ship as the remote `.env`.
#                        Default: the repo-root `.env`. Must define every
#                        variable the compose file references (PORT,
#                        HOST_PORT, IMAGE, CATALOG_IMAGE, *_BASE_URL,
#                        GATEWAY_UI, FETCH_OPENROUTER, SYNC_INTERVAL);
#                        IMAGE and CATALOG_IMAGE must be pullable tags.
#   SSH_OPTS             Extra options for ssh/scp, e.g. "-i ~/.ssh/id -p 2222".
#   DRY_RUN              Set to 1 to print what would run (env shown
#                        redacted) without touching the server.
#
# Usage:
#   ./scripts/deploy.sh                  # defaults: dev stack to apps.lan
#   DEPLOY_HOST=user@example.com make deploy
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

DEPLOY_HOST="${DEPLOY_HOST:-apps.lan}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/stacks/pennyrouter}"
COMPOSE_FILE="${COMPOSE_FILE:-compose.dev.yaml}"
DRY_RUN="${DRY_RUN:-0}"

# shellcheck disable=SC2086
SSH=(ssh ${SSH_OPTS:-})
# shellcheck disable=SC2086
SCP=(scp ${SSH_OPTS:-})

SRC="$ROOT/$COMPOSE_FILE"
if [[ ! -f "$SRC" ]]; then
  echo "error: compose file not found: $SRC" >&2
  exit 1
fi
if grep -Eq '^[[:space:]]*build:' "$SRC"; then
  echo "error: $COMPOSE_FILE must be pull-only (no build: section)" >&2
  exit 1
fi

ENV_FILE="${DEPLOY_ENV_FILE:-$ROOT/.env}"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: env file not found: $ENV_FILE" >&2
  exit 1
fi
for v in PORT HOST_PORT IMAGE; do
  if ! grep -Eq "^$v=" "$ENV_FILE"; then
    echo "error: $ENV_FILE does not define $v" >&2
    exit 1
  fi
done
# Health-check port comes from the shipped file, like the stack does.
HOST_PORT="$(grep -E '^HOST_PORT=' "$ENV_FILE" | tail -1 | cut -d= -f2)"
HOST_PORT="${HOST_PORT:-3000}"

REMOTE_COMPOSE="${REMOTE_COMPOSE:-compose.yaml}"

run() {
  if [[ "$DRY_RUN" == "1" ]]; then
    printf '+ %s\n' "$*"
  else
    "$@"
  fi
}

echo "--- deploying $COMPOSE_FILE to $DEPLOY_HOST:$DEPLOY_DIR (host port $HOST_PORT) ---"
if [[ "$DRY_RUN" == "1" ]]; then
  echo "--- shipped .env ($ENV_FILE, redacted) ---"
  sed -E 's/^([^=]*(KEY|SECRET|TOKEN)[^=]*)=.*/\1=<redacted>/' "$ENV_FILE"
fi
run "${SSH[@]}" "$DEPLOY_HOST" "mkdir -p $DEPLOY_DIR"
run "${SCP[@]}" "$SRC" "$DEPLOY_HOST:$DEPLOY_DIR/$REMOTE_COMPOSE"
run "${SCP[@]}" "$ENV_FILE" "$DEPLOY_HOST:$DEPLOY_DIR/.env"
run "${SSH[@]}" "$DEPLOY_HOST" \
  "cd $DEPLOY_DIR && chmod 600 .env && docker compose -f $REMOTE_COMPOSE pull && docker compose -f $REMOTE_COMPOSE up -d"
run "${SSH[@]}" "$DEPLOY_HOST" \
  "ok=0; for i in \$(seq 1 30); do if curl -fsS --connect-timeout 2 --max-time 5 http://localhost:$HOST_PORT/health; then ok=1; break; fi; sleep 2; done; [ \"\$ok\" = 1 ] || { echo 'deploy: gateway health check failed' >&2; exit 1; }"
echo "--- deployed: http://$DEPLOY_HOST:$HOST_PORT/health ---"
