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

## Phase 2 — Processing

- Goal: an AI-assisted job distills raw payloads into fixed indexed
  columns (normalized prices, precision class, quality signals) — the
  queryable projection routing and ranking read.
- Value: stable, comparable facts for ranking without losing any raw
  input; the fixed schema grows only with proven index needs.
- Test: processing fixtures yield the expected indexed values; raw
  payloads preserved.

## Later

Additional sources beyond OpenRouter, scheduling/ops hardening, and
retention policy — pulled forward deliberately, never by default.
