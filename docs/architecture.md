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
router ──► routing snapshot (in memory, one fast lookup)
  │  pick provider (sticky per session to preserve cache)
  ▼
backend adapter (OpenRouter first, more later)
  │  forward with caller-supplied key, stream back
  ▼
client
```

Off the realtime path:

```text
catalog sync (periodic) ──► raw provider/model data ──► durable local DB
ranker (periodic, AI-assisted) ──► ranked snapshot ──► atomic in-memory publication
usage trends (per app/key) ──► ranker input
```

## Components

- **Gateway**: OpenAI-compatible HTTP surface, streaming, timeouts, error
  mapping, token guards. Owns the latency budget. Uses bounded queues and
  upstream concurrency limits, plus graceful shutdown.
- **Router**: pure decision function — model + filters + session stickiness
  in, provider out. Must stay a single cheap lookup against an immutable
  in-memory snapshot.
- **Catalog**: durable local DB of models/providers plus the periodic sync
  from OpenRouter (later other sources). Fields stay dynamic/fluid as
  sources change. Background workers publish snapshots; requests do not
  query the DB directly.
- **Ranker**: periodic job that rebuilds the ranked index with AI assistance,
  blending live metrics with usage trends. Publishes versioned snapshots
  atomically. Never on the realtime path.
- **Backend adapters**: one per upstream (OpenRouter first). Forwarding,
  auth passthrough, response normalization. Preserve SSE bytes/order and
  enforce connect, first-byte, idle, and total timeouts.

## Key design decisions

- **Realtime path is dumb and fast.** All intelligence (ranking, trends,
  price modeling) is precomputed; a request does one index read.
- **Immutable routing snapshots.** Background jobs build complete snapshots
  and publish them atomically; requests keep using the last good snapshot
  if a rebuild fails.
- **Bounded realtime resources.** Queue depth and upstream concurrency are
  explicit; overload sheds load instead of growing memory without limit.
- **Durable writes publish after commit.** Catalog updates become visible
  through a new snapshot only after the durable writes commit.
- **Sticky provider per session.** Switching providers mid-session drops
  prompt cache, which dominates cost — stickiness beats marginal per-request
  savings.
- **Caller-supplied keys.** No local secrets to manage; the gateway forwards
  the key from the incoming request.
- **Same id is not the same offering.** Providers serve one model id at
  different FP resolutions/quantizations, so comparison happens within a
  precision class — see [ranking-index.md](ranking-index.md).
- **Dynamic index schema.** Provider fields (price parts, latency,
  throughput, uptime, ZDR, region, precision, promos, index score) change
  often; the catalog must absorb new fields without a redesign.

## Platform track

Deployment evolves in parallel to development (see
[platform.md](platform.md)): Docker images from development, deployments
from platform. Docker first; cloud providers later, all Terraform-managed.
The gateway stays provider-agnostic — no cloud SDKs on the request path.

## Repo layout (target)

```text
PennyRouter/
  README.md  TODO.md  LICENSE  AGENTS.md
  Cargo.toml + Cargo.lock  # workspace root
  .github/                # CI workflows, Dependabot
  crates/
    gateway/              # HTTP server, OpenAI compat
    router/               # routing decisions
    catalog/              # DB + sync
    ranker/               # periodic ranking job
    common/               # shared types, config, errors
  tests/                  # integration + load tests
  docs/                   # this folder
  compose.yaml            # local dev (builds from source)
  compose.prod.yaml       # server deploy (pulls GHCR image, no build)
  .env.example            # settings template (copy to .env)
  docker/                 # Dockerfile
```

Toolchain and crate choices are deferred to Iter 1, not locked here.
