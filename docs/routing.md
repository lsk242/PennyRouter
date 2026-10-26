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
