#!/bin/sh
# One sync+process+rank cycle into $OUT_DIR, then sleep-loop when
# SYNC_INTERVAL > 0. Input defaults suit the compose mount layout.
set -eu
: "${MODELS:=/input/models.json}"
: "${ENDPOINTS_DIR:=/input/endpoints}"
: "${OUT_DIR:=/catalog}"
: "${SYNC_INTERVAL:=0}"
: "${TRENDS:=/input/trends.json}"
: "${ASSIST:=/input/rank-assist.json}"
while true; do
  catalog-sync --models "$MODELS" --endpoints-dir "$ENDPOINTS_DIR" --out "$OUT_DIR" --process
  DB=$(ls -t "$OUT_DIR"/catalog-*.db | head -1)
  RANK_ARGS=""
  [ -f "$TRENDS" ] && RANK_ARGS="$RANK_ARGS --trends $TRENDS"
  [ -f "$ASSIST" ] && RANK_ARGS="$RANK_ARGS --assist $ASSIST"
  # Unquoted RANK_ARGS: empty by default, splits into flags when set.
  catalog-rank --db "$DB" --out "$OUT_DIR" $RANK_ARGS
  if [ "$SYNC_INTERVAL" -le 0 ]; then
    break
  fi
  sleep "$SYNC_INTERVAL"
done
