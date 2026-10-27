# Catalog Sync Roadmap

Phased plan for the sync process. Each phase ships one small layer:
testable by itself, valuable by itself. No phase may depend on a later
one. Gateway-side milestones stay in [roadmap.md](roadmap.md).

## Phase 1 — Gathering

- Goal: `catalog-sync` pulls models/providers from OpenRouter into the
  catalog DB: fixed core columns plus JSON payload columns preserving
  the raw source data. Publishes a versioned DB artifact.
- Value: the raw data foundation exists and is inspectable; later phases
  and the gateway build on a stable artifact.
- Test: fixture sync populates the DB; queries return expected rows;
  artifact carries version and checksum.

## Phase 2a — Processing (deterministic)

- Goal: a processing job distills raw payloads into fixed indexed columns
  with pure-code extraction (normalized price parts, precision class from
  quantization, promo discount) — the queryable projection routing and
  ranking read.
- Value: stable, comparable facts for ranking without losing any raw
  input; the fixed schema grows only with proven index needs.
- Test: processing fixtures yield the expected indexed values; raw
  payloads preserved.

## Phase 2b — Processing (AI-assisted)

- Goal: model-judged signals (e.g. quality flags) fill further indexed
  columns behind the hook Phase 2a establishes.
- Value: judgment-based facts that pure extraction cannot produce.
- Test: fixtures with recorded model outputs yield the expected indexed
  values; no live model calls in tests.

## Phase 3 — Local deployment

- Goal: the sync runs containerized on a schedule, sharing its DB
  artifact with the gateway through a mounted volume (gateway
  read-only); CI gates the Rust workspace and the catalog image.
- Value: the deployment shape is proven early — image, schedule,
  volume contract — before live data and gateway consumption land.
- Test: catalog image builds; one-shot container publishes an artifact
  a read-only mount can see; compose file renders.

## Later

Additional sources beyond OpenRouter, live fetching, remote artifact
delivery, retention policy — pulled forward deliberately, never by
default.
