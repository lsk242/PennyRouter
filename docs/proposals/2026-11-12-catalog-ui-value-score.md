# Proposal: catalog value score + catalog UI overview

> Date: 2026-11-12 · Author: freeclaw · Status: Implemented (Iter 34)
> Depends on: schema v7, Iter 31 AA indexes, Iter 32 offline tiers, Iter 33
> `minindex` (PR #40). Shipped as schema v8.
> Companion: `docs/ranking-index.md` (the ordering contract this extends),
> `docs/catalog-sync-architecture.md` (pipeline), `docs/proposals/2026-11-03-artificial-analysis-index.md`
> (the AA signal this consumes).

## TL;DR

Two halves, one iteration:

1. **Sync** computes a deterministic **rank score** per offering — "maximum
   intelligence for the lowest price" — from the AA index (precision-adjusted),
   the catalog price, and popularity, with speed then availability as
   tie-breakers. No AI, no external ranking service. Stored in new v8 columns so
   the UI never recomputes it.
2. **Catalog UI** becomes two real pages: an **overview** (data-quality counts +
   the exact score formula and constants) and **models** (tabs for `free`,
   `tiers`, `decision`, sorted by rank score, every input of the score shown per
   row).

Live snapshot used for the numbers below (`GET /v1/models`, 2026-11-12):
**473 models**, 209 with any AA index (intelligence 92, coding 170, agentic 138),
264 with none, 17 zero-prompt, median balanced cost **$1.00/M tokens**.

## 1. Why

Today the catalog has the ingredients but no verdict:

- `quality_tier` / `quality_score` come from the offline scorer: `coding_index /
  100` bucketed at 70/40. Coding is the right primary signal for this product,
  but three buckets throw away the index's resolution, and the score never meets
  price, precision, or popularity.
- Price lives per endpoint and is never combined with quality. The ranker
  (`rank.rs`) orders by tier, then error rate, then price — a lexicographic
  sort, not a value judgement. A $90/M model and a $0.09/M model with the same
  tier are "equal" until the price tie-break.
- Precision is a *class* used to avoid cross-class substitution, but never a
  quality discount: an fp4 endpoint of a model benchmarked at fp16 is scored as
  if it were the full model.
- Speed and availability are in the payload (`throughput_last_30m`,
  `latency_last_30m`, `uptime_last_30m`) and never extracted.
- Popularity is fetched but truncated to the top 10 slugs for the gateway menu —
  unusable as a per-model factor.
- The UI shows a model list with endpoint counts and a flat endpoint table. It
  cannot answer "how many models are missing an AA index?" or "why is this model
  ranked above that one?".

## 2. Part A — sync: the rank score

### 2.1 New extracted signals

Per endpoint (`process.rs`, new `ENDPOINT_INDEXED_COLUMNS`):

| Column | Source | Notes |
|---|---|---|
| `latency_ms` | `latency_last_30m` | number or null; negative/non-finite → null |
| `throughput_tps` | `throughput_last_30m` | tokens/s; same validation |
| `uptime_pct` | `uptime_last_30m` | 0..=100; out of range → null |

Per model (`MODEL_INDEXED_COLUMNS`):

| Column | Source | Notes |
|---|---|---|
| `aa_cost_per_task` | `benchmarks.artificial_analysis.cost_per_task` | **absent today** (verified live); reserved so AA's own number wins when it appears |
| `popularity_tokens` | `rankings-daily` window, summed per permaslug | matched to the model via `canonical_slug`, falling back to `id` |
| `popularity_share` | `popularity_tokens / max(popularity_tokens)` | 0..=1, null when the model is absent from the window |

Popularity currently truncates to `POPULAR_MODELS_TOP_N = 10` for the menu. The
score needs the whole window: `fetch_popularity` keeps the full aggregate for
scoring and still stores the top-10 list for the menu (two views, one fetch).

**Ordering change in `catalog-sync`:** the popularity fetch moves *before*
`run_processing` (today it runs after), so the score sees it. A failed fetch
still publishes — the score then uses `popularity_share = null` (factor 1.0,
no penalty) and the overview flags "popularity missing".

### 2.2 The formula

Per offering (model × provider × precision class), all terms in the DB:

```
quality      = aa_index × precision_factor            # 0..100
cost_per_task = effective_cost × TASK_TOKENS          # $, balanced profile
efficiency   = COST_HALF_CREDIT / (cost_per_task + COST_HALF_CREDIT)  # 0..1
popularity   = (1 + POP_WEIGHT × share) / (1 + POP_WEIGHT)   # 0.909..1.0
rank_score   = quality × efficiency × popularity      # 0..100
```

Constants (code, not env knobs — a score must mean the same thing on every
rebuild):

| Constant | Value | Rationale |
|---|---|---|
| `TASK_TOKENS` | 10 000 | the balanced profile's mix at a typical task size (5k in / 4k out / 0.5k cache read / 0.5k cache write) |
| `COST_HALF_CREDIT` | $0.01 | the cost/task at which price efficiency is exactly 0.5; the live median model costs $1.00/M ≈ $0.01/task, so the curve is centred on the real catalog |
| `POP_WEIGHT` | 0.10 | popularity can move a score by at most 10%; it never rescues a bad model |
| `RULESET_VERSION` | `rank-v1` | carried in `rank_ruleset` so a formula change is visible in the artifact |

#### What `COST_HALF_CREDIT` is — and is not

It is **not a model**, and **not a fallback for a missing price**. It is one
number in the price-efficiency curve: the cost per task at which a model earns
half credit on price. The curve is `ref / (cost + ref)`, so:

| cost/task | efficiency | meaning |
|---|---|---|
| $0 (free) | 1.000 | full credit |
| $0.001 | 0.909 | 10× cheaper than the anchor |
| $0.01 | 0.500 | the anchor — half credit |
| $0.05 | 0.167 | |
| $0.10 | 0.091 | |
| $1.00 | 0.010 | 100× the anchor |

A **missing price is a different case**: `rank_score = NULL` (§2.5). The model
is listed last with a "no price" badge, never scored as if it cost the anchor.

Why a saturating curve instead of `quality / cost`: raw ratios are unbounded
(a $0.0001/task model scores 10 000× a $1/task one) and unstable across
rebuilds. `ref/(cost+ref)` is monotone, bounded, and has one interpretable
anchor: **half credit at the median price**.

Why popularity is normalized by `1 + POP_WEIGHT`: the score stays on a 0..100
scale (100 = top quality, free, most popular) instead of drifting to 110.

### 2.3 Precision factor

AA benchmarks the reference model; a quantized endpoint is not that model.

| Class | Factor | Note |
|---|---|---|
| `fp32`, `fp16` (bf16) | 1.00 | reference precision |
| `fp8` | 0.97 | small, well-documented degradation |
| `int8` | 0.95 | |
| `int4` (nf4/fp4) | 0.88 | the case the proposal exists for |
| `unknown` | 0.95 | unverified precision is a mild, *uniform* discount — it never reorders two unknowns |

Constants, versioned with the ruleset. A later iteration can calibrate them
from paired AA runs of the same model at two precisions; until then they are
declared, not fitted.

### 2.4 Which AA index

**Coding first** (decided 2026-11-12): PennyRouter's workload is coding, the
offline tier scorer and the Iter 33 `minindex` filter already speak
`coding_index`, and it has the best coverage. Fallback chain, recorded in
`rank_basis`:

```
coding_index  →  intelligence_index  →  agentic_index  →  none
```

Live distribution of that chain over 473 models: **coding 170, intelligence 39,
agentic 0, none 264**. Agentic never becomes the basis today — every
agentic-scored model also carries coding or intelligence — but it stays in the
chain as a defensive fallback for a future model that only has it.

`none` means **no score** (`rank_score = NULL`), never a zero: an unmeasured
model must not sort below a measured bad one. The UI shows the basis per row
(`coding` / `intel` / `agentic` / `—`).

The score reads the AA columns directly, **not** `quality_score`: an online
scorer verdict must never move a deterministic score.

### 2.5 Free, decision, and missing data

- **Free** (`effective_cost == 0`): same formula, `efficiency = 1`. Free models
  therefore top a combined list, which is exactly why the UI separates them into
  their own tab.
- **Decision models** (`supports_decisions = 1`): different basis — the text
  chain does not apply to behaviour models, so quality comes from
  `agentic_index` (fallback `intelligence_index`), precision factor is 1.0
  (quantization is not the variable there), and the price term is unchanged.
  `rank_basis` records `agentic`. They get their own UI tab and are excluded
  from `free`/`tiers`.
- **No price** (unpriceable offering): `rank_score = NULL`. Value cannot be
  computed without a cost; the row sorts last with a "no price" badge.
- **No speed/uptime**: tie-breakers treat them as unknown (sort after known),
  never as zero.

### 2.6 Tie-breakers

Equal scores (to 3 decimals) resolve in this order — the user's stated
preference, and the only place speed/availability enter:

```
rank_score desc → throughput_tps desc → latency_ms asc → uptime_pct desc
                → provider asc → tag asc
```

Provider/tag last keeps rebuilds byte-stable, as `rank.rs` already does.

### 2.7 Where it runs, and schema v8

New module `crates/catalog/src/rank_score.rs` (pure functions + unit tests),
called from `run_processing` after the AA indexes and the offline tiers. One
transaction, idempotent, no network.

Schema v7 → v8, additive only (no renames, so migration is the existing
`has_column` + `ALTER TABLE` loop):

```rust
// endpoints
("latency_ms", "REAL"), ("throughput_tps", "REAL"), ("uptime_pct", "REAL"),
("rank_score", "REAL"), ("rank_quality", "REAL"), ("rank_efficiency", "REAL"),
("rank_popularity", "REAL"), ("rank_basis", "TEXT"), ("rank_ruleset", "TEXT"),
("ranked_at", "TEXT"),
// models
("aa_cost_per_task", "REAL"), ("popularity_tokens", "INTEGER"),
("popularity_share", "REAL"),
```

Storing the components (`rank_quality`, `rank_efficiency`, `rank_popularity`)
is what lets the UI show the arithmetic without re-deriving it — and lets a
future gateway adopt the score with one column read.

### 2.8 Ranking document

`ClassRanking` gains an optional `score: Vec<RankEntry>` ordering
(`#[serde(default)]`, so v7 documents still parse). The gateway **ignores it in
Iter 34** — adopting it for routing is a separate iteration with its own
rollout. Publishing it now means the ordering is already in the artifact when
that iteration lands.

## 3. Part B — catalog UI

### 3.1 `/` — overview

Replaces the current runs-first page. Sections:

1. **Artifact** — version, created_at, schema, source, DB size, age (warns past
   24h).
2. **Coverage** — the counts the user asked for, each with a percentage:
   - models: total, with any AA index, by basis (intel/coding/agentic), none
   - offerings: total, priced, unpriced, free, scored, unscored
   - precision classes: fp32/fp16/fp8/int8/int4/unknown
   - tiers: high/standard/low/untiered
   - decision models
3. **Data quality** — explicit warnings, not just numbers: "264 models have no
   AA index (56%)", "12 offerings have no price", "1 model has a negative
   price", "popularity missing (no RANKINGS_API_KEY)".
4. **How the score is calculated** — the formula, the constants table, the
   precision factors, the basis chain, the tie-break order, the ruleset
   version, and one worked example computed from the loaded artifact (so the
   explainer can never drift from the code).
5. **Runs** — the existing manifest table, condensed.

### 3.2 `/models` — tabs

Three tabs, one table each, all sorted by the §2.6 chain:

| Tab | Rows | Sort |
|---|---|---|
| `free` | offerings with `effective_cost == 0` | rank score desc |
| `tiers` | every other text offering | rank score desc |
| `decision` | `supports_decisions = 1` | rank score desc |

Columns: model, provider, precision, AA index (basis), precision factor,
quality, prompt/completion $/M, cost/task, efficiency, popularity, **score**,
throughput, latency, uptime.

Each row expands (`<details>`) to the full arithmetic:

```
quality      = 60 (coding) × 0.97 (fp8)         = 58.20
cost/task    = $1.2825/M × 10 000 tokens        = $0.012825
efficiency   = 0.01 / (0.012825 + 0.01)         = 0.438   (COST_HALF_CREDIT)
popularity   = (1 + 0.10 × 0.42) / 1.10         = 0.947
rank_score   = 58.20 × 0.438 × 0.947            = 24.1
tie-breaks   = 82 tok/s · 640 ms · 100% uptime
```

Filters: `?q=` substring on model id, `?min=` minimum score. No JS — the page
stays a server-rendered read-only view like today.

### 3.3 Routes

`/` overview · `/models?tab=free|tiers|decision` · `/endpoints?model=` (kept,
per-model detail) · `/ranking` · `/meta` · `/log`. Nav becomes
`overview | models | ranking | meta | log`.

## 4. What this deliberately does not do

- **No AI or external ranking service.** The score is arithmetic over stored
  facts. The online scorers stay for routing tiers and are not inputs.
- **No gateway change.** The score is published, not consumed; `minindex` and
  the existing tier ordering keep working unchanged.
- **No model-level rollup.** Rows are offerings, because price (and therefore
  value) is per offering. A "best offering per model" view is a later toggle.
- **No `design_arena` ELO, no per-task-type scores.** One general score; task
  variants (`coding`, `agentic`) are already visible as the basis.
- **No fitted weights.** Every constant is declared and versioned; calibration
  is a separate iteration with data.
- **No new env knobs.** Constants live in code.

## 5. Tests + acceptance

Unit (`rank_score.rs`):

- precision factors per class; unknown is uniform.
- efficiency: free → 1.0, `COST_HALF_CREDIT` → 0.5, 100× → ~0.0099.
- popularity: absent → 1.0 factor; top model → 1.0; mid → between.
- basis chain: coding wins, intelligence next, agentic last, none → `NULL`.
- missing price → `NULL`; missing index → `NULL`; never zero.
- tie-break chain: equal scores order by throughput, latency, uptime, then
  provider/tag; unknowns sort last.
- determinism: same inputs → identical rows.

Processing / migration:

- fixture models (fp16 vs fp4, free vs paid, indexed vs not) → expected
  `rank_*` columns; re-run is idempotent.
- v7 → v8 migration adds all 13 columns; idempotent; v7 rows keep `NULL` scores
  until reprocessed.
- popularity ordering: score sees the window when the fetch precedes
  processing; a missing window yields `NULL` share and a valid score.

Catalog UI (`crates/catalog-ui/tests/ui.rs`):

- overview renders every count and the formula/constants text; a catalog with
  no AA index shows the warning and no NaN.
- `/models?tab=free` lists only free offerings; `tiers` excludes them;
  `decision` lists only decision models; each sorted by score desc.
- a row's breakdown contains the exact component values from the DB.
- empty artifact degrades to "no verifying catalog" on every route.

Live smoke (optional, same bar as Iter 31):

- `catalog-sync --fetch-openrouter --process` → `SELECT COUNT(*) FROM endpoints
  WHERE rank_score IS NOT NULL` covers every priced offering of an indexed
  model (expect ~200 models × their endpoints); overview percentages match the
  live snapshot in §TL;DR.

## 6. Rollout

Single iteration, but it bumps the schema: a v8 gateway rejects v7 artifacts
(exact-match check in `routing.rs`), so the first v8 publish must land before or
with the gateway image — same note as v6 and v7. Reloads keep last-good across
the window; a boot on v7 stays in standby passthrough until the first v8
publish.

Order inside the iteration: schema + extraction → score module + processing →
ranking document field → UI overview → UI models tabs → docs
(`ranking-index.md`, `catalog-sync-architecture.md`, `operations.md`).

## 7. Decisions (2026-11-12)

1. **Basis: coding first.** `coding_index → intelligence_index → agentic_index
   → none`. Coding is the product's workload, matches the offline tier scorer
   and `minindex`, and covers 170/473 models versus 92 for intelligence.
2. **`COST_HALF_CREDIT` = $0.01/task.** Renamed from `COST_REF` because the
   name was opaque: it is the cost/task at which price efficiency is exactly
   0.5, not a model and not a missing-price fallback (§2.2). A missing price
   yields `rank_score = NULL`.
3. **Free tab ordering: as proposed.** Free models all have `efficiency = 1`,
   so the tab ranks on quality × popularity; a free model with a low index can
   outrank a paid one, which is the intended "free first" story.

## 8. As built (2026-11-12)

Deviations from the proposal, all deliberate:

- **Popularity is fetched before processing.** The score needs the
  whole window, not the top-10 menu pins, so `store_popularity` also
  writes `popular_tokens` (slug → tokens) and the sync pipeline moved
  the fetch above `run_processing`. A keyless sync stores nothing and
  the factor is a zero share.
- **`aa_cost_per_task` is stored but unused.** AA does not ship a
  cost-per-task field yet; the column and extractor exist so AA's
  number wins the moment it appears. Until then the score uses the
  catalog's own effective price.
- **Promo windows are not applied at processing.** The score uses the
  static discount column; time-boxed promos are evaluated at ranking
  rebuild time, as before. A promo therefore does not move
  `rank_score` until the next processing run.
- **The ranking document's `score` ordering is omitted when the
  catalog was never processed** (`skip_serializing_if`), so a v7-era
  document still parses and an unprocessed DB does not publish a
  meaningless identity list. Operator assist still applies to it.
- **The models page defaults to the `tiers` tab** (most offerings
  live there); `free` and `decision` are one click away. The overview
  also lists precision-class and tier distributions, which the
  proposal did not ask for but cost nothing to compute.
- **Speed metrics come from `latency_last_30m`, `throughput_last_30m`,
  `uptime_last_30m`.** Today's payloads carry only uptime, so latency
  and throughput are usually `NULL` and the tie-breakers fall through
  to provider/tag.

## References

- `crates/catalog/src/process.rs` — `run_processing`, `extract_aa_index`,
  `classify_precision`, `extract_price`.
- `crates/catalog/src/rank.rs` — `compare`, `Signals`, `ClassRanking`,
  `rank_catalog`.
- `crates/catalog/src/price.rs` — `effective_cost`, `balanced_profile`,
  `fmt_price_per_m`.
- `crates/catalog/src/popular.rs` — `fetch_popularity`, `aggregate_models`,
  `store_popularity`.
- `crates/catalog/src/schema.rs` — `SCHEMA_VERSION`, `ENDPOINT_INDEXED_COLUMNS`,
  `MODEL_INDEXED_COLUMNS`.
- `crates/catalog/src/main.rs` — sync pipeline order (popularity before
  processing).
- `crates/catalog-ui/src/main.rs` — `shell`, `nav`, `index`, `models`,
  `endpoints`.
- `docs/ranking-index.md`, `docs/catalog-sync-architecture.md`,
  `docs/pricing.md`, `docs/operations.md`.
- `docs/proposals/2026-11-03-artificial-analysis-index.md` — the AA signal.
