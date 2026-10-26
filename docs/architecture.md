# Architecture

Long-term direction. Each layer arrives in its own small iteration per
[roadmap.md](roadmap.md); nothing here is built yet.

## Data flow

```text
client (OpenAI format, carries backend key)
  │  POST /v1/chat/completions
  ▼
gateway (Rust HTTP server)
  │  parse model + suffix filters
  ▼
router ──► ranking index (local DB, one fast lookup)
  │  pick provider (sticky per session to preserve cache)
  ▼
backend adapter (OpenRouter first, more later)
  │  forward with caller-supplied key, stream back
  ▼
client
```

Off the realtime path:

```text
catalog sync (periodic) ──► raw provider/model data ──► local DB
ranker (periodic, AI-assisted) ──► ranked index ──► local DB
usage trends (per app/key) ──► ranker input
```

## Components

- **Gateway**: OpenAI-compatible HTTP surface, streaming, timeouts, error
  mapping, token guards. Owns the latency budget.
- **Router**: pure decision function — model + filters + session stickiness
  in, provider out. Must stay a single cheap lookup.
- **Catalog**: local DB of models/providers plus the periodic sync from
  OpenRouter (later other sources). Fields stay dynamic/fluid as sources
  change.
- **Ranker**: periodic job that rebuilds the ranked index with AI assistance,
  blending live metrics with usage trends. Never on the realtime path.
- **Backend adapters**: one per upstream (OpenRouter first). Forwarding,
  auth passthrough, response normalization.

## Key design decisions

- **Realtime path is dumb and fast.** All intelligence (ranking, trends,
  price modeling) is precomputed; a request does one index read.
- **Sticky provider per session.** Switching providers mid-session drops
  prompt cache, which dominates cost — stickiness beats marginal per-request
  savings.
- **Caller-supplied keys.** No local secrets to manage; the gateway forwards
  the key from the incoming request.
- **Dynamic index schema.** Provider fields (price parts, latency,
  throughput, uptime, ZDR, region, promos, index score) change often; the
  catalog must absorb new fields without a redesign.

## Repo layout (target)

```text
PennyRouter/
  README.md  TODO.md  LICENSE
  Cargo.toml              # workspace (Iter 0/1)
  crates/
    gateway/              # HTTP server, OpenAI compat
    router/               # routing decisions
    catalog/              # DB + sync
    ranker/               # periodic ranking job
    common/               # shared types, config, errors
  tests/                  # integration + load tests
  docs/                   # this folder
  docker/                 # Dockerfile, compose for local dev
```

Toolchain and crate choices are deferred to Iter 0/1, not locked here.
