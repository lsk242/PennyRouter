# Routing

Basic provider routing only — no classifier except the opt-in smart leg
([Smart classifier](#smart-classifier-iter-26)), which picks tiers,
never dollars. (Iter 5; data from Iter 4, ranking from Iter 6.)

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

Initial tag vocabulary (grows with the index): price orientations,
`privacy` (zero data retention); latency / throughput orientations
and region hints parse but are currently ignored — the index carries
no live signal for them (see §Status).
`lowest` (Iter 19) is the cross-model cheapest-paid pick: it rewrites
to whatever paid offering costs least catalog-wide, with no quality
floor — the fallback when free turns run out.
`openrouter/pair/free/fallback/<tier>` (Iter 21) chains them: serves
free while any free offering is eligible, else exactly one pick from
the named tier (`low`, `think`, `lowest`, `decision` — resolved
through the shared tier dispatch, so future tiers plug in
automatically). Unknown tier names fail closed with
`no_eligible_offering` at request time (parse accepts any non-empty
target; the dispatch rejects unknown names); auth/config errors
surface, never fallback; the fallback leg gets no further retry.

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
  `low` tier, minus its scored-tier floor. Fails closed like
  virtual ids (`no_eligible_offering` 503, `budget_exceeded` 400)
  instead of leaking the tagged id upstream; exactly-once (tags
  never retry). Unknown base models still pass through untouched,
  and first recognized orientation wins (`price/lowest` stays
  same-model).
- Free-fallback chains (Iter 21): `openrouter/pair/free/fallback/<tier>`
  (`low` | `think` | `lowest` | `decision`). Free leg first with existing free
  semantics; on free exhaustion (`no_eligible_offering`) or upstream
  429/502/503/504 after the single virtual retry, one fallback pick
  that skips the just-failed provider. Session pins under the full
  chain id. Upstream 429s cool offerings for the header-stated
  duration (`Retry-After`, else `X-RateLimit-Reset` epoch-ms, else
  60s) instead of a flat timer, and 429 responses carry the
  upstream's `X-RateLimit-*` headers to the client.
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
  or a manifest/DB file directly). Unset means plain passthrough with
  no reload task. Unloadable at boot attaches a standby router
  (Iter 23): requests pass through until the periodic reload heals it
  — a missing, mid-write, or schema-older catalog no longer needs a
  restart to recover. The snapshot loads at startup and refreshes every
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
  are disjoint: zero-cost serves `free` only. Per-key daily free caps
  live upstream, not here: OpenRouter's `GET /api/v1/key` reports each
  key's `free_model_daily_requests` used/limit/remaining, and an
  exhausted key surfaces as 429s — which the free leg already turns
  into retry + fallback. The gateway never pre-checks per-key quota
  (no request-path key inspection). Throttled providers
  (recent 429s, 60s cool-down, counted in
  `pennyrouter_upstream_throttled_total`) yield to fresh ones when any
  remain. `privacy/` variants enforce `zdr: true` +
  `data_collection: "deny"` unless the caller set them; true
  provider-side privacy filtering waits on index signals.
- Decision tier (Iter 24): `openrouter/pair/[privacy/]decision`
  routes to the cheapest decisions-capable offering (behavior-scoring
  models, `architecture.output_modalities` containing `"decisions"`,
  schema v5); `decision/free` scopes it to zero-cost with the same
  free semantics, and `decision/free/fallback/<tier>` falls back past
  exhaustion like the free tier. No quality floor and no precision
  class requirement (scoring endpoints report `unknown`
  quantization); paid-only bare scope keeps tier disjointness. (`POST
  /v1/decisions` fails `privacy/*` closed before routing — see
  Decisions route below.) The
  tier only sees what the catalog synced: the models list omits
  non-text-output models, so `EXTRA_MODEL_IDS` (comma-separated,
  catalog-sync env) nominates such ids for individual fetching.
- Decisions route (Iter 27): `POST /v1/decisions` dispatches through
  the same virtual ids (`decision`, `decision/free`,
  `decision/free/fallback/decision`) but rewrites the model only —
  no `provider`/`quantizations`/privacy injection — and forwards to
  `{OPENROUTER_BASE_URL}/alpha/decisions`. Unknown model ids pass
  through untouched. Retry, throttle memory, and 429 propagation
  match chat; tier fallback serves the `decision` target only.
  `privacy/decision[/free]` fail closed (`privacy_unsupported_on_decisions`,
  400, Q06-B): the decisions API has no verified zero-retention
  field, so the gateway refuses rather than sending on an unkeepable
  promise. Privacy on decisions means pinning a model directly —
  explicit caller choice, documented as such.
- Pinned-model routes (Iter 28): `openrouter/pair/[privacy/]model/\
  <model-id>` pins to one specific catalog model (the model id may
  contain slashes, e.g. `deepseek/deepseek-v4.1-flash`). The router
  picks the cheapest eligible provider for that model — ranking order
  when the catalog has it, otherwise inline effective cost — with
  provider/budget/tool/throttle filtering scoped to that model's
  offerings. `.../fallback/<tier>` falls through to the named
  cross-model tier when the pinned leg has nothing to serve, just like
  `free/fallback/<tier>`. Privacy/budget/auth semantics mirror the
  other virtual routes. An unknown model id fails closed with
  `no_eligible_offering` (503) rather than leaking the virtual name.
- AA coding-index floor (Iter 33): an optional `/minindex/<0..=100>`
  suffix on any virtual id (`low`, `think`, `decision`, `free`,
  `model/<id>`, `smart`) keeps only models whose catalog
  `aa_coding_index` is at or above the floor. It sits after the core
  id and before `/fallback/`:
  `openrouter/pair/privacy/think/minindex/40/fallback/low`. The floor
  is the first eligibility gate — a cheaper model below it (or with
  no index at all) never wins, and `minindex/0` means "must have any
  index". It scopes the primary leg only; the fallback tier runs
  unfiltered. An unusable value (empty, non-numeric, out of range,
  duplicated) fails closed with `invalid_model` (400); a satisfiable
  floor with no surviving candidate fails closed with
  `no_eligible_offering` (503). Routed responses echo the floor in
  `x-pennyrouter-minindex`.
- Cost (Iter 7): "cheapest" compares effective cost under the balanced
  workload profile (see [pricing.md](pricing.md)), promo column
  applied; unpriceable offerings sort last. The `openrouter/free`
  meta-model routes to any zero-cost offering and rewrites to its
  backend id, re-routing pins that stopped being free.
- Quality floors (Iter 14): each virtual tier promises a minimum
  scored quality — `low` needs any scored tier, `think` needs `high`
  (`free` needs none). A tier whose candidates all sit below its
  floor rejects with `quality_floor_unmet` (503) instead of serving
  below the promise.
- Tool-aware routing (Iter 22): the catalog indexes per-endpoint tool
  support (`supported_parameters` containing `"tools"`, schema v4;
  unknown on older catalogs). A request with a non-empty `tools`
  array matches only `Some(true)` endpoints — across `free`, `low`,
  `think`, `lowest`, direct-model, and fallback legs — so the gateway
  never pins a provider the upstream would 404 with "No endpoints
  found that support tool use". Tool-bearing requests starved while
  plain ones would serve fail closed with `tools_unsupported` (400):
  disable `tools` or choose a tool-capable route. Requests without
  tools ignore the flag entirely.
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

## Routing inspector (Iter 29)

`GET /ui/preview?model=<id>&top=N` (behind `GATEWAY_UI=1`) previews
one `model` value without routing or pinning anything: the ordered
top-N candidates with every stored column the router reads (prompt /
completion / cache-read / cache-write prices, blended effective
cost, precision class, quality tier/score, tool + decisions support,
promo discount, free eligibility, throttle availability, rank-doc
index) plus the fresh pick (no session) and the sticky pick (with
the given key) with the session namespace and hash. The key travels
as the `Authorization` header — the same raw value sessions pin
with — so it never lands in URLs or logs, and the preview never
stores a pin. Throttled pins fall through exactly like the request
path; fallback ids preview the leg that would serve; smart ids
preview as `smart_requires_classifier` because their tier comes
from the live classifier leg, not the local index. The `/ui` page
renders the ten fixed non-smart virtual combos top-3 plus a custom
model box on top of this endpoint. Plain-request view: tools, budget,
and only/ignore constraints are not applied, so a capped request can
fail closed where the preview shows a pick.
Session pins are keyed by a seeded 64-bit non-cryptographic hash of
the raw key (preview echoes the hash, never the key): with
high-entropy BYOK keys collisions are impractical, so no KDF slows
the request path — accepted risk, revisit only if keys become
low-entropy.

## Smart classifier (Iter 26)

`openrouter/pair/smart` (tiers `free`/`lowest`/`low`),
`openrouter/pair/smart/max` (plus `think`), and the `privacy/`
variants call a decision model on the request path to pick the
cheapest tier that can answer well. This is the one explicit,
scoped exception to the realtime contract below: the caller opts
into the extra leg by choosing the id. Every other id keeps pure
local lookup with no network on the routing path.

- Leg 1 (classify): one short non-streaming chat call to
  `CLASSIFIER_MODEL` (`CLASSIFIER_BASE_URL`, default
  `OPENROUTER_BASE_URL`), `temperature: 0`,
  `response_format: {type: "json_object"}`, capped output
  (`CLASSIFIER_MAX_TOKENS`, default 64) and input
  (`CLASSIFIER_MAX_INPUT_CHARS`, default 32000, oldest dropped
  first). The request carries the tier gate plus the buffered input
  (chat `messages`, or Responses `input`); the reply must be exactly
  `{"tier": ...}` inside the gate (`confidence`/`reasoning`
  optional, anything else rejected).
- Leg 2 (complete): the normal tier dispatch for the classified
  tier — budget, `only`/`ignore`, tool-awareness, quality floors,
  throttle-skip all still apply, and a rejected classified tier
  fails closed (never silently re-picked into another tier). One
  availability retry stays inside the classified tier.
- No session pinning anywhere on the smart path: the classifier runs
  per request, and the final pick never consults a pin (a stale pin
  must not override the classifier).
- Failures: `CLASSIFIER_MODEL` unset → `classifier_not_configured`
  (503, fail closed, no guess); classifier 401/403 →
  `classifier_auth_error` (502); timeout/429/5xx/bad verdict/empty
  input → serve the one `CLASSIFIER_FALLBACK_TIER` (`low`|`lowest`,
  default `low`), marked with `x-pennyrouter-classifier-fallback:
  true`. The classifier leg itself never retries.
- Auth (Q08, BYOK-only): no stored key, ever. The leg uses the
  incoming `Authorization` header, optionally overridden per-request
  by `x-pennyrouter-classifier-key`, else no auth. A foreign
  classifier base therefore receives the caller's key by model-choice
  consent, like pinned routes.
- `privacy/smart[/max]` enforces `zdr: true` +
  `data_collection: "deny"` on both legs.
- `smart` with a `/fallback/` suffix is rejected (`invalid_model`,
  400): the classifier replaces the chain, so the forms never
  compose.
