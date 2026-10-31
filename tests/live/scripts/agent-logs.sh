#!/usr/bin/env bash
# Unified log access for the live agent test. This script holds the
# *how* (hosts, commands, paths); tests/live/.env holds only the
# hostnames to override the defaults below.
#
# Usage (from the repo root):
#   ./tests/live/scripts/agent-logs.sh router [--tail N] [--follow|-f]
#   ./tests/live/scripts/agent-logs.sh hermes [--tail N]
#   ./tests/live/scripts/agent-logs.sh openclaw [--tail N]
#   ./tests/live/scripts/agent-logs.sh openhands [--tail N]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
if [[ -f "$HERE/../.env" ]]; then
  # shellcheck disable=SC1091
  set -a; . "$HERE/../.env"; set +a
fi
DEPLOY_HOST="${DEPLOY_HOST:-apps.lan}"
OPENCLAW_HOST="${OPENCLAW_HOST:-openclaw.lan}"
HERMES_HOST="${HERMES_HOST:-hermes.lan}"
OPENHANDS_HOST="${OPENHANDS_HOST:-openhands.lan}"

target="${1:-}"; shift || true
tail_n=50
follow=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tail) tail_n="${2:-}"; shift 2;;
    --follow|-f) follow=1; shift;;
    *) echo "error: unknown flag: $1" >&2; exit 1;;
  esac
done
[[ "$tail_n" =~ ^[0-9]+$ ]] || { echo "error: --tail needs a number" >&2; exit 1; }

# shellcheck disable=SC2086
SSH=(ssh ${SSH_OPTS:-})

case "$target" in
  router)
    # Gateway JSON lines: request_id, method, route, status, outcome.
    if [[ "$follow" == 1 ]]; then
      "${SSH[@]}" "$DEPLOY_HOST" docker logs -f gateway
    else
      "${SSH[@]}" "$DEPLOY_HOST" "docker logs --tail $tail_n gateway"
    fi
    ;;
  hermes)
    "${SSH[@]}" "$HERMES_HOST" "tail -n $tail_n ~/.hermes/logs/agent.log"
    ;;
  openclaw)
    # Fresh install, foreground session: no log files yet. Show any
    # that appear, else process state.
    "${SSH[@]}" "$OPENCLAW_HOST" \
      "logs=\$(find ~/.openclaw -maxdepth 2 -name '*.log' 2>/dev/null); if [[ -n \"\$logs\" ]]; then tail -n $tail_n \$logs; else echo '(no openclaw log files yet — foreground session)'; ps aux | grep '[o]penclaw' | grep -v 'maxdepth' | head -3; fi"
    ;;
  openhands)
    # Agent errors live in the canvas log file; the tmux pane only
    # shows server access lines (fallback when the file is missing).
    "${SSH[@]}" "$OPENHANDS_HOST" \
      "log=~/.openhands/agent-canvas/logs/agent-canvas.\$(date +%F).log; if [[ -f \"\$log\" ]]; then tail -n $tail_n \"\$log\"; else tmux capture-pane -p -t canvas:0 -S -$tail_n 2>/dev/null || echo '(openhands logs not found)'; fi"
    ;;
  *)
    echo "usage: agent-logs.sh (router|hermes|openclaw|openhands) [--tail N] [--follow]" >&2
    exit 1
    ;;
esac
