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

- [x] **Iter 9 — Resilience bounds.** Bounded upstream concurrency +
      queue depth with explicit 503 shedding + graceful shutdown.
      Test: soak sheds cleanly (`tests/resilience.rs`); SIGTERM drains.
      `cargo test` (81 passed), clippy, fmt clean.
- [x] **Iter 10 — Catalog freshness.** Periodic `CATALOG_DB` reload +
      atomic snapshot publication + last-good fallback; resolve
      routing-vs-operations contradiction. Test: new artifact picked
      up without restart; corrupt artifact keeps serving
      (`tests/reload.rs`). `cargo test` (84 passed), clippy, fmt clean.
- [x] **Iter 11 — Bug-review fixes.** F01–F11 (F08 verified
      pre-fixed) + A01–A05 + hardening (query redaction, hashed
      session keys + TTL, hop-by-hop strip, no-follow redirects).
      Test: one regression test per finding (`tests/hardening.rs`,
      F01/F04/F10/F11 unit + rank/process/price/judge tests).
      `cargo test` (105 passed), clippy, fmt clean.
- [x] **Iter 12 — Hot-path hygiene + fresh baseline.** Buffered
      logging thread, sessions RwLock; re-ran `tests/load`, recorded
      numbers (json p50 +0.26ms same as Iter 3, p99 tail open).
      `cargo test` (105 passed), clippy, fmt clean.
- [x] **Iter 13 — Virtual model catalog, free-first.** Six virtual ids
      (`openrouter/pair/…`); disjoint tiers; blended price metric +
      known-class guard; single retry (virtual only); 429 cool-down;
      curated local `GET /v1/models`. Test: `tests/virtual_models.rs`
      + unit tests. `cargo test`, clippy, fmt clean.
- [x] **Iter 14 — Product gaps.** Quality floor, strict-budget
      fail-closed, live free eligibility, affinity, Responses routing.
      Test: over-budget fails closed; ineligible never wins on price.
- [x] **Iter 15 — Decision-model catalog + selection.** Catalog which
      judge model serves which backend, selection policy
      (recorded vs live, sampling), cost caps, judgment audit trail;
      judges stay offline batch, never on the request path. Test:
      each cataloged judge reproduces fixtures; spend capped.
- [x] **Iter 16 — Provider alternatives (modal, baseten).**
      Provider-pinned direct routes with pure BYOK (caller token
      forwarded unchanged); unset provider fails closed. Test:
      per-upstream routing + auth forwarding + field stripping.
- [x] **Iter 17 — Debug UIs, off by default.** Gateway `GET /ui`
      (ranking + metrics) behind `GATEWAY_UI=1`; `catalog-ui`
      browser (runs, catalog, ranking, log) behind `CATALOG_UI=1`.
      Test: disabled falls through / refuses; pages serve.
- [x] **Iter 18 — Live OpenRouter fetch.** `catalog-sync
      --fetch-openrouter` pulls the public `GET /models` list plus one
      per-model endpoints body (no auth, no keys) into the same fixture
      boundary; `FETCH_OPENROUTER=1` flips the container off fixtures.
      Test: stub-server fetch tests; live one-shot synced 456 models /
      1483 endpoints with 18 zero-price offerings.
- [ ] **Iter 19 — `lowest` tag.** Cross-model cheapest-paid pick with
      no quality floor (fallback when free turns run out); fail-closed
      rejections, exactly-once. Test: 7 routing unit tests (pick,
      first-wins, passthrough, fail-closed, budget, tie-break, wire
      rewrite).
- [ ] **Iter 20 — Decode gzipped upstream errors.** Live-found: gzipped
      4xx bodies were wrapped raw into error messages. Decode (bounded)
      on the error path only; success bytes stay untouched. Test: 2
      passthrough tests (readable wrap, decoded JSON forward without
      encoding claim).
- [x] **Iter 21 — Free fallback + header-driven 429s.** `…/free/fallback/
      <tier>` serves free, then one tier pick on exhaustion; 429s cool
      offerings for the header-stated duration and surface reset info.
      Test: unit (headers, parse, triggers) + 2 integration (fallback
      200, 429 headers). Live: fallback chain serves DeepInfra paid
      pick on constrained trigger.
- [x] **Iter 22 — Tool-aware routing.** Index per-endpoint tool
      support (`supported_parameters` → `supports_tools`, schema v4);
      tool-bearing requests match only capable endpoints on every
      leg; starvation fails closed with `tools_unsupported` (400).
      Test: extraction asserts (1/0/NULL) + unit (detect/allow) + 2
      integration (routes capable, fails closed).
- [x] **Iter 23 — Standby router recovery.** Boot against an
      unloadable catalog attaches an empty standby router; the
      periodic reload heals it when a good artifact lands.
      Test: standby-heals integration (passthrough → routed, ready).
- [x] **Iter 24 — Decision tier.** `openrouter/pair/decision[/free]`
      routes decisions-capable offerings (schema v5 index from model
      modality); `decision/free/fallback/<tier>` falls back past
      exhaustion; `EXTRA_MODEL_IDS` syncs unlisted models.
      Test: extraction trio + fetch merge + parse + 3 integration
      (route, fallback, fail-closed).
- [x] **Iter 25 — Decisions route.** `POST /v1/decisions` forwards
      model-rewritten bodies to the upstream alpha endpoint with
      retry + decision-only fallback. Test: 5 integration (route,
      fail-closed, passthrough, fallback, wrong-target).
