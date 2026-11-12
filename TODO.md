# TODO — Iteration Plan

Rule: each iteration ships one small layer that is testable by itself and
adds value on its own. Detail and acceptance criteria live in
[docs/roadmap.md](docs/roadmap.md). Status source: TODO checkboxes;
roadmap headers mirror them in the same commit.

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
- [x] Local/server compose files with `.env.dev` + `.env` settings
      (`compose.yaml` builds locally and publishes ports;
      `compose.dev.yaml` pulls the `ghcr.io` images with no
      `build:` section, CI-guarded, proxy-reached with no
      published ports).
- [x] `scp` server deploy (`scripts/deploy.sh` + `make deploy`):
      ships pull-only server compose + merged `.env` (`.env.dev`
      base + `.env` secrets), pulls the `ghcr.io` images,
      restarts, health-checks via the proxy network (one-off
      curl container, no host port to probe).
      `Makefile` covers current common tasks
      (test/run/dev/build/smoke/load/compose-config).
- [x] Git-stack deploy (same pull-only compose file as a git
      stack, push-to-deploy via repo webhook;
      `scripts/deploy-webhook.sh` + `make deploy-webhook`
      trigger a redeploy via a configured API endpoint).
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
- [x] **Iter 19 — `lowest` tag.** Cross-model cheapest-paid pick with
      no quality floor (fallback when free turns run out); fail-closed
      rejections, exactly-once. Test: 7 routing unit tests (pick,
      first-wins, passthrough, fail-closed, budget, tie-break, wire
      rewrite).
- [x] **Iter 20 — Decode gzipped upstream errors.** Live-found: gzipped
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
- [x] **Iter 25 — OpenRouter-native interface.** `/api/v1/chat/completions`
      + `/api/v1/responses` alias to the routed handler; native fallback
      paths compose against the upstream root (no double `/api`);
      management writes deny by default (`ALLOW_MANAGEMENT_PROXY=1` opts
      in). Test: `tests/openrouter_native.rs` + metrics unit test;
      `cargo test`, clippy, fmt clean.
- [x] **Iter 26 — Smart classifier.** `openrouter/pair/smart[/max]`
      (+ `privacy/` variants) picks a tier per request via one
      decision-model leg (`free`/`lowest`/`low`, plus `think` on
      `smart/max`), then the normal tier dispatch with no session
      pinning; fail-closed when unconfigured, marked fallback tier
      otherwise, rejected tiers never re-picked; classifier metrics +
      `/ui` panel. Test: `crates/gateway/tests/smart_classifier.rs`
      (16 integration over mock classifier + mock upstream) +
      classifier/verdict + routing unit tests. `cargo test`, clippy,
      fmt clean.
- [x] **Iter 27 — Decisions route.** `POST /v1/decisions` forwards
      model-rewritten bodies to the upstream alpha endpoint with
      retry + decision-only fallback. Test: 5 integration (route,
      fail-closed, passthrough, fallback, wrong-target).
- [x] **Iter 28 — Pinned-model routes.** `openrouter/pair/model/<id>`
      (+ `privacy/model/<id>`) pins to one specific catalog model and
      picks the best provider for it — ranking/cost, throttle skip,
      budget caps, tool filter, `.../fallback/<tier>` all scope to that
      model's offerings. Test: parse + integration (route/fallback/
      budget/tool/throttle/privacy/unknown).
- [x] **Iter 29 — Routing inspector.** `GET /ui` inspector + `GET
      /ui/preview?model=<id>&top=N` (behind `GATEWAY_UI=1`): fixed
      non-smart virtual combos top-3 plus a custom-model box, every
      stored column (prices, effective cost, class, tier/score, tools,
      decisions, promo, free, availability, rank) with sticky-vs-fresh
      picks and the session hash. Key travels as `Authorization`,
      never logged/stored; previews never pin. Test: `tests/ui.rs`
      preview suite (ordering, sticky/fresh, no-pin, throttle,
      disabled gate, page content).
- [x] **Iter 30 — OpenRouter catalog judge.** `--judge-backend
      openrouter` judges through an operator-picked OpenRouter
      model (`OPENROUTER_JUDGE_MODEL`, fail-closed) on the shared
      OpenAI-chat core, keyed by the shared `RANKINGS_API_KEY`;
      cataloged like the other live backends. Test: stub-server
      round trip + fail-closed matrix + catalog + CLI dispatch.
- [x] **Iter 31 — Artificial Analysis index ingestion.** Schema
      v5→v6: `models` gains `aa_intelligence/coding/agentic_index`
      (ints rounded from live int/float values, `None` when
      absent/null/non-numeric/out-of-range), distilled by
      `run_processing`; gateway snapshot carries the coding map
      (display-only, `/ui` model headers) for future filters.
      Deploy with/after the first v6 publish (v6 gateway rejects
      v5 artifacts; last-good covers reloads). Test: extract
      vectors + migrate + processing + snapshot carry + live
      one-shot (168/469 coding).
- [x] **Iter 32 — Scoring rename + offline tiers.** Judging →
      scoring end to end (`Scorer`, `--score-backend`,
      `scorers.json`/`scores.json`, `OPENROUTER_SCORE_MODEL`,
      `MODAL_SCORE_URL`, schema v6→v7 audit-column rename);
      `OfflineScorer` derives tiers from the AA coding index
      (70+ high, 40+ standard, else low) in `run_processing`
      on untiered rows only — online verdicts always win.
      Deploy with/after the first v7 publish (v7 gateway rejects
      v6 artifacts) + rename the two env vars in `.env`. Test:
      cutoff/clamp/identity units + processing tiers/meta +
      precedence + v6→v7 migrate + CLI offline line; full
      workspace green (39 suites).
- [ ] **Iter 34 — Catalog value score + catalog UI (proposed).**
      Sync computes a deterministic `rank_score` per offering —
      `quality × efficiency × popularity`, where quality is the AA
      index (intelligence → coding → agentic, recorded in
      `rank_basis`) times a precision factor (fp8 0.97, int8 0.95,
      int4/fp4 0.88, unknown 0.95), efficiency is
      `COST_REF/(cost_per_task+COST_REF)` with `COST_REF` = $0.01
      (the live median), and popularity is a ≤10% factor from the
      rankings window; speed then availability are tie-breakers.
      No AI, no external ranking. Schema v7→v8 adds latency/
      throughput/uptime, popularity, and the `rank_*` components;
      the ranking document gains an optional `score` ordering the
      gateway ignores for now. Catalog UI becomes an overview
      (coverage counts, data-quality warnings, the formula and
      constants, a worked example) plus a models page with
      `free`/`tiers`/`decision` tabs sorted by score, each row
      expanding to its full arithmetic. Proposal:
      `docs/proposals/2026-11-12-catalog-ui-value-score.md`.
