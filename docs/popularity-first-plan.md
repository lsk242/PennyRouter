# Implementation Plan: Popularity-First Ranking

**Depends on:** `docs/proposals/2026-11-18-popularity-first-ranking.md`

This plan turns the proposal into small, testable iterations. Each ships
one layer, is testable alone, and adds value on its own.

## Iter 38 — Coding-app popularity data

**Goal:** Collect and store coding-focused popularity data from OpenRouter.

**Value:** The gateway knows which models developers actually use most (Cline,
Cursor, etc.), not just what's on the menu.

**Shipped:**
- `crates/catalog/src/popular.rs`: `fetch_coding_popularity(base_url, api_key,
  coding_apps)` — fetches `rankings-daily`, filters to `coding_apps` allowlist,
  aggregates tokens per model, returns `CodingPopularity { models: Vec<(String, u64)>, apps: Vec<String>, as_of }`.
- `CODING_APPS` config: env var (comma-separated) or `coders.json` file.
  Default list ships as a constant in `popular.rs`.
- Store in catalog meta: `coding_popular_models`, `coding_popular_apps`,
  `coding_popular_as_of` (new keys; old artifacts read as empty).
- `catalog-sync` passes the coding apps list through to `fetch_popularity`.

**Tests:**
- `crates/catalog/tests/popular_coding.rs`: mock rankings body → filter → aggregate
- Filter: only coding apps counted, `other` row skipped, unparseable dropped
- Empty allowlist → falls back to all-model popularity
- `store/read` round trip for new meta keys

**Config:**
```bash
# .env.example
CODING_APPS=Cline,Roo Code,Cursor,Continue,Zed,Aider,GitHub Copilot,Windsurf
```

## Iter 39 — Popularity-first ranking orientation

**Goal:** A new `popularity_first` ordering that puts coding-popular models first,
then price-segments into low/think tiers.

**Value:** Routing decisions match what developers actually use.

**Shipped:**
- `crates/catalog/src/rank.rs`: new `RankOrientation::PopularityFirst`
- `compare()` arm for `PopularityFirst`:
  1. Popularity score (descending) — models not in popularity data sort last
  2. Tier (high > standard > low) — quality floor within popularity
  3. Cost (ascending) — cheapest wins within tier
  4. Error rate (ascending)
  5. Latency (ascending)
  6. Provider identity (tie-breaker)
- `rank_catalog()` produces `ClassRanking.popularity_first: Vec<RankEntry>`
- `RankingDocument` gains `popularity_first` field per class (optional,
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]`).

**Tests:**
- `crates/catalog/tests/rank_popularity.rs`:
  - Popular model beats unpopular even if pricier
  - Within same popularity tier, cheapest wins
  - Unknown popularity sorts last
  - Tier floor applies within popularity

## Iter 40 — Gateway: popularity-first routing

**Goal:** Gateway reads and uses the popularity-first ordering.

**Value:** `openrouter/pair/low` picks the cheapest coding-popular model;
`openrouter/pair/think` picks the best coding-popular model.

**Shipped:**
- `crates/gateway/src/routing.rs`:
  - `ordered()` in `decide_ordered_virtual`: when `popularity_first` ranking
    exists for the model, use it; otherwise fall back to inline `compare()`
  - New `VirtualTier::Low` picks cheapest from popularity top-quartile
  - New `VirtualTier::Think` picks highest-quality from popularity top-decile
- Fallback chain: popularity_first → default → price → quality
- New virtual ID: `openrouter/pair/coding` (cheapest coding-popular)
- New virtual ID: `openrouter/pair/coding/think` (best coding-popular)

**Tests:**
- `crates/gateway/tests/popularity_routing.rs`:
  - Popular model routes over unpopular (even if pricier)
  - `coding` tier picks cheapest popular
  - `coding/think` picks best popular
  - Fallback to default when no popularity data
  - `x-pennyrouter-tier: coding` header

## Iter 41 — Long-tail min-value fallback

**Goal:** For models outside the popularity tiers, always route to the
cheapest eligible offering per type.

**Value:** Rare models still get served at the best possible price.

**Shipped:**
- `crates/gateway/src/routing.rs`:
  - `decide_ordered_virtual`: when model not in popularity data, use
    `RankOrientation::Price` (cheapest) instead of quality-first
  - `decide_pinned`: same — unknown model → cheapest provider
- New rejection code: `no_eligible_offering` (unchanged) but message clarifies
  "no popularity data, falling back to cheapest"

**Tests:**
- `crates/gateway/tests/popularity_routing.rs`:
  - Unknown model → cheapest provider
  - `openrouter/pair/model/rare-model` → cheapest offering
  - Popularity tier > price tier for known models

## Iter 42 — UI: popularity visibility

**Goal:** Show popularity data in the debug UI.

**Value:** Operators can see why a model was chosen (popularity rank + tier).

**Shipped:**
- `crates/gateway/src/lib.rs` `ui_page()`:
  - Overview: "Top coding-popular models" section (top 10 with token counts)
  - Models: popularity column per model (rank, tokens)
  - Inspector: popularity-aware preview (shows popularity rank in candidate table)
- New metrics: `pennyrouter_popularity_models_total{model, tier}`,
  `pennyrouter_popularity_tokens_total{model}`
- `/ui` Sessions tab: shows tier (low/think/coding) per pin

**Tests:**
- `crates/gateway/tests/ui.rs`:
  - Popularity section appears with top models
  - Models table shows popularity rank
  - Inspector shows popularity in candidate rows
  - Metrics include popularity counters

## Iter 43 — Default switch + validation

**Goal:** Make popularity-first the default for `low`/`think`, with rollback.

**Value:** The system learns what developers want and routes accordingly.

**Shipped:**
- Config: `ROUTING_DEFAULT=popularity_first|quality_first` (default: `popularity_first`)
- A/B validation: 10% traffic → popularity_first, measure error/latency
- Rollback: `ROUTING_DEFAULT=quality_first` reverts instantly
- Documentation update: `docs/routing.md` reflects new default
- `TODO.md` + `docs/roadmap.md` updated

**Tests:**
- Config flag honored
- Fallback to quality_first when flag set
- Metrics compare the two orderings

## Dependencies

```
Iter 38 (data)
  └─▶ Iter 39 (ranking)
       └─▶ Iter 40 (routing)
            ├─▶ Iter 41 (long-tail)
            └─▶ Iter 42 (UI)
                 └─▶ Iter 43 (default switch)
```

## Rollout Plan

1. **Iter 38-39**: Ship data + ranking. No gateway behavior change.
2. **Iter 40-41**: Ship routing. Default stays `quality_first`.
3. **Iter 42**: Ship UI. Operators can compare.
4. **Iter 43**: Switch default. Monitor. Rollback if needed.

## Open Decisions

1. **Coding app list**: Initial list in proposal. Who maintains it?
2. **Time window**: 7 days (current) vs 30 days (more stable)?
3. **Popularity floor**: What minimum token count makes a model "popular"?
   (e.g., 1M tokens/week?)
4. **Tier split**: Top 25% for `low`, top 10% for `think` — or absolute
   thresholds?