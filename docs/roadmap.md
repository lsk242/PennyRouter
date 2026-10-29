# Roadmap

Each iteration ships one small layer: testable by itself, valuable by
itself. No iteration may depend on a later one.

## Iter 0 — Repo skeleton + docs (done)

- Goal: reviewable structure, vision, and plan before any code.
- Value: the team aligns on direction and iteration slices up front.
- Test: tree + link audit — expected files exist, all doc links resolve,
  no source/toolchain files added.

## Iter 0.1 — Safety + platform foundations (done)

- Goal: secret scanning, Dependabot, agent conventions, README badges,
  platform track plan, precision-aware comparison notes — still no code.
- Value: the repo is safe to collaborate in (no leaked keys) and the
  deployment track is defined before the first image exists.
- Test: workflow configs present and valid YAML; all doc links resolve;
  no source/toolchain files added.

## Iter 1a — Health + minimal setup (done)

- Goal: Rust workspace, unauthenticated `GET /health`, Dockerfile,
  compose file, Docker CI workflow.
- Value: proves the Rust → Docker → CI chain before any gateway logic;
  gives the image a container check.
- Test: `cargo test` passes; `/health` returns `{"status":"ok"}` from
  the real binary; Docker image build (116MB) + container smoke check
  passed locally.

## Iter 1b — Non-streaming passthrough (done)

- Goal: `POST /v1/chat/completions` reaches OpenRouter and returns
  correctly, using the caller-supplied key.
- Value: a working gateway baseline everything else builds on; the
  performance-optimization starting point.
- Test: integration test against a mock backend plus a recorded fixture;
  response body byte-correct; must not import routing or DB code.
- Landed: `cargo test` (health + 2 passthrough tests), real-binary smoke
  (`/health` OK, OpenAI-shaped 502 on dead upstream), and Docker build +
  `/health` smoke all passed. Upstream configurable via
  `OPENROUTER_BASE_URL` (default `https://openrouter.ai/api`).

## Iter 2 — Streaming + guards (done)

- Goal: SSE streaming passthrough, timeouts, upstream error mapping,
  1M-token guard.
- Value: real clients (which stream) work; oversized requests fail fast
  without touching the backend.
- Test: chunked fixture streams through intact; oversize request rejected
  pre-backend; backend errors map to sane OpenAI-shaped errors.
- Landed: `cargo test` (9 passed: health, passthrough, streaming,
  guards), real-binary smoke (incremental SSE, 413, error wrap), and
  Docker build + `/health` smoke all passed. Streams key off the
  upstream content type; budgets are connect 10s / first response 30s /
  stream idle 60s / buffered total 300s; the token guard estimates
  bytes/4 and rejects over 1M (~4 MiB) with 413 before contacting the
  backend.

## Iter 3 — Observability + perf baseline (done)

- Goal: latency histogram, basic metrics/logs, load harness, 1 CPU / 1 GB
  Docker profile.
- Value: every later change can prove it did not regress latency or
  resource use.
- Test: load run reporting gateway overhead vs. direct-to-backend.
- Landed: `GET /metrics` (Prometheus counters + latency histogram),
  one JSON log line per request with request ID, and `tests/load/`
  compare harness (mock backend + chat workload + report). Baseline at
  20 sessions x 15s against a 20ms mock, gateway under 1 CPU / 1 GB:
  json p50 overhead +0.26ms / p99 +0.40ms, sse p50 +0.16ms / p99
  −0.10ms (noise), zero errors over ~3.8k requests per side.
  `cargo test` (12 passed), fmt, Docker build + smoke all passed.

## Iter 4 — Catalog sync track (done)

- Goal: a separate `catalog-sync` process (own binary, schedule, host)
  delivers a versioned catalog DB artifact to gateway hosts. Breakdown:
  [catalog-sync-roadmap.md](catalog-sync-roadmap.md).
- Value: the data foundation routing and ranking need; gathering and
  serving evolve independently — they share only the DB artifact.
- Test: fixture-produced artifact validates and loads; detail in the
  catalog-sync roadmap.
- Landed: catalog phases 1–3 done (gathering, deterministic + AI-assisted
  processing, containerized deployment with CI gates); versioned artifact
  publishes, verifies, and loads from fixtures. Phase 4 (live judge
  backends: Baseten + Modal) landed — see
  [catalog-sync-roadmap.md](catalog-sync-roadmap.md).

## Iter 5 — Provider routing (done)

- Goal: model-name suffix filters plus a good default; one fast local
  lookup; sticky provider per session. See [routing.md](routing.md).
- Value: callers get cheaper/faster/more-private providers with zero
  client changes; the default needs no tags.
- Test: routing unit tests over fixture data; passthrough test with a
  forced provider; stickiness test keeps the same provider mid-session.
- Landed: in-memory snapshot from the catalog artifact (longest-prefix
  model split; price/quality orientations; best-tier-then-cheapest
  default within the majority precision class; (authorization, model)
  sessions pin the provider; unknown models pass through untouched).
  Forced provider via OpenRouter `provider.order` with
  `allow_fallbacks: false`; chosen provider echoed in
  `x-pennyrouter-provider`.

## Iter 6 — Ranking index (done)

- Goal: periodic AI-assisted rebuild of provider ranking, blended with
  per-app usage trends. See [ranking-index.md](ranking-index.md).
- Value: routes track realtime fluctuations instead of static sorts;
  trends beat raw metrics where metrics lag.
- Test: rebuild from fixtures yields the expected stable ordering;
  realtime path remains a single lookup (routing tests still pass).
- Landed: `catalog-rank` rebuilds a versioned ranking artifact (signals
  + trend error/latency blend + recorded prefer/avoid adjustments,
  deterministic bytes) per model/precision-class/orientation; the
  gateway serves it with one lookup, falls back to inline sort when
  uncovered, and keeps the last good artifact on rebuild failure.

## Iter 7 — Price model v1 (done)

- Goal: input/output/cache price abstraction, free/promo windows, generic
  `openrouter/free` handling — no classifier. See [pricing.md](pricing.md).
- Value: cheapest-provider selection that survives promo expiry and
  cache-token asymmetry.
- Test: pricing fixtures (promo active/expired, cache-heavy/mixed) select
  the expected provider.
- Landed: workload profiles (balanced/cache-heavy/output-heavy) with
  effective-cost comparison everywhere; time-boxed provider-scoped promo
  windows evaluated at rebuild (active leads, expired falls back);
  `openrouter/free` meta-model routes to any free offering and re-routes
  pins that stopped being free.

## Iter 8 — Drop-in fallback proxy (done)

- Goal: any standard endpoint without a custom handler proxies upstream
  untouched and logs its method + full path — no silent 404s. See
  [api.md](api.md).
- Value: OpenAI-compatible clients work through the gateway for every
  endpoint, while logs reveal which ones are actually used so custom
  handlers can follow demand.
- Test: fallback tests over a mock backend (GET/POST proxied intact,
  oversize rejected pre-backend); live `GET /v1/models` through the
  gateway returns 200 with the fallback log line.
- Landed: generic axum fallback sharing the chat forward path (same
  body cap, timeouts, SSE + error mapping; no routing/rewriting).
  `cargo test` (36 passed), clippy clean, fmt clean.

## Platform track (parallel to development)

Runs alongside the iterations above; see [platform.md](platform.md).
Development delivers Docker images, platform delivers deployments.

- **Now:** Docker local dev + minimal image (Dockerfile + compose added
  in 1a; verified with a local build + `/health` smoke test), plus a
  dev/prod compose split with `.env` settings (prod pulls the GHCR
  image, never builds) and an `scp` server deploy (`scripts/deploy.sh`
  + `make deploy`, gateway only on host port 3000). CI runs secret
  scanning + Dependabot.
- **Docker build + push pipeline:** workflow added in 1a; proven green
  on main (build + push + `/health` smoke) across the v0.1.0–v0.6.0
  merges.
- **First cloud provider (done):** Hetzner, selected from the
  six-provider comparison. Terraform-managed (`terraform/hetzner`):
  CCX13 VM, primary IPv4, private network, firewall, Caddy TLS +
  pull-only gateway via cloud-init; CI validates fmt + config.
- **Later:** more providers on the same patterns; Terraform
  plan/apply automation per environment.

## Iter 9 — Resilience bounds (planned)

- Goal: close the architecture deviation — bounded upstream concurrency
  and queue depth with explicit load-shedding, plus graceful shutdown.
  See [architecture.md](architecture.md) and
  [performance.md](performance.md).
- Value: the gateway degrades with 429/503 instead of collapsing under
  a burst; deploys stop dropping in-flight requests.
- Test: soak past the concurrency bound sheds with 503 (no error
  cascade); SIGTERM drains in-flight requests before exit.

## Iter 10 — Catalog freshness (planned)

- Goal: close the freshness deviation — periodic `CATALOG_DB` reload
  with atomic snapshot publication and last-good fallback, so ranking
  and price updates reach a running gateway without restart.
- Value: re-ranking and promo windows actually take effect; resolves
  the routing-vs-operations contradiction (startup-once vs automatic
  pickup) and wires `CATALOG_DB` in prod compose/Terraform.
- Test: drop a new artifact → gateway serves it without restart;
  corrupt artifact → last-good snapshot keeps serving.

## Iter 11 — Bug-review fixes (planned)

- Goal: fix the
  [security-and-bug review](review/PennyRouter-security-and-bug-review.md)
  findings F01–F11 (keys already addressed separately) plus hardening
  notes A01–A05: preserve provider fields (`max_price`/`only`/ZDR) on
  rewrite, gzip handling, endpoint/precision enforcement, bound
  upstream bodies, SSE failure rule, atomic publish, Terraform `/api`
  base, readiness loop, None-scores-first ordering, promos reaching the
  free filter; hash session keys with TTL/eviction.
- Value: routing stops silently dropping caller constraints or
  misrouting on stale/promo data.
- Test: one regression test per finding; full `cargo test` green.

## Iter 12 — Hot-path hygiene + fresh baseline (planned)

- Goal: act on the
  [resource-efficiency review](review/PennyRouter-resource-efficiency-review.md) —
  raw-envelope passthrough (no full parse + re-serialize when no rewrite
  applies), stream-or-bound non-streaming bodies, buffered structured
  logging (no `println!` per request), less per-request lock contention;
  re-run `tests/load` and record post-Iter-8 numbers (Iter 3 baseline is
  stale).
- Value: the "dumb and fast" path stays allocation-lean; every later
  change has a current baseline to prove against.
- Test: load compare vs the Iter 3 baseline recorded here; no
  allocation growth on the unrouted path.

## Iter 13 — Virtual model catalog, free-first (planned)

- Goal: serve six virtual models and nothing else. Ids carry the
  provider first so future providers slot in (today only OpenRouter,
  BYOK key forwarded as now): `openrouter/pair/free`,
  `openrouter/pair/low`, `openrouter/pair/think`,
  `openrouter/pair/privacy/free`, `openrouter/pair/privacy/low`,
  `openrouter/pair/privacy/think`.
- Tiers: `free` = zero-cost offering; `low` = cheapest at basic level
  (DeepSeek 4.1 Flash class); `think` = flagship reasoning (ChatGPT
  Luna 6 MAX class). No speed/latency axis in v1.
- Price metric: `effective_cost` blended over input/output/cache-read/
  cache-write under the documented workload profile; FP resolution is
  a separate comparison class, never silently substituted; eligibility
  (modality, context, ZDR) filters before price compares.
- Availability: retry/fallback policy per virtual model (revisit
  `allow_fallbacks: false`); for `free`, track rate limits from model
  info plus organic 429 history so routing avoids exhausted offerings.
- `GET /v1/models` answers locally with exactly the six curated
  entries (no upstream proxy leak); chat with a virtual id routes to a
  real backend offering. See [api.md](api.md), [routing.md](routing.md),
  [pricing.md](pricing.md).
- Value: clients see a stable 6-model menu; the gateway owns mapping,
  price math, and failover.
- Test: each virtual id routes to an eligible offering in fixtures;
  `/v1/models` returns exactly the six; unknown ids keep current
  passthrough behavior.

## Iter 14 — Product gaps (planned)

- Goal: close the remaining
  [product-viability gaps](review/PennyRouter-product-viability-review.md):
  quality floor per tier, strict-budget fail-closed, live free
  eligibility, provider affinity, Responses-routing coverage.
- Value: the free-first coding policy is trustworthy on paid tiers too.
- Test: over-budget request fails closed with an OpenAI-shaped error;
  ineligible providers never win on price alone.

## Iter 15 — Decision-model catalog + selection (planned)

- Goal: answer "which model judges, and how is it chosen" with a
  cataloged, reviewable setup. Today judges run only as an offline
  batch — `catalog-process --judge <file>` (recorded) or
  `--judge-backend baseten|modal` (live OpenAI-compatible chat calls
  via the shared `judge_openai` core) — writing `quality_tier`/
  `quality_score`/`judged_at` into the DB; the gateway never calls a
  judge and no endpoint serves judging. Keep that separation.
- Value: judge choice becomes config, not tribal knowledge; judging
  cost stays bounded and auditable.
- Decisions to record: the judge-model catalog (which judge model per
  backend, with base URL/model/auth source); selection policy (when
  recorded suffices vs live, sampling rate, per-endpoint scoping);
  cost caps per rebuild; judgment audit trail (model + prompt version
  stored with each judgment); no request-path judge calls, ever.
- Test: `catalog-process` with each cataloged judge reproduces
  fixture judgments; unknown backend rejected; live-judge spend per
  rebuild logged and capped.

## After Iter 15

Deferred ideas in [future.md](future.md): more backends, parallel fan-out,
judge models, classifier routing. Each becomes roadmap items only when
pulled forward deliberately.
