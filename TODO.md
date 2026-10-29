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
- [x] **Iter 2 — Streaming + guards.** SSE streaming passthrough, timeouts,
      error mapping, 1M-token guard. Test: chunked fixture streams intact
      (byte-equality + incremental delivery + idle cutoff); oversize
      rejected pre-backend; errors mapped; `cargo test`, real-binary smoke,
      and Docker build + `/health` smoke all passed locally.
- [x] **Iter 3 — Observability + perf baseline.** Latency histogram, basic
      metrics/logs, load harness, 1 CPU / 1 GB Docker profile. Test:
      compare run reported gateway overhead vs. direct-to-mock under load
      (json +0.26ms p50 / +0.40ms p99, sse +0.16ms / −0.10ms, 0 errors);
      `cargo test`, fmt, and Docker build + smoke all passed locally.
- [x] **Iter 4 — Catalog sync track.** Separate `catalog-sync` process
      delivers a versioned catalog DB artifact (breakdown in
      [docs/catalog-sync-todo.md](docs/catalog-sync-todo.md), all phases
      done). Test: fixture-produced artifact validates and loads.
- [x] **Iter 5 — Provider routing.** Model-name suffix filters (e.g.
      `meta/muse-spark-open/...` tags) plus a good default; one fast local
      lookup; sticky provider per session to preserve cache. Test: routing
      unit tests + passthrough with forced provider; default needs no tags;
      `cargo test`, clippy, fmt, and Docker build + smoke all passed.
- [x] **Iter 6 — Ranking index.** Periodic AI-assisted rebuild of provider
      ranking + per-app usage trends. Test: rebuild from fixtures yields the
      expected stable ordering; realtime path still a single lookup
      (routing tests still pass); `cargo test`, clippy, fmt, Docker
      build + smoke all passed.
- [x] **Iter 7 — Price model v1.** Input/output/cache price abstraction,
      free/promo windows, generic `openrouter/free` handling — no classifier.
      Test: pricing fixtures (promo active/expired, cache-heavy/mixed) select
      the expected provider; `cargo test`, clippy, fmt, Docker build +
      smoke all passed.
- [x] **Iter 8 — Drop-in fallback proxy.** Any path without a custom
      handler proxies upstream untouched (method/path/query/headers/body)
      and logs method + full path; same 1M cap, timeouts, error mapping.
      Test: fallback tests (GET/POST proxied intact, oversize 413) +
      live `/v1/models` 200 through the gateway; `cargo test`, clippy,
      fmt all passed.

## Platform track (parallel — see [docs/platform.md](docs/platform.md))

- [x] Docker local dev + minimal image (Dockerfile + compose added in 1a;
      build + `/health` verified locally via `docker build` and compose).
- [x] Dev/prod compose split with `.env` settings (`compose.yaml`
      builds locally; `compose.prod.yaml` pulls the `ghcr.io` image
      with no `build:` section, CI-guarded).
- [x] `scp` server deploy (`scripts/deploy.sh` + `make deploy`):
      ships pull-only prod compose + generated `.env`, pulls the
      `ghcr.io` image, restarts, health-checks; gateway only on
      host port 3000. `Makefile` covers current common tasks
      (test/run/dev/build/smoke/load/prod-config).
- [x] Docker build + push pipeline with `/health` smoke check (proven
      green on main across the v0.1.0–v0.6.0 merges, image pushed).
- [x] Select first cloud provider: Hetzner (see
      [docs/cloud-providers.md](docs/cloud-providers.md)).
- [x] Terraform-managed deployment (`terraform/hetzner`): CCX13 VM,
      primary IPv4, private network, firewall, Caddy TLS + pull-only
      gateway via cloud-init; CI runs fmt + validate.

Deferred (see [docs/future.md](docs/future.md), not committed): additional
backends, parallel fan-out (fastest/all/judge), traffic review via a local or
cheap judge model, classifier-based routing.

## Next (planned — detail in [docs/roadmap.md](docs/roadmap.md))

- [ ] **Iter 9 — Resilience bounds.** Bounded upstream concurrency +
      queue depth with explicit 429/503 shedding + graceful shutdown.
      Test: soak sheds cleanly; SIGTERM drains.
- [ ] **Iter 10 — Catalog freshness.** Periodic `CATALOG_DB` reload +
      atomic snapshot publication + last-good fallback; resolve
      routing-vs-operations contradiction. Test: new artifact picked
      up without restart; corrupt artifact keeps serving.
- [ ] **Iter 11 — Bug-review fixes.** F01–F11 (keys done separately) +
      A01–A05: provider-field preservation, gzip, endpoint/precision
      enforcement, upstream body bounds, SSE failure rule, atomic
      publish, Terraform `/api` base, readiness loop,
      None-scores-first, promos→free filter; hashed session keys + TTL.
      Test: one regression test per finding.
- [ ] **Iter 12 — Hot-path hygiene + fresh baseline.** Raw-envelope
      passthrough, stream-or-bound bodies, buffered logging, less lock
      contention; re-run `tests/load`, record numbers. Test: load
      compare vs Iter 3 baseline.
- [ ] **Iter 13 — Virtual model catalog, free-first.** Six virtual ids
      (`openrouter/pair/free`, `/low`, `/think` + `/privacy/…`
      variants; provider-first, OpenRouter-only for now); tier
      definitions; blended
      input/output/cache price metric with FP-resolution classes and
      eligibility-first comparison; retry/fallback policy; free-tier
      rate-limit awareness (model info + 429 history); curated local
      `GET /v1/models`. Test: each virtual id routes; models list
      returns exactly the six.
- [ ] **Iter 14 — Product gaps.** Quality floor, strict-budget
      fail-closed, live free eligibility, affinity, Responses routing.
      Test: over-budget fails closed; ineligible never wins on price.
- [ ] **Iter 15 — Decision-model catalog + selection.** Catalog which
      judge model serves which backend, selection policy
      (recorded vs live, sampling), cost caps, judgment audit trail;
      judges stay offline batch, never on the request path. Test:
      each cataloged judge reproduces fixtures; spend capped.
