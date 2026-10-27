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
  to the default provider.

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
- Session = (`Authorization`, model). The first decision pins the
  provider and class; later requests keep the provider, with
  orientation tags refining only which of its offerings serves.
  Anonymous requests route fresh every time. A vanished pin re-routes.
- Enforcement: `model` is stripped to the backend id and the request
  gains `provider: {"order": [<chosen>], "allow_fallbacks": false}` —
  never silently pay another provider. The choice is echoed in the
  `x-pennyrouter-provider` response header.
- Source: `CATALOG_DB` (directory takes the newest verified manifest,
  or a manifest/DB file directly). Unset or unloadable means plain
  passthrough. The snapshot loads once at startup; `reload`/`swap`
  hooks exist for the ranking job (Iter 6).
