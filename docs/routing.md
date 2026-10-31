# Routing

Basic provider routing only — no classifier. (Iter 5; data from Iter 4,
ranking from Iter 6.)

## Idea

One model (e.g. `meta/muse-spark-open`) is served by several providers at
different prices, latencies, throughputs, and uptimes. The router picks the
best provider per request with a single local index lookup.

## Filter syntax

Filters ride in the `model` field as suffix segments:

```text
meta/muse-spark-open/privacy/low_cost/speed
```

- First segment(s): the backend model id.
- Remaining segments: filter/flag tags used to select the provider.
- Unknown tags are ignored; absent tags mean "use the default". The default
  route must be good enough that most callers never pass a tag — the custom
  syntax is the smallest feature, not the point.

Initial tag vocabulary (grows with the index): price / latency / throughput
orientations, `privacy` (zero data retention), region hints.
`lowest` (Iter 19) is the cross-model cheapest-paid pick: it rewrites
to whatever paid offering costs least catalog-wide, with no quality
floor — the fallback when free turns run out.

## Why not OpenRouter sorts

OpenRouter already offers `:nitro`, `:floor`, and custom sort orders, but
static sorts miss realtime price/latency/availability fluctuations — and
re-sorting mid-session switches providers, dropping prompt cache at high
cost. PennyRouter instead precomputes a fresh ranking (see
[ranking-index.md](ranking-index.md)) and holds the provider sticky for the
session.

## Realtime contract

- One local lookup per request; no network calls on the routing path.
- Sticky provider per session (cache preservation beats marginal
  per-request savings).
- Precision-aware: same model id at different FP resolutions are distinct
  candidates; never substitute across precision silently.
- Never fail a request because of routing: on any lookup problem, fall back
  to the default provider. Exception (Iter 14): gateway-owned virtual
  ids fail closed with an explicit error instead of leaking upstream,
  where they would 404 opaquely — see quality floors below.

## Status (Iter 5)

- Model/tag split by longest-prefix match against known model ids;
  unknown models pass through untouched.
- Orientations: `price`/`low_cost`/`cost`/`cheap` (cheapest prompt
  price), `quality`/`best` (best tier, then score). First recognized
  orientation wins; `privacy`, `speed`/`latency`/`throughput`, and
  region hints parse but are ignored until the index carries those
  signals — as are unknown tags.
- Default: best quality tier, then cheapest prompt price, within the
  model's most common precision class.
- `lowest` (Iter 19): cheapest paid priceable offering with a known
  precision class across all models — same paid-only filter, budget,
  throttle skip, stickiness (under the full tagged id namespace),
  and deterministic tie-breaks (cost, model id, provider) as the
  `low` tier, minus its judged-tier floor. Fails closed like
  virtual ids (`no_eligible_offering` 503, `budget_exceeded` 400)
  instead of leaking the tagged id upstream; exactly-once (tags
  never retry). Unknown base models still pass through untouched,
  and first recognized orientation wins (`price/lowest` stays
  same-model).
- Session = (`Authorization`, model). The first decision pins the
  provider and class; later requests keep the provider, with
  orientation tags refining only which of its offerings serves.
  Anonymous requests route fresh every time. A vanished pin re-routes.
- Enforcement: `model` is stripped to the backend id and the request
  gains `provider: {"order": [<chosen>], "allow_fallbacks": false}` —
  never silently pay another provider. Every other caller `provider`
  field (`zdr`, `data_collection`, `only`/`ignore`, `max_price`,
  `quantizations`, …) is preserved; candidates and sticky pins must
  satisfy `only`/`ignore`, otherwise the body passes through untouched
  for the upstream to enforce (Iter 11, F01). The chosen precision
  class is forwarded as a `quantizations` requirement unless the
  caller set one — best-effort, since classes are coarser than exact
  backend quantizations; same-provider tier targeting beyond the
  provider pin stays documented-limitation (Iter 11, F04). The choice
  is echoed in the `x-pennyrouter-provider` response header.
- Source: `CATALOG_DB` (directory takes the newest verified manifest,
  or a manifest/DB file directly). Unset or unloadable means plain
  passthrough. The snapshot loads at startup and refreshes every
  `RELOAD_INTERVAL_SECS` (default 60s, 0 disables) via `reload()`,
  which swaps atomically and keeps the last good snapshot on failure;
  `swap` hooks remain for tests and the ranking job (Iter 6/10).
- Virtual models (Iter 13): `openrouter/pair/[privacy/]<free|low|think>`
  resolve before catalog lookup — they are not catalog models, so
  longest-prefix split never sees them. Only the `openrouter`
  provider exists; other providers and unknown tiers pass through.
  `free` shares the legacy free path under its own session namespace;
  `low` picks the cheapest priceable known-class offering across
  models; `think` picks the best quality signals across models. Tiers
  are disjoint: zero-cost serves `free` only. Throttled providers
  (recent 429s, 60s cool-down, counted in
  `pennyrouter_upstream_throttled_total`) yield to fresh ones when any
  remain. `privacy/` variants enforce `zdr: true` +
  `data_collection: "deny"` unless the caller set them; true
  provider-side privacy filtering waits on index signals.
- Cost (Iter 7): "cheapest" compares effective cost under the balanced
  workload profile (see [pricing.md](pricing.md)), promo column
  applied; unpriceable offerings sort last. The `openrouter/free`
  meta-model routes to any zero-cost offering and rewrites to its
  backend id, re-routing pins that stopped being free.
- Quality floors (Iter 14): each virtual tier promises a minimum
  judged quality — `low` needs any judged tier, `think` needs `high`
  (`free` needs none). A tier whose candidates all sit below its
  floor rejects with `quality_floor_unmet` (503) instead of serving
  below the promise.
- Strict budgets (Iter 14): `provider.max_price: {prompt,
  completion}` sets a hard per-token ceiling on raw price columns
  (see [pricing.md](pricing.md)). A malformed cap is no cap; under a
  well-formed cap, unpriceable legs are ineligible. Caps override
  everything, including affinity: a pinned provider above the ceiling
  re-routes, and a virtual request with no in-budget candidate fails
  closed with `budget_exceeded` (400) — the upstream never sees it.
  Rejection bodies are OpenAI-shaped
  (`{"error": {"message", "type", "code"}}`); empty tiers and unmet
  floors answer 503 with `Retry-After: 1`, since a catalog reload or
  cool-down expiry may restore service.
- Affinity vs policy (Iter 14): sticky pins are re-checked against
  the current restrictions on every request. A pin that stops
  satisfying `only`/`ignore`/budget falls through to a fresh pick
  (re-pinned); a vanished provider or class does the same.
- Responses (Iter 14): `POST /v1/responses` runs the same
  route-and-forward path as chat completions — same `model` rewrite,
  provider pinning, budget, floor, retry, and rejection semantics —
  and counts under `route="responses"` in metrics/logs. Other
  fallback endpoints still proxy untouched (virtual ids only exist
  on the two routed endpoints).
- Provider-pinned direct routes (Iter 16): `modal/<model>` and
  `baseten/<model>` bypass the catalog entirely and forward to that
  provider's upstream (`MODAL_BASE_URL` / `BASETEN_BASE_URL`; no
  router or catalog needed, and no `/v1` prefix on the base — the
  gateway appends the incoming path itself). The prefix is stripped, the remainder
  passes through verbatim, and the OpenRouter-specific `provider`
  object is dropped — foreign upstreams speak plain OpenAI chat.
  Auth is pure BYOK: every header, including `Authorization`,
  forwards unchanged, so one caller token serves all upstreams.
  An unset provider fails closed with `provider_not_configured`
  (400) — never a leak to OpenRouter, never a guessed URL. Pinned
  requests are exactly-once (no retry, no throttle steering) and
  carry the provider in `x-pennyrouter-provider` like routed ones.
