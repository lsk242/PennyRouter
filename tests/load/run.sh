#!/usr/bin/env bash
# Health-only load scaffold runner.
#
# Starts the gateway under the resource-capped perf profile, runs the
# session workload stages, samples container stats, and writes evidence.
# Refuses to overwrite an existing results directory.
#
# Usage (from the repo root):
#   tests/load/run.sh [--workload FILE] [--results-dir DIR]
#                     [--compose-file FILE] [--no-build]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORKLOAD="$ROOT/tests/load/workloads/health_sessions.json"
RESULTS_DIR=""
BASE_COMPOSE="$ROOT/compose.yaml"
PERF_COMPOSE="$ROOT/compose.perf.yaml"
BUILD="--build"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --workload) WORKLOAD="$2"; shift 2 ;;
    --results-dir) RESULTS_DIR="$2"; shift 2 ;;
    --compose-file) BASE_COMPOSE="$2"; shift 2 ;;
    --no-build) BUILD=""; shift ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$RESULTS_DIR" ]]; then
  RESULTS_DIR="$ROOT/tests/load/results/$(date -u +%Y%m%dT%H%M%SZ)"
fi
if [[ -e "$RESULTS_DIR" ]]; then
  echo "refusing to overwrite existing results dir: $RESULTS_DIR" >&2
  exit 1
fi
mkdir -p "$RESULTS_DIR"
cp "$WORKLOAD" "$RESULTS_DIR/workload.json"

HOST_PORT="${HOST_PORT:-3000}"
BASE_URL="http://localhost:${HOST_PORT}"
COMPOSE=(docker compose -f "$BASE_COMPOSE" -f "$PERF_COMPOSE")
SAMPLER_PID=""

cleanup() {
  if [[ -n "$SAMPLER_PID" ]]; then kill "$SAMPLER_PID" 2>/dev/null || true; fi
  "${COMPOSE[@]}" down --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "--- compose config ---"
"${COMPOSE[@]}" config > "$RESULTS_DIR/compose.config.yaml"

echo "--- starting constrained gateway ---"
# shellcheck disable=SC2086
"${COMPOSE[@]}" up $BUILD -d

echo "--- waiting for /health ---"
for _ in $(seq 1 60); do
  if curl -fsS "$BASE_URL/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
curl -fsS "$BASE_URL/health" > "$RESULTS_DIR/health.json"
echo

CID="$("${COMPOSE[@]}" ps -q gateway)"
docker inspect "$CID" --format '{{.Image}} {{.Config.Image}}' > "$RESULTS_DIR/image.txt"
{
  echo "git: $(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
  docker --version
  docker compose version
  uname -a
} > "$RESULTS_DIR/environment.txt"

echo "--- sampling container stats to stats.jsonl ---"
(
  while true; do
    docker stats --no-stream --format '{{json .}}' "$CID" || break
    sleep 1
  done > "$RESULTS_DIR/stats.jsonl"
) &
SAMPLER_PID=$!

echo "--- running workload stages ---"
set +e
python3 "$ROOT/tests/load/session_workload.py" \
  --workload "$WORKLOAD" --base-url "$BASE_URL" \
  --output-dir "$RESULTS_DIR/stages"
WORKLOAD_RC=$?
set -e

kill "$SAMPLER_PID" 2>/dev/null || true
SAMPLER_PID=""
"${COMPOSE[@]}" down --remove-orphans
trap - EXIT

cat > "$RESULTS_DIR/manifest.json" <<EOF
{
  "workload_rc": $WORKLOAD_RC,
  "base_compose": "$BASE_COMPOSE",
  "perf_compose": "$PERF_COMPOSE",
  "base_url": "$BASE_URL"
}
EOF

echo "--- results in $RESULTS_DIR (rc=$WORKLOAD_RC) ---"
exit "$WORKLOAD_RC"
