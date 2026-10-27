#!/usr/bin/env bash
# Gateway-overhead comparison runner.
#
# Runs the same chat workload against a mock backend directly and through
# the resource-capped gateway, then writes a comparison report. Refuses to
# overwrite an existing results directory.
#
# Usage (from the repo root):
#   tests/load/compare.sh [--mode json|sse|both] [--sessions N]
#     [--duration S] [--mock-latency-ms N] [--results-dir DIR] [--no-build]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MODE="both"
SESSIONS="20"
DURATION="15"
MOCK_LATENCY_MS="20"
RESULTS_DIR=""
BASE_COMPOSE="$ROOT/compose.yaml"
PERF_COMPOSE="$ROOT/compose.perf.yaml"
BUILD="--build"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE="$2"; shift 2 ;;
    --sessions) SESSIONS="$2"; shift 2 ;;
    --duration) DURATION="$2"; shift 2 ;;
    --mock-latency-ms) MOCK_LATENCY_MS="$2"; shift 2 ;;
    --results-dir) RESULTS_DIR="$2"; shift 2 ;;
    --no-build) BUILD=""; shift ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$MODE" in
  json|sse) MODES="$MODE" ;;
  both) MODES="json sse" ;;
  *) echo "mode must be json, sse, or both" >&2; exit 2 ;;
esac

if [[ -z "$RESULTS_DIR" ]]; then
  RESULTS_DIR="$ROOT/tests/load/results/compare-$(date -u +%Y%m%dT%H%M%SZ)"
fi
if [[ -e "$RESULTS_DIR" ]]; then
  echo "refusing to overwrite existing results dir: $RESULTS_DIR" >&2
  exit 1
fi
mkdir -p "$RESULTS_DIR"

HOST_PORT="${HOST_PORT:-3000}"
COMPOSE=(docker compose -f "$BASE_COMPOSE" -f "$PERF_COMPOSE")
MOCK_PID=""

cleanup() {
  if [[ -n "$MOCK_PID" ]]; then kill "$MOCK_PID" 2>/dev/null || true; fi
  "${COMPOSE[@]}" down --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "--- starting mock backend ---"
# Bound to all interfaces so the containerized gateway can reach it via
# host.docker.internal; the direct phase still uses 127.0.0.1.
python3 "$ROOT/tests/load/mock_backend.py" --port 0 --bind 0.0.0.0 \
  --latency-ms "$MOCK_LATENCY_MS" >"$RESULTS_DIR/mock.port" \
  2>"$RESULTS_DIR/mock.stderr.log" &
MOCK_PID=$!
for _ in $(seq 1 30); do
  if grep -q '^PORT=' "$RESULTS_DIR/mock.port" 2>/dev/null; then break; fi
  sleep 0.2
done
MOCK_PORT="$(sed -n 's/^PORT=//p' "$RESULTS_DIR/mock.port")"
if [[ -z "$MOCK_PORT" ]]; then
  echo "mock backend failed to start" >&2
  cat "$RESULTS_DIR/mock.stderr.log" >&2 || true
  exit 1
fi
echo "mock on 127.0.0.1:$MOCK_PORT (pid $MOCK_PID)"

run_workload() { # <mode> <base-url> <output>
  python3 "$ROOT/tests/load/chat_workload.py" \
    --mode "$1" --base-url "$2" --output "$3" \
    --sessions "$SESSIONS" --duration "$DURATION"
}

for mode in $MODES; do
  echo "--- direct phase ($mode) ---"
  run_workload "$mode" "http://127.0.0.1:$MOCK_PORT" \
    "$RESULTS_DIR/direct-$mode.json"
done

echo "--- compose config ---"
"${COMPOSE[@]}" config > "$RESULTS_DIR/compose.config.yaml"

echo "--- starting constrained gateway ---"
# shellcheck disable=SC2086
OPENROUTER_BASE_URL="http://host.docker.internal:$MOCK_PORT" \
  "${COMPOSE[@]}" up $BUILD -d

echo "--- waiting for /health ---"
for _ in $(seq 1 60); do
  if curl -fsS "http://localhost:${HOST_PORT}/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
curl -fsS "http://localhost:${HOST_PORT}/health" > "$RESULTS_DIR/health.json"
echo

CID="$("${COMPOSE[@]}" ps -q gateway)"
docker inspect "$CID" --format '{{.Image}} {{.Config.Image}}' > "$RESULTS_DIR/image.txt"
{
  echo "git: $(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
  docker --version
  docker compose version
  uname -a
} > "$RESULTS_DIR/environment.txt"
cat > "$RESULTS_DIR/params.json" <<EOF
{"mode": "$MODE", "sessions": $SESSIONS, "duration_seconds": $DURATION,
 "mock_latency_ms": $MOCK_LATENCY_MS, "mock_port": $MOCK_PORT,
 "host_port": $HOST_PORT}
EOF

for mode in $MODES; do
  echo "--- gateway phase ($mode) ---"
  run_workload "$mode" "http://localhost:${HOST_PORT}" \
    "$RESULTS_DIR/gateway-$mode.json"
done
curl -fsS "http://localhost:${HOST_PORT}/metrics" > "$RESULTS_DIR/metrics.txt"

"${COMPOSE[@]}" down --remove-orphans
kill "$MOCK_PID" 2>/dev/null || true
MOCK_PID=""
trap - EXIT

for mode in $MODES; do
  echo "=== report ($mode) ==="
  python3 "$ROOT/tests/load/compare.py" \
    "$RESULTS_DIR/direct-$mode.json" "$RESULTS_DIR/gateway-$mode.json" \
    --output-dir "$RESULTS_DIR/$mode" \
    --label "mock_latency=${MOCK_LATENCY_MS}ms"
done
echo "--- results in $RESULTS_DIR ---"
