# Ranking Index

The key differentiator: a locally stored, continuously re-ranked provider
index that makes realtime routing a single lookup. (Catalog in Iter 4,
ranking in Iter 6.)

## Inputs

Gather everything available, OpenRouter first, other sources later:

- Price parts (input, output, cache read/write), promos and free windows.
- Latency, throughput, uptime/availability.
- Privacy flags (zero data retention), region, model/provider type.
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

## Trends

Keep per-app (per key) usage trends: which models/providers an application
actually uses and how they behaved. Trend data feeds the ranker and
improves defaults for callers that pass no filters.

## Realtime guarantee

Ranking work never happens per request. If the ranker lags or fails, the
router keeps serving the last good index.
