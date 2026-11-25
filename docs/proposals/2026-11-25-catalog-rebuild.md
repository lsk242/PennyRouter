# Proposal: Catalog rebuild — columns, not code

**Date:** 2026-11-25
**Status:** Proposed (awaiting review)
**Authors:** leandro
**Supersedes:** `2026-11-18-popularity-first-ranking.md` ((rank-v1/v2, brackets-as-code, and the score-first gateway it specified are withdrawn by this rebuild)

## Summary

Scratch the catalog and start over: fresh code, fresh schema, fresh
artifacts, no v1/v2 legacy. `catalog-sync` makes three requests,
computes one set of **use columns**, and stores them. The gateway
never recomputes anything — every virtual id is a pure
filter + order over stored columns.

This proposal exists because the current stack misroutes in ways
that are hard to see and harder to fix incrementally. The
kimi-k3.1-pro incident (Nov 25) is the exhibit: `low` served a
$1.96/M model while $0.05/M popular models sat excluded, through
an interaction of three layers nobody can hold in their head at
once (v2 quality floor 40 on the intel basis × unknown-precision
exclusion × score-path pool rules). The fix is not a fourth layer.
It is fewer layers.

## Goals

1. One catalog with one row shape per offering and per model, where
   every number the router reads is computed at sync time.
2. Every virtual id defined as exactly: pool filter + ordering (+
   shared gates). No inline blends, no recompute, no fallback
   chains with different semantics per leg.
3. Usage (tokens + trend) as the ground-truth quality signal that
   every index-based decision is contrasted against.
4. Tiers the operator can predict: cheapest, cheap-and-popular,
   rising value, coding sessions, hard tasks.

## Non-goals

- Changing the gateway's transport: forwarding, SSE/streaming,
  retries, guards, timeouts, token accounting stay as they are.
- Changing sessions/stickiness, throttle skip, budget/tool/context
  gates, or privacy injection: these are mechanical, tier-agnostic,
  and stay.
- Backfilling history: old artifacts are dropped, not migrated.

## Background: what broke

- **Score complexity.** `rank-v2` mixes index basis chains, version
  gates, AA-vs-catalog costs, activity factors, and bracket rules.
  Its output (a single score) hides which input decided, so a
  surprising pick takes an hour to explain.
- **Floor calibration.** The low floor (quality ≥ 40) was set
  against fixture-scale indexes (60–90). Real intel data runs
  ~26–46: the floor silently redefined `low` as flagship-only.
- **Unknown precision excludes flagships.** First-party providers
  don't publish quantization, so gemini/opus-class models carry
  `unknown` class and fail the known-class gate everywhere.
- **Coding signal gone.** Embedded coding/agentic indexes are nulled
  upstream; the code-benchmarks slice 400s; no AA key was set. Every
  quality number currently rides the intel basis alone.
- **Decisions are invisible.** See §8: zero of 472 list models carry
  the modality; only the hand-nominated respan row matches.

## Data sources (the three requests)

All fetches are sync-time batch, with per-group warn-and-continue:
one failing source never fails the sync, and missing data reads as
missing (never zero, never guessed).

1. **Full catalog** — `GET /v1/models` (472 models) +
   `GET /v1/models/{id}/endpoints` per model (+ `EXTRA_MODEL_IDS`
   as today). Yields: offerings, prices, tags, quantization,
   context limits, modalities, embedded AA payloads.
2. **Usage leaderboard** — OpenRouter Data API `rankings-daily`
   (recent window + priors) for per-model tokens and trend.
   This is the ground truth (§6). `app-rankings` stays for the
   hot/coding app display only.
3. **AA Intelligence Index** — AA Data API free tier (key now
   configured): per-model intel index + index version +
   cost/task. Versions gate comparisons: two indexes only
   compare on the same version.

> **Open question 1:** §2 assumes "LLM Leaderboard" means the
> OpenRouter usage rankings. If it means something else (Arena,
> a benchmarks rollup), say so — the usage column needs a home.

## The catalog: use columns

Fresh schema (`v1`, no migration chain). Two tables (plus `sync_meta`
for citations/health, as today).

Per **offering** (computed from sources 1+3):

| Column | Meaning | Missing |
|---|---|---|
| `price_task` | Blended $/task (balanced profile) | NULL = unpriceable, excluded everywhere paid |
| `precision_class` | fp32/fp16/fp8/int8/int4/fp4/`unknown` | `unknown` (see §7 Q6) |
| `time_task` | `TASK_TOKENS / throughput_tps`, seconds | NULL = neutral in code ordering |
| `uptime_pct`, `throughput_tps`, `latency_ms` | Speed/availability as fetched | NULL = sorts last, never zeroes |
| `is_free`, `is_batch`, `tools`, `decisions` | Capability flags | false/`None` as today |

Per **model** (computed from sources 2+3):

| Column | Meaning | Missing |
|---|---|---|
| `intel`, `intel_version` | AA index + test version | NULL |
| `usage_level` (0..1 share of top), `usage_trend` (ratio) | From the rankings window | NULL |
| `value` | `best price_task / intel` ($/task/intel-point) | NULL if either side missing |
| `top_rank` | Usage rank, 1-based | NULL when unmeasured |

Derived rule: **cost per task is always per intelligence point.**
Raw $/task never orders anything except `lowest`; every other
comparison goes through `value` (or an explicit variant), so a
cheap weak model cannot outrank on price alone — and the Ember
case (very low price/task, not in the top) resolves correctly:
Ember loses on `value` and on usage, not on special-casing.

Missing-data doctrine (the AA-reliability answer): an index
without a matching version is ignored for ordering (kept for
display); a model without usage can never enter a TOP pool but
can still serve `lowest`/direct; a model without an index can
never win an intel-ordered comparison but keeps its usage rank.
No silent zeros anywhere.

## Virtual ids: filter + order

Shared gates apply on every tier exactly as today: provider
allow/ignore, tools, context fit, budget ceiling, throttle skip,
stickiness under the full id namespace. Speed/availability order
ties everywhere (uptime desc, then throughput desc, then
provider/tag) — no silent substitution, no zeroes.

| Id | Pool filter | Order | Purpose |
|---|---|---|---|
| `lowest` | priced, not batch | `price_task` ASC | Cheapest possible; trivial tasks |
| `low` | TOP (§6) AND priced, not free, intel ≥ 25 | `price_task` ASC | Popular cheap models for agents |
| `trending` | Rising (`trend` > 1, `level` ≥ 0.005) | `value` ASC | Rising value, early |
| `code` | TOP AND priced AND intel present | `code_value` ASC (below) | Long coding sessions |
| `think` | TOP-10 most-used | `price_task` ASC with trend preference (below) | Hard agent tasks |
| `free`, `decision`, `batch` | Unchanged scopes (zero-cost; decisions-capable; `:batch` only) | Cheapest within scope | As today |
| `model/<id>` | That model's offerings | Cheapest eligible | Explicit pin |

Formulas (all inputs stored; the gateway compares, never computes):

- `value = price_task / intel`, per offering (model intel ×
  offering price). NULL when either side is missing.
- `code_value = value / speed_factor`, where `speed_factor =
  throughput_tps / median_tps` over the pool (missing speed →
  1.0, neutral). Lower wins: max intelligence per dollar per
  unit of speed.
- `think`: order by `price_task` ASC, but divide by `(1 + boost)`
  with the bounded trend boost first (rising models get
  effectively cheaper); ties by intel DESC. The TOP-10 gate is
  the quality proof — no separate floor.
- `low` floor: intel ≥ 25 default; an explicit `/minindex/N`
  overrides it (see Q4).

Worked example (live Nov-25 numbers): `low` pool = TOP minus
free; deepseek-v4.1-flash (intel 31 ≥ 25, $0.000581/task) beats
glm-5.3-flash ($0.000891) beats kimi ($0.0196). Kimi can only win
`low` if every cheaper TOP model drops below the floor — the
incident becomes the acceptance test.

> **Open question 2:** is `trending` a real tier (menu +
> privacy variant + inspector), or a signal inside other
> orderings? Proposed above as a tier.
> **Open question 3:** the `think` formula above is my best
> reading of "price with highest used … best price per token
> trend". Alternatives: max intel under a spend cap (current
> behavior), or pure usage order. Which promise should `think`
> make?
> **Open question 4:** does `/minindex/N` override the 25
> default on `low`, or take `max(25, N)`?

## TOP ranking

TOP = top 25 models by `usage_level`, each with absolute recent
share ≥ 0.5%, computed at sync time and stored (`top_rank`).
`think` reads the top 10 of the same list. Rationale: usage is
the only signal that cannot be gamed by pricing and exists for
every model anyone cares about; 25 + 0.5% keeps the long tail
out while surviving a flat week.

## Filters: privacy and minindex

- `privacy/`: unchanged — `zdr: true` + `data_collection:
  "deny"` enforcement on every tier including the new ones.
- `/minindex/N`: rebased onto the **intel** index (the coding
  basis is dead: nulled upstream, no AA fallback configured).
  `low` starts at 25; other tiers take N as given (0–100).
- The old coding-index floor and all coding-based gates go away
  with the rebuild.

> **Open question 5:** keep the `/minindex/` path name (compat,
> now means intel) or rename to `/minintel/` (clarity, breaks
> existing ids)? Proposed: keep, document loudly.
> **Open question 6:** first-party `unknown` precision — keep
> excluding (conservative, drops flagships), or treat unreported
> first-party as full precision? This single rule decides
> whether gemini/opus-class models can serve tiered traffic.

## Gateway: what survives, what dies

Survives (mechanical, tier-agnostic): forwarding + SSE,
retries, guards, sessions/stickiness, throttle skip,
budget/tool/context gates, privacy injection, smart classifier
legs, batch + decisions scopes, `/v1/decisions` route, metrics
and logs, both debug UIs (rewritten for the new columns),
`ROUTING_DEFAULT` (becomes: new ordering vs legacy? see Q8).

Dies with the rebuild: inline cost blending, tier blending and
floors, offline/online scorers, `rank_score` v1/v2 + popularity
and activity factors, bracket-as-code + ends computation,
long-tail partition, version-gate-at-route-time, rank_ruleset
branching, `POP_WEIGHT`/`TREND_BOOST_MAX` tuning constants. The
router keeps snapshot loading, pinning, and pick-by-comparison.

> **Open question 7:** confirm the drop list — anything above
> you want kept (e.g. the TOP tab arithmetic display, the
> score gauges)?

## Decisions finding (investigated Nov 25)

Live `/v1/models` (472 models): **zero** carry `decisions` in
`architecture.output_modalities` (only audio/image/text/video
exist). The one decision row in the catalog (respan free) matches
only because individually-fetched model bodies (`/v1/models/{id}/
endpoints`, via `EXTRA_MODEL_IDS`) still carry the modality —
the list shape does not. So decision-capable paid models are
invisible unless hand-nominated, and there is no automatic way
to discover new ones.

> **Open question 8:** (a) keep the extractor + curate
> `EXTRA_MODEL_IDS` by hand; (b) probe every model's detail
> endpoint per sync (~472 extra calls — rate-limit risk);
> (c) drop the `decision` tier + `/v1/decisions` route until
> upstream restores the signal. Proposed: (a).

## Rollout (scratch plan)

1. New branch `rebuild/catalog-v1`. Delete-first milestones:
   catalog fetch/process/store → gateway routing → UIs →
   fixtures/tests. Nothing migrates; old artifacts are dropped
   (wipe the catalog volume on cutover).
2. Single cutover deploy: catalog-sync publishes fresh v1, then
   the gateway (requires v1, last-good covers the gap as usual).
3. Validation gate: the kimi incident inverted — `low` must serve
   a sub-$0.10/M TOP model while kimi keeps `think`-style
   quality legs; full suite green; smoke + TOP review on srv.lan.

## Test plan

- Catalog: fixture syncs per source (present/missing/malformed);
  column-exact assertions incl. NULL rules; join ladders.
- Gateway: one HTTP suite per tier (filter + order + gates +
  sticky + fail-closed); preview parity; minindex/privacy
  composition; invalid-env startup refusal.
- Live: TOP review + smoke on srv.lan before promoting the
  default.

## Open questions (review checklist)

1. Is "LLM Leaderboard" the OpenRouter usage rankings?
2. `trending`: real tier or a signal?
3. `think` formula: trend-discounted price, max intel under cap,
   or pure usage order?
4. `/minindex/N` on `low`: override the 25 default or `max(25,N)`?
5. Keep `/minindex/` path or rename to `/minintel/`?
6. First-party `unknown` precision: exclude or treat as full?
7. Confirm the drop list.
8. Decisions: curate, probe-all, or drop the tier?
