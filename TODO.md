# TODO — Iteration Plan

Rule: each iteration ships one small layer that is testable by itself and
adds value on its own. Detail and acceptance criteria live in
[docs/roadmap.md](docs/roadmap.md).

- [x] **Iter 0 — Repo skeleton + docs (this turn).** Value: reviewable
      structure, vision, and plan before any code. Test: tree + link audit.
- [ ] **Iter 1 — Minimal non-streaming passthrough.** `POST
      /v1/chat/completions` → OpenRouter, caller-supplied key, byte-correct
      response. Test: integration test against a mock backend + recorded
      fixture; must not depend on any routing or DB code.
- [ ] **Iter 2 — Streaming + guards.** SSE streaming passthrough, timeouts,
      error mapping, 1M-token guard. Test: chunked fixture streams intact;
      oversize request rejected before touching the backend.
- [ ] **Iter 3 — Observability + perf baseline.** Latency histogram, basic
      metrics/logs, load harness, 1 CPU / 1 GB Docker profile. Test: report
      gateway overhead vs. direct-to-backend under load.
- [ ] **Iter 4 — Catalog DB + sync.** Local DB of models/providers synced
      from OpenRouter (price, latency, throughput, uptime, ZDR, region,
      promos). Test: fixture sync populates DB; queries return expected rows.
- [ ] **Iter 5 — Provider routing.** Model-name suffix filters (e.g.
      `meta/muse-spark-open/...` tags) plus a good default; one fast local
      lookup; sticky provider per session to preserve cache. Test: routing
      unit tests + passthrough with forced provider; default needs no tags.
- [ ] **Iter 6 — Ranking index.** Periodic AI-assisted rebuild of provider
      ranking + per-app usage trends. Test: rebuild from fixtures yields the
      expected stable ordering; realtime path still a single lookup.
- [ ] **Iter 7 — Price model v1.** Input/output/cache price abstraction,
      free/promo windows, generic `openrouter/free` handling — no classifier.
      Test: pricing fixtures select the expected cheapest provider.

Deferred (see [docs/future.md](docs/future.md), not committed): additional
backends, parallel fan-out (fastest/all/judge), traffic review via a local or
cheap judge model, classifier-based routing.
