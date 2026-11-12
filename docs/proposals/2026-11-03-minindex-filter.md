# Proposal: `minindex` suffix filter — minimum AA coding index on the model path

> Date: 2026-11-03 · Author: freeclaw · Status: Implemented (Iter 33).
> As-built deviations from the draft: the floor rides `Decision` and is
> echoed as `x-pennyrouter-minindex`; metrics are
> `pennyrouter_minindex_requests_total{minindex,tier,result}` (no
> `pennyrouter_minindex_floor` gauge); the `/ui` coding-index column
> landed in Iter 31 as a model-header annotation, not a table column;
> the draft's `low/minindex/80/fallback/low` example is not valid
> grammar (fallback stays restricted to `free`/`decision/free` and
> `model/<id>`), so the fallback-leg test uses
> `free/minindex/90/fallback/low`.
> Depends on: `upstream/main` @ `a79e5a8` — schema v5, Iter 28 pinned-model routes; and `docs/proposals/2026-11-03-artificial-analysis-index.md` (AA index ingestion — this doc is the gateway consumer).
> Companion: `docs/proposals/2026-11-01-openrouter-interface.md` (native paths), `docs/proposals/2026-11-02-smart-classifier.md` (smart classifier).

## TL;DR

Add `/minindex/[number]` as an optional suffix to the model path — the minimum Artificial Analysis `coding_index` a candidate must meet to be shown/served. Examples:

- `openrouter/pair/low/minindex/50` — cheapest basic offering whose model has `aa_coding_index >= 50`.
- `openrouter/pair/model/anthropic/claude-4.5-sonnet/minindex/70` — that specific model, only if it meets the floor (else fail-closed).
- `openrouter/pair/privacy/think/minindex/40/fallback/low` — the floor scopes the primary leg; fallback leg is its own tier pick (no floor unless also suffixed in a future extension).

Without a suffix, behavior is unchanged. With an invalid suffix, fail-closed (`invalid_model` 400) — never silent passthrough.

## 1. Why suffix, not query param or header

- The gateway already carries routing intent in the `model` field as suffix segments (`/privacy`, `/low`, `/think`, `/lowest`, `/fallback/<tier>`, `/model/<id>`). A filter that lives there composes with `provider.max_price`, `only`/`ignore`, tool-awareness, and budgets without a new header or query contract.
- The harness change is one string edit (`model: "openrouter/pair/low/minindex/50"`), not a new API surface. Opencode custom providers already template `model` per request.
- Header/query filters would need new proxy/Fallback handling and would diverge from the existing `parse_virtual` grammar. Suffix keeps the gateway's "model string is the routing intent" story intact.

## 2. What counts as "minimum index"

- **Source:** `benchmarks.artificial_analysis.coding_index` from OpenRouter's `GET /v1/models`, indexed as `models.aa_coding_index INTEGER` via the companion catalog proposal (v5→v6). Observed range `0..100` (int or null).
- **Semantics:** `minindex/N` means `aa_coding_index >= N`. `N` is an integer `0..100` inclusive. `0` is valid (effectively "must have any coding index", since null < 0 and thus excluded — see §5).
- **Scope:** per-model filter, not per-endpoint. All offerings of a model share the same `aa_coding_index`. The filter runs before provider ranking/throttle/budget.
- **Other AA indices:** `intelligence_index` / `agentic_index` are ingested but not filtered in this iteration. A follow-up can add `minindex-intelligence/N` etc. without changing the catalog.

## 3. Grammar

```
virtual_model := "openrouter/pair/" [ "privacy/" ] <core> [ "/minindex/" <n> ] [ "/fallback/" <tier> ]
core          := "free" | "low" | "think" | "decision" | "decision/free"
               | "smart" | "smart/max"
               | "model/" <model-id>
n             := integer 0..100 (no sign, no decimal, no leading `+`/`-`; leading zeros allowed, e.g. "050" == 50)
tier          := "low" | "think" | "lowest" | "decision" | ... (existing fallback tier names, resolved through decide_tier)
```

Notes:

- `privacy/` stays first (existing). `minindex` sits after the core id and before `/fallback/` when both present.
- `model/<id>` itself may contain slashes (e.g. `deepseek/deepseek-v4.1-flash`). The parser splits from the right: look for `/fallback/` first, then `/minindex/<n>` before it, then the `model/` prefix — same technique Iter 28 uses for `model/<id>/fallback/<tier>`.
- `smart`/`smart/max` accept `minindex` too (classifier picks tier, then filter applies to that tier's candidates — see §4).
- Unknown suffixes after `minindex` that are not `/fallback/` are invalid (no silent ignore — the gateway owns this id space, unlike raw model ids where unknown tags are ignored).
- Raw tagged-model suffixes (`<model-id>/<tag>`, Iter 5 tag grammar; the `lowest` tag itself is Iter 19) do not use `minindex`; their future is a `minindex`-like filter on the raw model path if needed — explicitly out of scope here to avoid tag-grammar collision.

Examples valid:

- `openrouter/pair/low/minindex/50`
- `openrouter/pair/privacy/low/minindex/50`
- `openrouter/pair/model/deepseek/deepseek-v4.1-flash/minindex/60`
- `openrouter/pair/model/deepseek/deepseek-v4.1-flash/minindex/60/fallback/low`
- `openrouter/pair/smart/minindex/40`
- `openrouter/pair/free/fallback/low` (existing, no floor — floor on fallback leg alone stays future work)

Examples invalid → `400 invalid_model` (OpenAI-shaped `{"error":{"message","type":"invalid_request_error","code":"invalid_model"}}`):

- `openrouter/pair/low/minindex/` (empty N)
- `openrouter/pair/low/minindex/50.5`, `minindex/-1`, `minindex/abc`, `minindex/101`
- `openrouter/pair/low/minindex/50/minindex/60` (duplicate)

## 4. Routing

### 4.1 Where it runs

- `crates/gateway/src/routing.rs::parse_virtual` gains `minindex: Option<u8>` on `VirtualModel` (alongside `tier`, `privacy`, `free_only`, `fallback`, `pinned_model`).
- Tier dispatch (`decide_tier`, `decide_virtual`, `decide_ordered_virtual` (the shared `low`/`think` pick), `decide_lowest`, `decide_decision`, pinned-model path) filters candidates by `aa_coding_index >= minindex` when present. The filter is the first eligibility gate, before tool support, budget, throttle, and promo/price ranking — so "too low quality" is not rescued by being cheaper.
- The filter consults the snapshot's `aa_coding_index: HashMap<String, i64>` map (populated from `models.aa_coding_index` at load + reload). No DB hit on the request path.

### 4.2 Interaction with existing features

- **Pinned models:** one model — if its `aa_coding_index` is below the floor (or null), the pinned leg has zero candidates → `503 no_eligible_offering` (or fallback leg if `/fallback/` present).
- **Cross-model tiers (`low`, `think`, `lowest`, `decision`):** filter shrinks the candidate pool; ranking formulas unchanged, just over fewer models.
- **Fallback:** `.../minindex/N/fallback/<tier>` — the floor applies to the primary leg only. Fallback tier dispatch runs without the floor. (Applying the floor to both legs would require a second `minindex` segment on the fallback side — future work; YAGNI now.)
- **Budgets / tool-awareness / throttle:** applied after the floor, so a minindex-eligible candidate can still be rejected by a tighter constraint, failing closed with the usual code (`budget_exceeded` 400, `tools_unsupported` 400). The floor never overrides a cap.
- **Smart classifier:** `smart/minindex/N` classifies into `free`/`lowest`/`low` (or plus `think` on `smart/max`), then the floor filters that tier's candidates. If the classified tier has no surviving candidate, fail-closed with the tier's rejection code (don't re-pick another tier — classifier's decision is not retried, per `docs/routing.md#Smart classifier`). Fallback tier on classifier failure (`CLASSIFIER_FALLBACK_TIER`) also respects the floor.
- **Quality floors / precision class:** unchanged, applied after the floor.

### 4.3 Realtime contract

One local lookup per request still holds (`docs/routing.md`). The floor is an in-memory HashMap check. No network on the routing path except the existing `smart` classifier leg.

## 5. Missing/null index

- `aa_coding_index IS NULL` (model has no `coding_index` or catalog hasn't reprocessed yet) → **exclude** when `minindex` is present. Rationale: an unknown score cannot be said to meet a floor; including unknowns would defeat the filter.
- The non-suffixed path (`openrouter/pair/low` without `minindex`) is unaffected — nulls remain eligible as today.

## 6. Failure modes

| Condition | Status | Code | Body |
|---|---|---|---|
| Valid `minindex` but no candidate meets it | 503 | `no_eligible_offering` | OpenAI-shaped, `Retry-After: 1` (a reload may restore) |
| Invalid `N` (non-int, out of range, duplicate) | 400 | `invalid_model` | `invalid_model: minindex must be 0..100, got ...` |
| Valid `minindex` on pinned unknown model id | 503 | `no_eligible_offering` | same as today |
| `minindex` present but catalog is old (no AA column) | 503 | `no_eligible_offering` | until next reload with v6 artifact — no passthrough |

## 7. Wire effects

Headers added on routed responses (never leak prompt):

- `x-pennyrouter-minindex: <N>` — echoed when present.
- `x-pennyrouter-provider: <chosen>` — as today.
- Existing smart headers (`x-pennyrouter-smart-tier`, `x-pennyrouter-classifier-*`) unchanged when `minindex` composes with `smart`.

`GET /v1/models` — curated menu stays 14 ids. Capability advertised by adding suffixed examples to the description fields, not by enumerating every `minindex` value (unbounded).

`GET /ui` (`GATEWAY_UI=1`) — adds a `Coding index` column to the model table where known; filtered requests highlight excluded models.

Metrics — new histogram/counter, bounded labels:

- `pennyrouter_minindex_requests_total{minindex="50",tier="low",result="ok|no_eligible|invalid"}` — `minindex` label is the literal `N` string (101 values, bounded).
- Optionally `pennyrouter_minindex_floor` gauge for the active DB coverage (`SELECT COUNT(*) WHERE aa_coding_index >= floor`).

## 8. Config surface

No new env vars. The floor comes from the model string per request. Operators can enforce a minimum via a reverse-proxy or middleware if needed — not a gateway knob.

## 9. Tests + acceptance

Unit (`routing.rs`):

- `parse_virtual` for every valid/invalid example in §3, including `model/<id>` with slashes + `minindex` + `fallback` composition, duplicate `minindex`, leading zeros.
- Filter: `minindex/50` over 3 fixture models with indices `30`, `60`, `null` → only `60` eligible; `minindex/0` → only non-null survive; no `minindex` → all survive.
- Pinned model + floor: below floor → `no_eligible_offering`; above → serves.
- Fallback: `low/minindex/80/fallback/low` — primary exhausted by floor, fallback serves `low` without floor.

Integration (mock catalog DB + mock upstream):

- `openrouter/pair/low/minindex/50` against a fixture DB with mixed indices → cheapest eligible serves; `x-pennyrouter-minindex: 50` present.
- `.../minindex/abc` → `400 invalid_model`.
- `.../minindex/50` where no model meets → `503 no_eligible_offering` with `Retry-After: 1`.
- `smart/minindex/40` — classifier picks `low`, floor applied; classifier auth failure → fallback tier also filtered.

## 10. What this deliberately does not add

- No query-param or header filter — suffix only (§1).
- No `minindex` on the raw tagged model path (`meta/muse-spark-open/minindex/50`) — that grammar is Iter 5 tag territory (the `lowest` tag is Iter 19) and would need a separate proposal to avoid tag collisions.
- No `maxindex` or range filter — one floor covers the stated need ("minimum value for index to show").
- No ELO / `design_arena` filter — different shape (per-category ELO), needs its own aggregation policy (future proposal if wanted).
- No `intelligence_index` / `agentic_index` suffix in this iteration — catalog ingests all three, but exposing more than `coding_index` widens the grammar without a stated need. Add `minindex-intelligence/N` later if asked.
- No ranking-index formula change — ranking stays per `docs/ranking-index.md` (price/tier/latency/trends); this is a pre-filter.

## 11. Rollout

Iter A — catalog AA ingestion lands first (companion doc), DB reaches v6, gateway snapshot carries `aa_coding_index` unused.
Iter B — this `minindex` change: `parse_virtual` + filter + headers/metrics + docs + tests. No DB migration of its own; an old DB without the column behaves as "no candidate meets the floor" (503) until the next artifact reload — no silent fallback to unfiltered.

Both iters preserve exactly-once semantics for non-smart paths; `smart`'s extra classifier leg is unchanged apart from the post-classification filter.

## References

- `crates/gateway/src/routing.rs` — `parse_virtual`, `VirtualModel`, `VIRTUAL_MODELS`, `Snapshot`, `Offering`, `is_smart_fallback`, `decide_*`.
- `crates/catalog/src/schema.rs` / `store.rs` / `process.rs` — v5→v6 AA columns, `extract_aa_index`, `aa_coding_index` map.
- `docs/api.md`, `docs/architecture.md`, `docs/routing.md`, `docs/ranking-index.md`.
- `docs/proposals/2026-11-03-artificial-analysis-index.md` — catalog prerequisite.
- `docs/proposals/2026-11-01-openrouter-interface.md`, `docs/proposals/2026-11-02-smart-classifier.md` — native paths, smart classifier.
- OpenRouter: `https://openrouter.ai/openapi.json` (server `https://openrouter.ai/api/v1`), `GET /v1/models` (verified 451/248 on 2026-11-03), `GET /benchmarks`.
