# Proposal: Catalog score + popularity-first ranking

**Date:** 2026-11-18 · **Revised:** 2026-11-20, 2026-11-24
**Status:** Proposed
**Authors:** leandro

> Revision note: the 2026-11-18 draft filtered rankings per coding
> app, but the rankings API has no app dimension, so that filter is
> unimplementable. This revision replaces it with all-model activity
> + trend, makes the catalog score (not a new lexicographic order)
> the routing input, moves the TOP view into the catalog UI, and
> drops the `coding/*` virtual ids — `low`/`think` already are the
> popular-bracket tiers.
>
> 2026-11-24: the `coding/*` family is restored (Iter 48) with new
> semantics — not per-app popularity (still impossible) but the
> `low`/`think` legacy orderings scoped to the most-used bracket:
> popularity-first picks alongside the score-first tiers.

## Executive Summary

The catalog owns scoring; the gateway consumes it. Each sync, the
catalog downloads everything it needs (AA free API + OpenRouter Data
API), runs one deep analytical step, and publishes a versioned score
and ordering per offering. Gateway routing stays a single lookup: it
reads the published ranking/score and never recomputes it.

Usage drives the ranking. `low` and `think` serve only the top-usage
bracket — `low` is the cheapest offering with a performance floor,
`think` the best performance under a max spend — while `lowest`
keeps the entire list for cheapest-overall with `minindex`
filtering. Intelligence indexes stop being the primary key and become
the performance input, always pinned to the latest test version:
models without the current version are excluded from the bracket,
and popular ones missing it are flagged.

## Contract: catalog → gateway

- The catalog publishes, per offering: `rank_score` + components
  (performance, efficiency, activity), `rank_basis`,
  `rank_ruleset`, and the `score` ordering in the ranking
  document. One ruleset version per artifact.
- The gateway reads the score/ordering and applies only
  request-scoped gates (budget, `only`/`ignore`, tools, context,
  throttle, stickiness). No price blending, no tier math on the
  request path.
- A scoring change ships as a new ruleset version in the artifact.
  Gateway code does not change when the formula does.

## Current system (as of Iter 36)

- `rank_score` (`rank-v1`, Iter 34): `quality × efficiency ×
  popularity`, with precision discounting and a saturating price
  curve. Published per offering; the gateway ignores it.
- Gateway orientations (`rank.rs`): `Default`/`Price`/`Quality`
  lexicographic sorts, plus `low`/`think` quality floors
  (`low` needs any scored tier, `think` needs `high`).
- Popularity (`popular.rs`): 7-day `rankings-daily` window, top-10
  menu pins plus the full slug→tokens map behind the ≤10%
  popularity factor.
- AA signals: embedded `benchmarks.artificial_analysis.*` per model
  (Iter 31) plus the intelligence backfill from the benchmarks API.
- Catalog UI: overview + models (`free`/`tiers`/`batch`/`decision`),
  no TOP view, no model-level rollup.

**Limitations:**

- The score is published but never consumed — routing still sorts
  by tier, then error rate, then price.
- Popularity is a static 7-day sum: no trend, no activity signal.
- No test-version gating: a stale index scores like a current one,
  and popular models without any score fail silently.
- The catalog UI cannot show "top models with score arithmetic".
- `rankings-daily` rows carry no app dimension, so per-model
  "coding app" filtering is impossible (see §4).

## Data sources

Two sources, both sync-time only, never the request path. Both get
a self-imposed fetch cap so syncs stay polite and artifacts stay
reproducible instead of rate-driven.

### 1. AA Data API (free tier)

`GET https://artificialanalysis.ai/api/v2/language/models/free`
with an `x-api-key` (`AA_API_KEY`). Free-shape fields: headline
Intelligence/Coding/Agentic indexes, `intelligence_index_version`,
the six Capability Indexes, median performance, Performance v3
headline means, input/output pricing, and the Intelligence Index run
cost including `cost_per_task.total_cost`. Provider detail, full
eval sets, percentiles, and context/params stay Pro-only.

```rust
// Fetch policy: at most 1 request/hour to the AA Data API per sync
// host (self-imposed; the free tier allows far more). Sync-time
// only, never the request path.
```

Stored per model: indexes + `intelligence_index_version` +
`cost_per_task` + pricing + `aa_as_of`. The version gates the top
bracket (§5): only the current version counts, and the sync records
which version that is so staleness is visible in the artifact.

### 2. OpenRouter Data API

Any valid OpenRouter key (`RANKINGS_API_KEY`; the inference key is
fine). Upstream limits: 30 requests/minute per key, 500/day per
account; data is CC-BY and every figure carries its `meta.as_of`
citation, as the existing popularity fetch already does.

```rust
// Fetch policy: at most 1 request/hour to the OpenRouter Data API
// per endpoint group per sync host (self-imposed; upstream allows
// 30/min per key, 500/day per account). Sync-time only, never the
// request path.
```

Endpoints used:

- `rankings-daily`: top 50 models/day + `other` row as
  `(date, model_permaslug, total_tokens)`, window up to 366 days.
  Feeds the activity level (recent-window share) and the trend
  (recent window vs the windows before it). Tokenizers differ per
  provider, so cross-provider sums are coarse magnitudes.
- `app-rankings`: top apps by tokens, `sort=popular|trending`,
  `category=coding` for the coding display. Trending (excess growth
  vs the prior three windows) is the closest native "what is hot"
  signal. It has no per-app models, so it cannot filter models —
  it only names the apps and the trend.
- `benchmarks`: keeps the AA intelligence backfill; adds Design
  Arena code categories and OpenRouter's own GPQA/tau-bench as
  fallbacks. Note: the docs say AA retired its Coding/Agentic
  indexes for `task_type` filtering — verify against live payloads
  before trusting any non-intelligence basis (open question 1).
- Task classifications: sampled relative traffic shares with a
  Code macro-category — a coding weight when a per-model coding
  join is unavailable (which is always: no endpoint joins apps to
  models).

## Score calculation (catalog-owned, `rank-v2` sketch)

Per offering, all terms stored so the UI shows arithmetic and the
gateway reads one column:

```
performance  = gated_index × precision_factor        # 0..100
efficiency   = COST_HALF_CREDIT / (cost_task + COST_HALF_CREDIT)
activity     = level × (1 + trend_boost)              # bounded
rank_score   = performance × efficiency × activity
```

- `gated_index`: the model's index on the *current* test version
  only (Intelligence vN first; Coding/Agentic only if the
  retirement question resolves in their favor). No current version
  → no score (`NULL`), never zero.
- `precision_factor`: keep the `rank-v1` table (fp32/fp16 1.00,
  fp8 0.97, int8 0.95, int4 0.88, unknown 0.95). Quantization is
  part of performance, not a tie-break.
- `cost_task`: AA `cost_per_task.total_cost` wins wherever it is
  universal; otherwise the catalog's own effective cost ×
  `TASK_TOKENS`, same curve as `rank-v1`.
- `activity`: recent-window token share (level) times a bounded
  trend multiplier (rising vs prior windows). This is the usage
  signal that drives the ranking; price, availability, and speed
  tune inside it via efficiency and the existing tie-breaks
  (throughput → latency → uptime → provider/tag).
- Constants stay in code with the ruleset version, never env knobs.

## Brackets and tier names

- **Top-usage bracket**: models ordered by activity; the cutoff is
  a declared rule (top-N + minimum share — open question 2). Only
  scored (current-version) models enter. A popular model without
  the current version is excluded *and flagged* (§8).
- **`think`** = top of the bracket: best performance index inside
  the bracket under a declared max spend (`THINK_MAX_COST_TASK`).
- **`low`** = bottom of the bracket: cheapest offering in the
  bracket at or above a declared performance floor. Prices move
  models up/down the bracket every rebuild — dynamic, not pinned.
- **`lowest`** (existing tag, unchanged meaning): cheapest paid
  offering over the *entire* list, no quality floor, combinable
  with `/minindex/` to filter up/down. Activity still orders equal
  prices so trending cheap models surface.
- **`free` / `decision` / `batch`**: unchanged scopes (zero-cost,
  decisions-capable, `:batch` suffix). They read the same score
  where it exists but keep their own eligibility rules.
- **`coding` / `coding/think`** (restored 2026-11-24, Iter 48):
  the `low`/`think` legacy orderings (cheapest / best quality
  signals) scoped to the most-used bracket — popularity-first
  picks alongside the score-first tiers. No bracket data means
  no pool (fail closed). The 2026-11-20 draft had dropped this
  family as redundant; operator demand restored it with
  bracket-scoped (not per-app) semantics.

Top of the activity list never means "best" by itself — the score
always blends performance × price efficiency × activity, and the
TOP tab shows all three so the tradeoff is inspectable.

## Long tail: minimum value

Models outside the bracket (unpopular, unscored, or long-tail
direct pins like `openrouter/pair/model/<id>`) route to the
cheapest eligible offering per type. Unknown is cheapest-first,
never quality-first: a rare model still serves at the best price.

## Data flow

```text
AA free API ──┐
              ├─▶ catalog-sync (≤1 req/hour/source) ─▶ catalog DB ─▶ ship
OpenRouter    │      │                                     │
Data API ─────┘      ▼                                     ▼
              deep analysis step              gateway hosts: atomic
              (score v2 + brackets             swap → snapshot rebuild
              + flags, 1 txn)                  → in-memory publish
                                                     │
                                                     ▼
                                              gateway: single lookup
                                              of score/ordering +
                                              request gates only
```

## Reliability & fallbacks

| Scenario | Behavior |
|----------|----------|
| AA API down | Keep last-good AA payload + version; scores carry the old `aa_as_of`, TOP tab warns |
| Rankings API down | Keep last-good activity window; trend freezes, level persists |
| No AA key | No version gate possible: bracket falls back to embedded AA indexes, TOP tab warns "AA direct missing" |
| No rankings key | No activity: score degrades to performance × efficiency, brackets fall back to price segments |
| No score at all | Gateway falls back to the current tier ordering (rollback path) |
| Stale artifact | Gateway keeps serving last-good; boot without a scored artifact stays in standby passthrough |

## Catalog TOP tab (debuggable output)

The catalog UI gains a `top` tab: generic models only (no
providers — per-provider quantification stays in the offering
rows), ordered by rank score, each row expandable to the full
arithmetic:

```text
activity     = share 0.42 × trend +12%              = 0.470
performance  = 71 (intel v5) × 0.97 (fp8)           = 68.87
cost/task    = $0.012825 (AA)                       → eff 0.438
rank_score   = 68.87 × 0.438 × 0.470                = 14.18
bracket      = top-usage #6 · think-eligible (under cap)
```

- Flags column: `stale-version`, `no-current-score`,
  `popular-but-unscored`, `no-price`, `no-activity`.
- Filters: `?q=` substring, `?min=` minimum score — same as the
  other tabs, no JS.
- The overview page adds bracket counts (in-bracket, excluded
  stale, flagged popular-unscored) next to the existing coverage.
- Gateway `/ui` gets only a score-rank column in the inspector:
  deep debugging lives in the catalog, where the score is made.

## Migration path

**Phase 1: Data (no behavior change).** Add the AA fetch and the
extended OpenRouter fetches (trend window, app-rankings, task
classes) with the 1/hour policies; store versioned payloads; old
artifacts read as empty.

**Phase 2: Score.** Ship `rank-v2` + brackets + TOP tab. Gateway
still routes on the old ordering; operators compare in the UIs.

**Phase 3: Consume + switch.** Gateway reads the score ordering
behind `ROUTING_DEFAULT` (`score_first` vs legacy), validated by
A/B, with instant rollback to the legacy ordering.

## Metrics & observability

```text
pennyrouter_score_models_total{basis,bracket}
pennyrouter_score_flag_total{flag}
pennyrouter_activity_share{model}
pennyrouter_sync_fetch_total{source,result}
```

Catalog overview shows fetch health per source (last ok, `as_of`,
version); gateway metrics count served brackets per tier.

## Testing strategy

- **Unit:** version gating (current in, stale out, missing
  `NULL`); trend math (rising > flat > falling at equal level);
  bracket ends (`low` cheapest-above-floor, `think`
  best-under-cap); `lowest` full-list cheapest; flags fire exactly
  when defined.
- **Integration:** fixture AA + rankings bodies → sync → artifact
  carries versioned scores, brackets, flags; keyless syncs degrade
  per the fallback table; v9→v10 migration is additive.
- **Live validation:** one-shot score-vs-legacy routing diff on a
  real artifact; A/B 10% traffic on error/latency/cost before the
  default switch.

## Open questions

1. **AA Coding/Agentic retirement:** OpenRouter docs say retired
   for `task_type`; the AA free shape still lists them. Check live
   payloads — if retired, the basis chain becomes Intelligence vN
   (+ Capability Indexes later), and `minindex` follows it.
2. **Bracket cutoff:** top-N (e.g. 25) + minimum share (e.g. 0.5%
   of window tokens)? Absolute thresholds beat percentages for
   debuggability.
3. **Think max spend:** one `THINK_MAX_COST_TASK` constant (e.g.
   $0.05/task)? It must live with the ruleset version, not env.
4. **Trend window:** 7-day recent vs prior 7-day ×3, or 30-day
   smoothing? Start with 7-day to match the existing fetch.
5. **`cost_per_task` universality:** if AA lacks it for many top
   models, keep the catalog-computed cost for v2 and revisit.

## Risks & mitigations

| Risk | Mitigation |
|------|------------|
| Activity gaming / bot traffic | Trend + performance floor bound the damage; a pure-popularity spike cannot outrank a scored model |
| Tokenizer incomparability | Shares are coarse magnitudes; brackets use wide cutoffs, never adjacent-rank decisions |
| Stale AA version | Version travels in the artifact; gateway/TOP tab warn instead of silently serving |
| Score churn per rebuild | Bracket cutoffs are wide; stickiness still absorbs per-request flapping |
| Extra sync fetches | 1/hour/source caps; two sources, small bodies, sync-time only |

## Success criteria

1. TOP tab lists the top-usage bracket with exact arithmetic and
   zero unexplained exclusions (every exclusion has a flag).
2. `low`/`think` route to the bracket ends on a live artifact;
   `lowest` still serves full-list cheapest.
3. Routing latency unchanged (offline compute, single lookup).
4. Error/latency/cost neutral-or-better in A/B before the switch.

## References

- [AA Data API reference](https://artificialanalysis.ai/data-api/docs/reference)
- [OpenRouter Data API](https://openrouter.ai/docs/cookbook/administration/data-api)
- [value-score proposal](2026-11-12-catalog-ui-value-score.md) (rank-v1 this extends)
- [AA index ingestion](2026-11-03-artificial-analysis-index.md) (embedded signal)
- `crates/catalog/src/rank_score.rs` (score), `popular.rs`
  (activity fetch), `benchmarks.rs` (backfill), `rank.rs`
  (orientations); `crates/catalog-ui/src/main.rs` (TOP tab).
