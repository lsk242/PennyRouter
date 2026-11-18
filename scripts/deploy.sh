#!/usr/bin/env bash
# Deploy the stack to a server over ssh (platform track, Docker stage).
#
# Copies the pull-only compose file plus a merged env file (committed
# `.env.dev` base overlaid with the gitignored `.env` secrets) to the
# server, then pulls the CI-built images from GitHub Packages and
# restarts the services. Nothing is built on the server. When the
# compose file loads generic config via `env_file: [.env.dev]`, the
# base file ships beside compose.yaml under that same name.
#
# Config (including catalog score keys) ships in that merged `.env` —
# the no-keys rule covers gateway *serving* keys only, which stay pure
# BYOK inside each request. The remote `.env` is chmod 600.
#
# For the git-based deploy (push-to-deploy + webhook force), see
# scripts/deploy-webhook.sh. Run only one deploy method per host —
# both stacks use the same container names.
#
# Configuration (all via environment, or the gitignored `.env.deploy`
# file — see `.env.deploy.sample` — which loads automatically when
# present; explicit environment wins over file values):
#   DEPLOY_HOST          ssh target, e.g. user@example.com. Default: srv.lan.
#   DEPLOY_DIR           Remote directory. Default: /opt/stacks/pennyrouter.
#   COMPOSE_FILE         Local compose file to ship. Default: compose.dev.yaml
#                        (gateway + catalog-sync). Must be pull-only (no
#                        `build:` section, CI-enforced).
#   REMOTE_COMPOSE       Name on the server. Default: compose.yaml, so plain
#                        `docker compose` finds it without -f.
#   DEPLOY_ENV_FILE      Local base env file (generic config).
#                        Default: the repo-root `.env.dev` (committed).
#                        Must define PORT, IMAGE, and PROXY_NETWORK
#                        (checked); IMAGE and CATALOG_IMAGE must be
#                        pullable tags. HOST_PORT is local-dev-only —
#                        the server stack publishes no ports (reverse
#                        proxy), so the post-deploy health check runs
#                        from a one-off curl container on the proxy
#                        network against http://gateway:$PORT/health.
#   DEPLOY_SECRETS_FILE  Local secrets overlay (keys + custom URLs).
#                        Default: the repo-root `.env` (gitignored).
#                        Missing/empty = ship the base alone (no online
#                        scoring keys; unset providers fail closed).
#                        Overlay keys win over the base.
#   SSH_OPTS             Extra options for ssh/scp, e.g. "-i ~/.ssh/id -p 2222".
#   DRY_RUN              Set to 1 to print what would run (env shown
#                        redacted) without touching the server.
#
# Usage:
#   ./scripts/deploy.sh                  # defaults: dev stack to srv.lan
#   DEPLOY_HOST=user@example.com make deploy
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

DEPLOY_HOST="${DEPLOY_HOST:-srv.lan}"
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

BASE_FILE="${DEPLOY_ENV_FILE:-$ROOT/.env.dev}"
if [[ ! -f "$BASE_FILE" ]]; then
  echo "error: base env file not found: $BASE_FILE" >&2
  exit 1
fi
SECRETS_FILE="${DEPLOY_SECRETS_FILE:-$ROOT/.env}"

# Merge base + secrets overlay into one shippable .env: base lines
# first minus keys the overlay redefines, then the overlay whole,
# so overlay keys win and each key is defined exactly once.
MERGED="$(mktemp)"
trap 'rm -f "$MERGED"' EXIT
if [[ -f "$SECRETS_FILE" ]]; then
  OVERLAY_KEYS="$(grep -oE '^[A-Za-z_][A-Za-z0-9_]*' "$SECRETS_FILE" | sort -u | paste -sd'|')"
  if [[ -n "$OVERLAY_KEYS" ]]; then
    grep -vE "^($OVERLAY_KEYS)=" "$BASE_FILE" > "$MERGED"
  else
    cat "$BASE_FILE" > "$MERGED"
  fi
  {
    printf '\n# --- secrets overlay (%s): keys below win over the base ---\n' "$SECRETS_FILE"
    cat "$SECRETS_FILE"
  } >> "$MERGED"
else
  echo "note: no secrets overlay at $SECRETS_FILE — shipping the base alone" >&2
  cat "$BASE_FILE" > "$MERGED"
fi

for v in PORT IMAGE PROXY_NETWORK; do
  if ! grep -Eq "^$v=" "$MERGED"; then
    echo "error: merged env does not define $v (base: $BASE_FILE)" >&2
    exit 1
  fi
done
# Health-check target comes from the shipped file, like the stack does.
PORT="$(grep -E '^PORT=' "$MERGED" | tail -1 | cut -d= -f2)"
PORT="${PORT:-8080}"
PROXY_NET="$(grep -E '^PROXY_NETWORK=' "$MERGED" | tail -1 | cut -d= -f2)"
PROXY_NET="${PROXY_NET:-nginx-proxy_default}"

REMOTE_COMPOSE="${REMOTE_COMPOSE:-compose.yaml}"

run() {
  if [[ "$DRY_RUN" == "1" ]]; then
    printf '+ %s\n' "$*"
  else
    "$@"
  fi
}

echo "--- deploying $COMPOSE_FILE to $DEPLOY_HOST:$DEPLOY_DIR (proxy net $PROXY_NET, port $PORT) ---"
if [[ "$DRY_RUN" == "1" ]]; then
  echo "--- shipped .env (base $BASE_FILE + overlay $SECRETS_FILE, redacted) ---"
  sed -E 's/^([^=]*(KEY|SECRET|TOKEN|PASSWORD)[^=]*)=.*/\1=<redacted>/' "$MERGED"
fi
run "${SSH[@]}" "$DEPLOY_HOST" "mkdir -p $DEPLOY_DIR"
run "${SCP[@]}" "$SRC" "$DEPLOY_HOST:$DEPLOY_DIR/$REMOTE_COMPOSE"
# env_file entries resolve next to the compose file on the server, so
# ship the base under the exact referenced name (missing env_file is
# a hard compose error). Compose files without the reference only
# get the merged .env below.
if grep -Eq '^[[:space:]]+-[[:space:]]+\.env\.dev\s*$' "$SRC"; then
  run "${SCP[@]}" "$BASE_FILE" "$DEPLOY_HOST:$DEPLOY_DIR/.env.dev"
fi
run "${SCP[@]}" "$MERGED" "$DEPLOY_HOST:$DEPLOY_DIR/.env"
run "${SSH[@]}" "$DEPLOY_HOST" \
  "cd $DEPLOY_DIR && chmod 600 .env && docker compose -f $REMOTE_COMPOSE pull && docker compose -f $REMOTE_COMPOSE up -d"
run "${SSH[@]}" "$DEPLOY_HOST" \
  "ok=0; for i in \$(seq 1 30); do if docker run --rm --network $PROXY_NET curlimages/curl:latest -fsS --connect-timeout 2 --max-time 5 http://gateway:$PORT/health; then ok=1; break; fi; sleep 2; done; [ \"\$ok\" = 1 ] || { echo 'deploy: gateway health check failed' >&2; exit 1; }"
echo "--- deployed: gateway healthy on $PROXY_NET behind the reverse proxy ---"
