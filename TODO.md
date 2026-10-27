# TODO — Iteration Plan

Rule: each iteration ships one small layer that is testable by itself and
adds value on its own. Detail and acceptance criteria live in
[docs/roadmap.md](docs/roadmap.md).

- [x] **Iter 0 — Repo skeleton + docs.** Value: reviewable
      structure, vision, and plan before any code. Test: tree + link audit.
- [x] **Iter 0.1 — Safety + platform foundations.** Secret scanning,
      Dependabot, agent conventions, README badges, platform track plan,
      precision-aware comparison notes. Test: workflow configs valid;
      links resolve; still no code.
- [x] **Iter 1a — Health + minimal setup.** Rust workspace, `GET /health`,
      Dockerfile, compose, Docker CI workflow. Test: `cargo test`,
      `/health` smoke, and Docker image build all passed locally.
- [x] **Iter 1b — Non-streaming passthrough.** `POST
      /v1/chat/completions` → OpenRouter, caller-supplied key, byte-correct
      response. Test: mock-backend integration + fixture byte-equality,
      `cargo test`, real-binary smoke, and Docker build + `/health` smoke
      all passed locally; no routing or DB code involved.
- [ ] **Iter 2 — Streaming + guards.** SSE streaming passthrough, timeouts,
      error mapping, 1M-token guard. Test: chunked fixture streams intact;
      oversize request rejected before touching the backend.
- [ ] **Iter 3 — Observability + perf baseline.** Latency histogram, basic
      metrics/logs, load harness, 1 CPU / 1 GB Docker profile. Test: report
      gateway overhead vs. direct-to-backend under load.
- [ ] **Iter 4 — Catalog sync track.** Separate `catalog-sync` process
      delivers a versioned catalog DB artifact (breakdown in
      [docs/catalog-sync-todo.md](docs/catalog-sync-todo.md)). Test:
      fixture-produced artifact validates and loads.
- [ ] **Iter 5 — Provider routing.** Model-name suffix filters (e.g.
      `meta/muse-spark-open/...` tags) plus a good default; one fast local
      lookup; sticky provider per session to preserve cache. Test: routing
      unit tests + passthrough with forced provider; default needs no tags.
- [ ] **Iter 6 — Ranking index.** Periodic AI-assisted rebuild of provider
      ranking + per-app usage trends. Test: rebuild from fixtures yields the
      expected stable ordering; realtime path still a single lookup.
- [ ] **Iter 7 — Price model v1.** Input/output/cache price abstraction,
      free/promo windows, generic `openrouter/free` handling — no classifier.
      Test: pricing fixtures (promo active/expired, cache-heavy/mixed) select
      the expected provider.

## Platform track (parallel — see [docs/platform.md](docs/platform.md))

- [x] Docker local dev + minimal image (Dockerfile + compose added in 1a;
      build + `/health` verified locally via `docker build` and compose).
- [x] Dev/prod compose split with `.env` settings (`compose.yaml`
      builds locally; `compose.prod.yaml` pulls the `ghcr.io` image
      with no `build:` section, CI-guarded).
- [ ] Docker build + push pipeline with `/health` smoke check (workflow
      added in 1a; proves itself on next push to main).
- [ ] Select first cloud provider (Hetzner / AWS / Azure / … — TBD).
- [ ] Terraform-managed deployment: VMs, resource allocation, networking.

Deferred (see [docs/future.md](docs/future.md), not committed): additional
backends, parallel fan-out (fastest/all/judge), traffic review via a local or
cheap judge model, classifier-based routing.
