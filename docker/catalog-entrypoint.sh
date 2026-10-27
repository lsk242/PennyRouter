#!/bin/sh
# One sync+process cycle into $OUT_DIR, then sleep-loop when
# SYNC_INTERVAL > 0. Input defaults suit the compose mount layout.
set -eu
: "${MODELS:=/input/models.json}"
: "${ENDPOINTS_DIR:=/input/endpoints}"
: "${OUT_DIR:=/catalog}"
: "${SYNC_INTERVAL:=0}"
while true; do
  catalog-sync --models "$MODELS" --endpoints-dir "$ENDPOINTS_DIR" --out "$OUT_DIR" --process
  if [ "$SYNC_INTERVAL" -le 0 ]; then
    break
  fi
  sleep "$SYNC_INTERVAL"
done
