# Implementation Plan: Catalog score + popularity-first ranking

**Depends on:** `docs/proposals/2026-11-18-popularity-first-ranking.md`

This plan turns the proposal into small, testable iterations. Each ships
one layer, is testable alone, and adds value on its own. Rule for every
fetch added here: sync-time only, never the request path, at most 1
request/hour per source (self-imposed; upstream tiers allow more).

## Iter 42 — Score data sources (AA + OpenRouter Data API)

**Goal:** Download everything the score needs, versioned and cited.

**Value:** The catalog stops depending on embedded payloads alone:
direct AA indexes with test versions and cost/task, plus activity
level + trend from the rankings window.

**Shipped:**

AA API (`crates/catalog/src/aa.rs`, new):

- `fetch_aa_models(base_url, api_key)` — `GET
  /api/v2/language/models/free` with `x-api-key` (`AA_API_KEY`);
  parses headline indexes, `intelligence_index_version`,
  `cost_per_task.total_cost`, pricing, median performance.
- Store per model: `aa_intel_index`, `aa_intel_version`,
  `aa_cost_per_task`, `aa_pricing`, plus `aa_as_of` in meta.
  Keyless syncs store nothing; old artifacts read as empty.

// Fetch policy: at most 1 request/hour to the AA Data API per sync
// host (self-imposed; the free tier allows far more). Sync-time
// only, never the request path.

OpenRouter Data API (`popular.rs` + `benchmarks.rs`, extended):

- Rankings window grows to recent + 3 prior windows for trend
  (same `rankings-daily` endpoint, one call with a wider span);
  `aggregate_activity()` returns level (recent share) + trend
  (recent vs prior mean) per slug; store as `activity_tokens`.
- `app-rankings` with `sort=trending` + `category=coding` for the
  hot/coding app display (names only — the API has no per-app
  models, so this never filters models).
- Task classifications Code share stored as `task_code_share`
  (coding weight); benchmarks keep the intel backfill and add
  Design Arena code + OpenRouter GPQA/tau-bench fallbacks.

// Fetch policy: at most 1 request/hour to the OpenRouter Data API
// per endpoint group per sync host (self-imposed; upstream allows
// 30/min per key, 500/day per account). Sync-time only, never the
// request path.

**Tests:**

- `crates/catalog/tests/aa_fetch.rs`: fixture free-shape body →
  parse → store → read round trip; keyless → empty, no penalty.
- `crates/catalog/tests/popular_coding.rs` → renamed
  `activity_trend.rs`: multi-window fixture → level + trend;
  `other` skipped; unparseable dropped; single-window → trend
  neutral; store/read round trip for new meta keys.
- Citation: `as_of` present for every stored source.

**Config:**

```bash
# .env.example
AA_API_KEY=
# RANKINGS_API_KEY=  (already exists; unchanged)
```

## Iter 43 — Catalog score v2 + brackets

**Goal:** One versioned score per offering plus the top-usage
bracket, computed offline in a single transaction.

**Value:** Routing inputs become inspectable facts: performance ×
efficiency × activity with version gating, and declared bracket
ends for `low`/`think`.

**Shipped:**

- `crates/catalog/src/rank_score.rs`: `RULESET_VERSION =
  "rank-v2"`; `score()` gains activity input (level × bounded
  trend boost) and the version gate (index counts only on the
  current `aa_intel_version`, else `NULL`); AA `cost_per_task`
  wins when present, else catalog cost × `TASK_TOKENS`.
- Brackets (`process.rs`): top-usage cutoff (top-N + minimum
  share, declared constants); `low` end = cheapest at/above the
  performance floor; `think` end = best performance under
  `THINK_MAX_COST_TASK`; flags per model (`stale-version`,
  `no-current-score`, `popular-but-unscored`, `no-price`,
  `no-activity`).
- Model-level TOP rollup: new `top_models` meta (generic models,
  best score + components + flags each) — the TOP tab reads this,
  never recomputes.
- Schema v9→v10, additive only: `aa_intel_version`,
  `activity_level`, `activity_trend`, `rank_activity`,
  `rank_ruleset` already exists; migration via the existing
  `has_column` + `ALTER TABLE` loop.

**Tests:**

- Version gating: current in, stale out, missing → `NULL`.
- Trend math: rising > flat > falling at equal level; neutral on
  one window.
- Bracket ends on fixtures: `low` cheapest-above-floor, `think`
  best-under-cap, over-cap excluded, popular-unscored flagged.
- `lowest` semantics untouched (full-list cheapest, no floor).
- v9→v10 migration idempotent; unprocessed rows keep `NULL`.

## Iter 44 — Gateway consumes the score

**Goal:** Routing reads the published score/ordering and stops
recomputing quality math.

**Value:** `low`/`think` serve the bracket ends from one lookup;
`lowest` keeps full-list cheapest; scoring changes ship without
gateway deploys.

**Shipped:**

- `crates/gateway/src/routing.rs`:
  - `decide_ordered_virtual` orders by the published `score`
    ranking for `low`/`think` (floor = bracket membership +
    existing budget/tool/context gates); no inline tier blending.
  - `lowest` stays full-list cheapest (`Price` orientation) with
    `/minindex/` filtering; activity breaks price ties.
  - Fallback chain: score → legacy tier ordering → cheapest.
- No new virtual ids: `low`/`think`/`lowest` already name the
  bracket ends and the full list.

**Tests:**

- `crates/gateway/tests/score_routing.rs`:
  - `low` picks cheapest-above-floor from the bracket fixture.
  - `think` picks best-under-cap; over-cap never wins.
  - `lowest` picks full-list cheapest incl. out-of-bracket.
  - Missing score column → legacy ordering (rollback path).
  - Budget/tool/context gates still apply on top of score order.

## Iter 45 — Long-tail min-value fallback

**Goal:** For models outside the bracket, always route to the
cheapest eligible offering per type.

**Value:** Rare models still get served at the best possible price.

**Shipped:**

- `crates/gateway/src/routing.rs`:
  - `decide_ordered_virtual`: model absent from the bracket uses
    `RankOrientation::Price` (cheapest) instead of score order.
  - `decide_pinned`: same — unknown model → cheapest provider.
- Rejection message clarifies "outside the top-usage bracket,
  falling back to cheapest" (code unchanged).

**Tests:**

- `crates/gateway/tests/score_routing.rs`:
  - Unknown model → cheapest provider.
  - `openrouter/pair/model/rare-model` → cheapest offering.
  - Bracket members never take the long-tail path.

## Iter 46 — UI: catalog TOP tab

**Goal:** Show the score, its arithmetic, and its flags where the
score is made.

**Value:** Operators debug routing inputs without guessing: every
bracket decision is inspectable per model.

**Shipped:**

- `crates/catalog-ui/src/main.rs`:
  - `/models?tab=top`: generic models only (best score +
    components + activity/trend + flags each), score desc;
    expandable arithmetic per row; `?q=` + `?min=` filters.
  - Overview: bracket counts (in-bracket, stale-excluded,
    popular-unscored flagged) + per-source fetch health (`as_of`,
    version, last ok).
- `crates/gateway/src/lib.rs` inspector: score-rank column only.
- Metrics: `pennyrouter_score_models_total{basis,bracket}`,
  `pennyrouter_score_flag_total{flag}`,
  `pennyrouter_sync_fetch_total{source,result}`.

**Tests:**

- `crates/catalog-ui/tests/ui.rs`:
  - TOP tab lists bracket models score-desc with arithmetic.
  - Flags render exactly when set; unscored popular flagged.
  - Overview shows bracket counts + fetch health.
  - Empty/unscored artifact degrades gracefully, no NaN.

## Iter 47 — Default switch + validation

**Goal:** Make the score ordering the default for `low`/`think`,
with rollback.

**Value:** The system routes on usage-backed value instead of
static tier sorts.

**Shipped:**

- Config: `ROUTING_DEFAULT=score_first|legacy` (default:
  `score_first`).
- A/B validation: 10% traffic → score order, measure
  error/latency/cost vs legacy.
- Rollback: `ROUTING_DEFAULT=legacy` reverts instantly.
- Docs: `docs/routing.md` reflects the new default; `TODO.md` +
  `docs/roadmap.md` updated.

**Tests:**

- Config flag honored; legacy fallback works.
- Metrics compare the two orderings.

## Iter 48 — Coding tier family (depends on Iter 44)

**Goal:** `openrouter/pair/coding[/think]` (+ `privacy/`) as
popularity-first picks alongside the score-first tiers.

**Value:** Operators get the most-used bracket in legacy
price/quality order — more options to select, no score math.

**Shipped:**

- `crates/gateway/src/routing.rs`:
  - `Coding`/`CodingThink` tiers through `decide_ordered_virtual`
    with the legacy comparators and floors, pool scoped to the
    bracket; fail closed without bracket data (never a silent
    full-list serve).
  - `minindex` composes; valid `/fallback/` targets; no
    `/fallback/` suffix of their own.
  - Menu grows to 20 ids, inspector fixed combos to 16.
- Docs: proposal revision note, routing/API/operations counts.

**Tests:**

- `crates/gateway/tests/coding_tiers.rs`:
  - `coding` → cheapest in-bracket; `coding/think` → best quality
    in-bracket; out-of-bracket traps lose both.
  - Empty bracket fails closed (route + preview).
  - Privacy variant, `minindex` composition, fallback target, menu.

## Dependencies

```
Iter 42 (data)
  └─▶ Iter 43 (score + brackets)
       └─▶ Iter 44 (gateway consume)
            ├─▶ Iter 45 (long-tail)
            ├─▶ Iter 48 (coding tiers)
            └─▶ Iter 46 (TOP tab)
                 └─▶ Iter 47 (default switch)
```

## Rollout Plan

1. **Iter 42-43**: Ship data + score. No gateway behavior change.
2. **Iter 44-45**: Ship consumption. Default stays `legacy`.
3. **Iter 46**: Ship TOP tab. Operators compare score vs legacy.
4. **Iter 47**: Switch default. Monitor. Rollback if needed.

## Open Decisions

1. **AA Coding/Agentic retirement**: verify live payloads in Iter
   38 — decides the basis chain and whether `minindex` follows.
2. **Bracket cutoff**: top-N + minimum share values (proposal
   suggests 25 + 0.5%).
3. **Think max spend**: `THINK_MAX_COST_TASK` value (proposal
   suggests $0.05/task), versioned with the ruleset.
4. **Trend window**: 7-day recent vs 3 priors, or 30-day smoothing?
5. **`cost_per_task` coverage**: Iter 42 measures it; v2 uses AA
   cost only where universal.
