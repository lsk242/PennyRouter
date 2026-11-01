# Proposal: Live per-request classifier — `openrouter/pair/smart`

> Date: 2026-11-02 · Author: freeclaw · Status: Proposed (not yet implemented)
> Depends on: Iter 22 on `upstream/main` — schema v4, tool-aware routing, curated `GET /v1/models`.
> Companion: `docs/proposals/2026-11-01-openrouter-interface.md` (OpenRouter-native surface — no overlap).

## TL;DR

Add **one** new virtual model that calls a decision model on the request path to pick a tier: `openrouter/pair/smart`. Without it, the gateway keeps its current rule — pure local lookup, no model call on the hot path. Variants:

| Virtual id | Classifier output | Tier set |
|---|---|---|
| `openrouter/pair/smart` | `{free, lowest, low}` | Free ↔ paid-cheap ↔ curated-cheap |
| `openrouter/pair/smart/max` | `{free, lowest, low, think}` | …plus best-quality via judge signals |
| `openrouter/pair/privacy/smart` | same as `smart` | same tiers, plus `zdr`/`data_collection: deny` on both calls |
| `openrouter/pair/privacy/smart/max` | same as `smart/max` | same, with privacy fields |

Every other id — `openrouter/pair/free|low|think`, `openrouter/pair/free/fallback/<tier>` (Iter 21), `modal/…`, `baseten/…` (Iter 16), direct model ids + suffix tags — stays exactly as today. This is not a suffix filter. A bare `smart` tag on another model still does nothing today (unknown tag ignored) and keeps doing nothing.

## 1. Why only `smart`

The product problem: `free` is best when available but 50/day walls hit live; `lowest` is cheapest paid fallback but wastes frontier quality on hard prompts; `low`/`think` are good defaults but blind to prompt difficulty. No static tag solves "pick the cheapest tier that can actually answer this well."

The design principle: classifier use must be explicit and scoped. `smart` is the one id where the caller opts into an extra network call. The rest of the gateway keeps the current contract (`docs/routing.md` realtime contract: one local lookup, no network on routing path). This also limits blast radius — blast = exactly those callers, easy to measure.

## 2. Two-step request flow

For `model: "openrouter/pair/smart"` (and `smart/max`, `privacy/…`):

```
client → gateway                         ─┐
        → classifier upstream               │ classifier leg
        ← {tier:"low", confidence:0.86}    ─┘
        → tier routing (existing path)      ─┐
        → final upstream                    ─┘  completion leg
        ← streamed/buffered completion  → client
```

Step 1 — classify. A short non-streaming `POST /v1/chat/completions` to a dedicated classifier upstream/model, response `json_object` only. Step 2 — complete. The normal tier dispatch (`free`/`lowest`/`low`/`think`) picks the offering; existing guard/budget/max_price/floor/retry/429-cooldown/strict-fail-closed all still apply to step 2. The classifier never bypasses them.

Visible latency = classifier leg + completion leg. No batch, no fan-out, no judge-on-responses in this proposal (see §8 for explicit non-goals). Streaming of the final completion is preserved — only the classifier body is buffered (tiny JSON).

Non-smart ids never pay the extra hop.

## 3. Classifier interface

### Model + upstream

```yaml
# env — same pattern as Iter 16 pinned providers
CLASSIFIER_MODEL: openai/gpt-4o-mini          # or any OpenAI-compatible chat model
CLASSIFIER_BASE_URL: https://openrouter.ai/api # default: same as OPENROUTER_BASE_URL
CLASSIFIER_API_KEY: ${OPENROUTER_API_KEY}      # optional: override bearer for this leg only
  # if unset, reuse the incoming Authorization bearer on the classifier leg (BYOK preserved)
```

The classifier is just an OpenAI chat endpoint — swap vendors for free/cheap by changing the URL. Tests mock this.

### Prompt

A single system + user turn, `temperature: 0`, `response_format: {type:"json_object"}`, no tools:

```
SYSTEM: You are a routing classifier. Given the full chat history, pick the cheapest tier that can answer well. Reply exactly one JSON object with no other text.

  smart modes: {"tier": "free"|"lowest"|"low"}
  smart/max modes: {"tier": "free"|"lowest"|"low"|"think"}

  tiers:
    free   — trivial chat, greetings, reformatting, facts answerable by small models.
    lowest — general help where lowest paid cost wins (no judged floor).
    low    — cheapest judged offering (known quality floor).
    think  — needs frontier quality/deep reasoning (best judged signals).

  Consider: prompt length, multi-step reasoning, factual risk, need for recency, ambiguity.
  Optional: {"confidence": 0.0..1.0, "reasoning": "one short sentence"} — never shown to user, logged.

USER: tier_set: ["free","lowest","low"]   # or ["free","lowest","low","think"] on smart/max
      prompt: <the buffered chat body — full messages array>
```

The `tier_set` field is how the gateway constrains `max` vs non-max. Unknown values back to `tier_set` are rejected (same as `wrong_type_tag` safety in `crates/catalog/src/judge.rs` Iter 11 A05: fail closed, never widen).

### Response validation

```json
{"tier": "low", "confidence": 0.86, "reasoning": "multi-step synthesis needed"}
```

Rules:
- `tier` must be one of `tier_set` — otherwise `classifier_invalid tier=tier not in gate` and fallback.
- `confidence`/`reasoning` ignored on the wire; logged + emitted in `x-pennyrouter-classifier-*` headers (see §5).
- Non-JSON / empty choices / unknown keys → `classifier_invalid` → fallback (see §4). Never throw 502 on bad classifier JSON; the user prompt still has a real model to serve.

### Cost

Classifier cost is bounded by design: one short chat per smart request; the prompt is the client prompt (already paid for) plus ~120 fixed system tokens. Budget: configurable `CLASSIFIER_MAX_TOKENS` cap on input (reuse the 1M guard in `crates/gateway/src/lib.rs`); `max_tokens` on the classifier call itself clamped (e.g. 64). Cost shown in metrics, see §5. No per-token billing of the classifier to the user — it's an operator cost, like catalog-sync.

## 4. Failure contract — the classifier never 500s the user request

| Classifier outcome | Gateway action | User-visible |
|---|---|---|
| 200 with valid `{tier}` in `tier_set` | Route to that tier normally | Streamed completion |
| `CLASSIFIER_URL` not set / empty | `not_configured` | `503 classifier_not_configured` (fail closed, no guess) |
| Timeout / 429 / 5xx upstream | Retry once with truncated prompt if oversize, else `classifier_unavailable` | Fallback to `low` (or `lowest` if configured via `CLASSIFIER_FALLBACK_TIER`) — still inside `route="smart"` metrics |
| 4xx (auth) | `classifier_auth_error` → `502` (caller's BYOK bad) | User fixes key |
| Invalid JSON / tier not in `tier_set` | `classifier_invalid` → fallback as above | Fallback, counted in `*_invalid_total` |
| Body had no buffered messages (stream probe failed) | `classifier_no_input` → fallback | Fallback |

There is no "fallback chain" composition with Iter 21 chains — the classifier replaces the chain for smart ids. The configured fallback tier is one value, not recursive. Rationale: smart + fallback would hide double hops and confuse cost accounting; keep the DAG to one classifier edge plus one completion edge.

Metrics (labels, all bounded):
- `pennyrouter_classifier_total{result="ok|timeout|invalid|auth_error|unavailable|not_configured", tier="free|lowest|low|think|none"}`
- `pennyrouter_classifier_latency_ms` histogram
- `pennyrouter_smart_fallback_total{tier="low|lowest"}` — classifier failed, fell back to the configured tier
- Logs carry `classifier_model`, `classifier_tier`, `classifier_latency_ms`, `classifier_fallback` booleans (query values redacted as in `crates/gateway/src/lib.rs`'s `redact_query`).

## 5. Wire effects

Smart requests otherwise obey the same contract as any virtual id:
- Token guard (Iter 2, 1M), upstream bounds (Iter 9, 64/128 + 503), timeouts, body bounds (Iter 11 F05 16 MiB) apply to both legs; the classifier leg gets its own `CLASSIFIER_TIMEOUT_MS` (default 8000) and does not share the completion timeout.
- Budget caps (`provider.max_price`, Iter 14), `only`/`ignore` / `quantizations` restrictions (Iter 11 F01), tool-awareness (`supports_tools`, Iter 22), quality floors per-tier (Iter 14 `tier_floor`), throttle-skip (Iter 21 header-derived cool-down) — all enforced on the final tier pick. The classifier never overrides a cap.
- Strict fail-closed (Iter 14): `budget_exceeded`/`quality_floor_unmet`/`no_eligible_offering`/`tools_unsupported` still fire; the classifier is not a free pass. If the classified tier rejects, do not silently re-pick another tier — that would hide the classifier's decision under a second policy. Return the rejection with the classifier headers attached.
- Session stickiness (`docs/routing.md`): classifier decision is not pinned. The `(authorization, openrouter/pair/smart[/max])` session key exists for consistency, but the classifier runs per request — "easy now, hard later" should not be sticky. The final tier's within-tier provider pinning (majority class + ranking + throttle skip) is still sticky as usual.

Response headers added (all optional, never leak prompt):
```
x-pennyrouter-smart-tier: low
x-pennyrouter-smart-classifier: openai/gpt-4o-mini
x-pennyrouter-classifier-confidence: 0.86
x-pennyrouter-classifier-latency-ms: 214
x-pennyrouter-classifier-fallback: true   # when §4 fallback fired
x-pennyrouter-provider: <actual serving provider as today>
```

`GET /ui` (Iter 17, `GATEWAY_UI=1`) adds a `Smart classifier` section: classifier model/base/fallback tier, counts + p50/p95 latency, invalid rate, last error.

## 6. Config surface

```
CLASSIFIER_MODEL              # e.g. openai/gpt-4o-mini (unset ⇒ smart disabled)
CLASSIFIER_BASE_URL           # override upstream for classifier only; default OPENROUTER_BASE_URL
CLASSIFIER_API_KEY            # optional bearer override for classifier leg
CLASSIFIER_TIMEOUT_MS         # per-call budget, default 8000
CLASSIFIER_FALLBACK_TIER      # when classifier unavailable, which tier to serve: low|lowest (default low)
SMART_THINK                   # legacy alias for "smart/max includes think" — if set, smart already spans think; otherwise smart is 3-tier and smart/max is 4-tier (default false → opt-in)
```

No DB migration. No catalog flag. The tier definitions (`VirtualTier::Free|Low|Think` plus `lowest` from Iter 19) stay authoritative in `crates/catalog/src/price.rs` + `crates/gateway/src/routing.rs::decide_tier`. `smart` and `smart/max` are sugar over that dispatch — a new tier automatically appears in smart's output domain once it exists in routing.

Privacy: `privacy/smart` and `privacy/smart/max` attach `zdr:true` + `data_collection:"deny"` on both classifier and completion wire like any privacy virtual (§routing `privacy` handling). No new redaction rule needed — query redaction already covers log paths.

## 7. Tests + acceptance

Unit: classifier JSON validation (valid/invalid/missing tier, wrong tier_set, extra keys), prompt rendering, fallback tier selection, privacy header forwarding to classifier.

Integration (mock classifier + mock OpenRouter): smart picks each tier; smart/max allows think; unconfigured → 503 fail-closed; timeout → fallback to low; invalid tier → fallback + `fallback:true` header; tool-needing prompt classified free → final request still fails with `tools_unsupported` (classifier does not bypass); 429 on final leg still cools down and counts in `pennyrouter_upstream_throttled_total`.

Live smoke (optional, same bar as `catalog-sync --fetch-openrouter` in Iter 18): one `POST /v1/chat/completions` with `model: openrouter/pair/smart` against real OpenRouter, classifier on `openai/gpt-4o-mini`, body round-trips.

## 8. What this deliberately does not add

- No per-token cost prediction/classifier for pricing (see `docs/pricing.md` — that awaits a real price classifier; this one picks tier, not dollars).
- No parallel fan-out or judge-on-response (the deferred `future.md` items "Parallel multi-model calls" / "Judge / decision model — realtime scoring" stay deferred; the catalog's batch judge `crates/catalog/src/judge*.rs` stays offline and never on the path).
- No new catalog columns, ranking fields, or offering traits — the smart layer reuses existing tiers.
- No new suffix tag `smart/...` on arbitrary model ids — if needed later, it's a follow-up that must handle the suffix-lookup interaction explicitly; for now `smart` lives only as the full virtual id.
- No multi-hop fallback (`smart → free → low → think`); see §4.

## 9. Alternatives considered

- A tag filter like `model/…/smart`: rejected — tags ride on the `model` field and interact with `split_model`/`orientation_from` parsing on real catalog models; a dedicated virtual id keeps the grammar clean (`parse_virtual` is already strict — see `crates/gateway/src/routing.rs`).
- Embedding classifier inside `decide_virtual` without a network call: would require shipping the classifier in the binary or Wasm; network call keeps this proposal testable with the existing async proxy path and swappable across vendors.
- Default-smart for all traffic: rejected — doubles latency and cost for callers that explicitly asked free/paid, violates the realtime contract.

## 10. Rollout

Iter A — docs + unit: `openrouter/pair/smart` in `VIRTUAL_MODELS`, `parse_virtual` + routing stubs returning `classifier_not_configured` with no new upstream code. Tests + docs land; gateway behavior unchanged.

Iter B — classifier leg + wiring: `classifier.rs` (prompt build + call + JSON validation), `SMART_MODEL_IDS` set, `smart/max` + privacy variants, env wiring, metrics, `/ui` panel, live smoke. No DB change; no catalog change.

Both iters keep exactly-once semantics for non-smart paths; smart pays exactly two legs (classifier + one completion, plus one availability retry on the completion if 429/502/503/504). The classifier never retries by itself.

## References

- `crates/gateway/src/lib.rs` — `OPENROUTER_BASE_URL`, `fallback_proxy`, `forward_upstream`, `routed_completion`, `track`/`_in_flight`, 1M guard, `GATEWAY_UI` gating.
- `crates/gateway/src/routing.rs` — `parse_virtual`, `VirtualTier`/`VirtualModel` + `fallback: Option<String>`, `VIRTUAL_MODELS`, `FREE_MODEL`, `decide_virtual`/`decide_tier`/`decide_free`/`decide_lowest`, `throttle_until` + `MAX_COOLDOWN`, `ProviderConstraints` (budget + tool checks), `ProviderConstraints::from_body`.
- `crates/catalog/src/schema.rs` / `store.rs` / `process.rs` — schema v4 + `supports_tools`, `EndpointIndex::supports_tools`.
- `crates/catalog/src/judge*.rs` — batch judge (stays offline).
- `docs/api.md`, `docs/architecture.md`, `docs/routing.md`, `docs/ranking-index.md`, `docs/pricing.md`, `docs/operations.md`, `docs/future.md`.
- `docs/proposals/2026-11-01-openrouter-interface.md` — OpenRouter-native surface (this PR is orthogonal).
