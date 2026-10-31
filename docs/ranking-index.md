# Ranking Index

The key differentiator: a locally stored, continuously re-ranked provider
index that makes realtime routing a single lookup. (Catalog in Iter 4,
ranking in Iter 6.)

## Inputs

Gather everything available, OpenRouter first, other sources later:

- Price parts (input, output, cache read/write), promos and free windows.
- Latency, throughput, uptime/availability.
- Privacy flags (zero data retention), region, model/provider type.
- Numeric precision / FP resolution (FP8/FP16/BF16/FP32, quantization):
  the same model id at different precision is not the same offering.
- Index/score signals where provided.

The field list is dynamic and fluid — sources change constantly, and the
catalog must absorb new fields without a redesign.

## Ranking job

A periodic background job (AI-assisted) rebuilds the ranking per model:

- Blend live metrics with trends of top models per application — existing
  trends often beat lagging raw metrics.
- Treat free/promo pricing as time-boxed facts with expiry, including
  partial discounts; provider-scoped, never assumed global.
- Emit a stable ordering the realtime path reads with one lookup.
- Publish an immutable, versioned snapshot atomically, only after the
  durable catalog writes commit.

## Trends

Keep per-app (per key) usage trends: which models/providers an application
actually uses and how they behaved. Trend data feeds the ranker and
improves defaults for callers that pass no filters.

## Comparability

Same model id does not mean comparable: precision, quantization, and
provider-specific tuning change quality and effective price. The index
compares within a precision class and never silently substitutes across
precision — a cheaper FP8 offer is not automatically "the same model
cheaper".

## Realtime guarantee

Ranking work never happens per request. If the ranker lags or fails, the
router keeps serving the last good index.

## Status (Iter 6)

- `catalog-rank --db <catalog.db> --out <dir> [--trends <t.json>]
  [--assist <a.json>] [--promos <p.json>]` rebuilds the ranking
  offline and publishes `ranking-<version>.json` + manifest sidecar
  (content-hash versioned, hash-verified on load, like the catalog
  artifact).
- Ordering per model, precision class, and orientation (default/price/
  quality). Formulas, best-first: default = tier, error rate, price,
  latency; price = price, error rate, tier, latency; quality = tier,
  score, error rate, price; provider/tag break ties so rebuilds are
  byte-stable. Unknown error rates count as zero; unknown latencies
  sort after known ones.
- Trends file: per-app `{app, model_id, provider_name, requests,
  errors, avg_latency_ms}` records aggregated per (model, provider).
  Live traffic collection is future work — the file is the interface.
- Assist file: recorded `{model_id, provider_name, action:
  prefer|avoid, reason}` overrides (no live model calls); prefer
  floats a provider to the front of its group, avoid sinks it.
  Unknown records are ignored, never an error.
- Gateway: `CATALOG_DB` directories auto-load the newest verified
  ranking; uncovered models fall back to the same formulas inline;
  stale entries are skipped and new offerings append, so a stale
  ranking never drops a live offering. `reload`/`swap_ranking` rotate
  artifacts without dropping the last good one.
- Promo/free windows landed with the price model (Iter 7): `--promos`
  consumes provider-scoped windows (see [pricing.md](pricing.md)),
  and promo-free eligibility publishes in the ranking document
  (`free_offers`, Iter 11).
