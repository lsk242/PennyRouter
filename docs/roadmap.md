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
  server compose file with `.env.dev` + `.env` settings (pulls the
  GHCR images, never builds; no published ports — served behind the
  reverse proxy) and an `scp` server deploy (`scripts/deploy.sh`
  + `make deploy`). A git-stack variant deploys the same pull-only
  file as a git stack (push-to-deploy;
  `scripts/deploy-webhook.sh` triggers a redeploy via a
  configured API endpoint). CI runs secret scanning + Dependabot.
- **Docker build + push pipeline:** workflow added in 1a; proven green
  on main (build + push + `/health` smoke) across the v0.1.0–v0.6.0
  merges.
- **First cloud provider (done):** Hetzner, selected from the
  six-provider comparison. Terraform-managed (`terraform/hetzner`):
  CCX13 VM, primary IPv4, private network, firewall, Caddy TLS +
  pull-only gateway via cloud-init; CI validates fmt + config.
- **Later:** more providers on the same patterns; Terraform
  plan/apply automation per environment.

## Iter 9 — Resilience bounds (done)

- Goal: close the architecture deviation — bounded upstream concurrency
  and queue depth with explicit load-shedding, plus graceful shutdown.
  See [architecture.md](architecture.md) and
  [performance.md](performance.md).
- Value: the gateway degrades with 503 instead of collapsing under
  a burst; deploys stop dropping in-flight requests.
- Test: soak past the concurrency bound sheds with 503 (no error
  cascade); SIGTERM drains in-flight requests before exit.
- Landed: `ConcurrencyLimits` semaphores on the forward path
  (`UPSTREAM_MAX_CONCURRENT`/`UPSTREAM_MAX_QUEUED`, defaults 64/128);
  fast-path slot, bounded wait, OpenAI-shaped 503 + `Retry-After` +
  `pennyrouter_load_shed_total` beyond; graceful shutdown on
  Ctrl-C/SIGTERM in `main.rs`. `cargo test` (81 passed), clippy clean,
  fmt clean; new `tests/resilience.rs` (shed + queued).

## Iter 10 — Catalog freshness (done)

- Goal: close the freshness deviation — periodic `CATALOG_DB` reload
  with atomic snapshot publication and last-good fallback, so ranking
  and price updates reach a running gateway without restart.
- Value: re-ranking and promo windows actually take effect; resolves
  the routing-vs-operations contradiction (startup-once vs automatic
  pickup).
- Test: drop a new artifact → gateway serves it without restart;
  corrupt artifact → last-good snapshot keeps serving.
- Landed: `spawn_reload_task` in the gateway runtime (`main.rs`,
  `RELOAD_INTERVAL_SECS` default 60s, 0 disables); `reload()` reuses
  the existing atomic swap + last-good path; attempts counted in
  `pennyrouter_catalog_reload_total{result="ok|error"}`. `cargo test`
  (84 passed), clippy clean, fmt clean; new `tests/reload.rs`
  (new-model pickup, corrupt-DB last-good, background publish).
  Note: `CATALOG_DB` in Terraform left for the platform
  track — the gateway side honors it whenever set, and prod compose
  now mounts `CATALOG_DB=/catalog` (Q14-A).

## Iter 11 — Bug-review fixes (done)

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
- Landed: F01 caller `provider` fields preserved + `only`/`ignore`
  intersection with pin invalidation (unsatisfiable passes through
  for upstream enforcement); F02 admission-before-collection + 30s
  upload-idle / 60s upload-total deadlines (408); F03 end-to-end
  response headers preserved (transparent, never decoded), truthful
  representation headers on rewritten bodies, hop-by-hop/proxy
  request headers stripped, redirects pass through unfollowed; F04
  precision class forwarded as `quantizations` (best-effort,
  caller wins; tier targeting documented as limitation); F05 16 MiB
  upstream body bound on success and error paths (502
  `upstream_too_large`); F06 terminal SSE `error` event on
  stall/break (clean EOF unchanged); F07 atomic data-before-manifest
  publish + newest-*verifying*-manifest discovery with invalid
  candidates skipped; F08 verified already correct (`/api` default
  everywhere); F09 readiness loops exit nonzero after exhaustion with
  per-probe timeouts (verified by stub); F10 descending scores keep
  unknowns last; F11 `free_offers` with `valid_until_unix` published
  in the ranking document and wall-clock-checked by the gateway.
  A01 chat keeps its query string; A02 negative/non-finite prices
  unpriceable; A03 real tag in offline rank signals (matches inline);
  A04 stream-lifetime in-flight slot; A05 wrong-type tags rejected at
  load. Hardening: query values redacted from fallback logs, session
  keys are seeded hashes with 30-min TTL eviction, `Retry-After` on
  shed. `cargo test` (105 passed), clippy clean, fmt clean; new
  `tests/hardening.rs`, `tests/reload.rs` F07 case, F01/F04/F10/F11
  unit + rank/process/price/judge tests.

## Iter 12 — Hot-path hygiene + fresh baseline (done)

- Goal: act on the
  [resource-efficiency review](review/PennyRouter-Rust-resource-efficiency-review.md) —
  buffered structured logging (no `println!` per request), less
  per-request lock contention; re-run `tests/load` and record
  post-Iter-11 numbers (Iter 3 baseline was stale).
- Value: the "dumb and fast" path stays lean; every later change has a
  current baseline to prove against.
- Test: load compare vs the Iter 3 baseline recorded here.
- Landed: single background log-writer thread over a bounded
  1024-line channel (full drops counted in
  `pennyrouter_log_dropped_total`, flush ≥1/s); session table
  `Mutex` → `RwLock` (concurrent lookups no longer serialize;
  expiry reap preserved). Parse + re-serialize kept only on the
  routed path — unrouted requests never parse (already true since
  Iter 5); full raw-envelope rewrite deferred as disproportionate.
- Baseline re-run (`tests/load/compare.sh`, 20 sessions × 15s, 20ms
  mock, 1 CPU / 1 GB profile, image built from this tree):
  `tests/load/results/compare-20261029T212247Z`. json p50 overhead
  +0.26ms (identical to Iter 3), p99 +13.10ms (vs +0.40ms — single
  run, variance unconfirmed, needs a confirmation run); sse p50
  +0.22ms, p99 +4.15ms; 0 errors on all sides; no sheds, no guard
  rejections. p50 matches Iter 3 exactly, so the typical path did not
  regress; the p99 tail is open investigation, not a verdict.
- Confirmation N=5 (2026-11-04, Q17, same params, current tree):
  `tests/load/results/compare-20261104T14*` r1–r5. json overhead
  median p50 +0.28ms / p99 +0.43ms; sse median p50 +0.20ms / p99
  +0.11ms; 0 errors in all 10 phases; gateway peak RSS 15.0–15.5 MiB
  under 20-session load (far inside the 1 GB profile). The +13.10ms
  spike was variance — bug-todo P0 closed, no latency iteration.

## Iter 13 — Virtual model catalog, free-first (done)

- Goal: serve the curated virtual-model menu and nothing else (six
  at the time; fourteen curated ids + pinned-model/fallback forms
  today). Ids carry the provider first so future providers slot in
  (today only OpenRouter, BYOK key forwarded as now):
  `openrouter/pair/free`, `openrouter/pair/low`,
  `openrouter/pair/think`, `openrouter/pair/privacy/free`,
  `openrouter/pair/privacy/low`, `openrouter/pair/privacy/think`.
- Tiers: `free` = zero-cost offering; `low` = cheapest basic
  (cheapest effective cost across models); `think` = best quality
  signals across models. No speed/latency axis in v1.
- Price metric: `effective_cost` blended over input/output/cache-read/
  cache-write under the documented workload profile; FP resolution is
  a separate comparison class, never silently substituted; eligibility
  (restrictions, priceable, known class) filters before price compares.
- Availability: one retry on another provider for virtual requests on
  retryable outcomes (429/502/503/504); wire `allow_fallbacks` stays
  false (gateway-owned failover, no upstream surprise). For `free`
  (and all virtual picks), organic 429 history steers around
  throttled providers (60s cool-down) with availability fallback.
- `GET /v1/models` answers locally with exactly the curated entries
  (six at the time; no upstream proxy leak); chat with a virtual id
  routes to a real backend offering. See [api.md](api.md),
  [routing.md](routing.md), [pricing.md](pricing.md).
- Value: clients see a stable curated menu (six at the time); the
  gateway owns mapping, price math, and failover.
- Test: each virtual id routes to an eligible offering in fixtures;
  `/v1/models` returns exactly the curated menu (six at the time);
  unknown ids keep current passthrough behavior.
- Landed: `parse_virtual` strict parsing before catalog split;
  `decide_virtual` (`free` shares legacy path per-id namespace,
  `low`/`think` cross-model with disjoint tiers + known-class guard);
  `privacy/` injects `zdr`/`data_collection` unless caller-set;
  `note_throttled` + `skip_throttled` (60s, all-throttled still
  serves); `route_request_retry` (failed provider joins `ignore`,
  single retry, virtual only); curated local `GET /v1/models`
  (fallback test moved to embeddings query coverage);
  `pennyrouter_upstream_throttled_total`. `cargo test`, clippy, fmt
  clean; new `tests/virtual_models.rs` (menu, tiers, privacy wire,
  unknown passthrough, retry, throttle loop). Deferred: model-info
  rate limits (no live feed exists — organic 429 history only);
  `think` reasoning-capability signal (quality orientation is the
  proxy); true provider-side privacy filtering (needs index signals,
  Iter 14/15).

## Iter 14 — Product gaps (done)

- Goal: close the remaining
  [product-viability gaps](review/PennyRouter-product-viability-review.md):
  quality floor per tier, strict-budget fail-closed, live free
  eligibility, provider affinity, Responses-routing coverage.
- Value: the free-first coding policy is trustworthy on paid tiers too.
- Test: over-budget request fails closed with an OpenAI-shaped error;
  ineligible providers never win on price alone.
- Shipped: per-tier quality floors (`low` needs a judged tier,
  `think` needs `high`) rejecting with `quality_floor_unmet` (503);
  `provider.max_price` hard caps on raw price columns failing closed
  with `budget_exceeded` (400), overriding affinity; sticky pins
  re-checked against live restrictions every request; `POST
  /v1/responses` on the same route-and-forward path as chat;
  free-tier 429 failover proving live eligibility.
  `cargo test --workspace`, clippy, fmt clean.

## Iter 15 — Decision-model catalog + selection (done)

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
- Shipped: judges file (`judges` + `policy`) loaded with strict
  validation; recorded default, live only on explicit selection with
  deterministic sampling and pre-write cap checks; per-row
  (`judge_model`, `judge_prompt`) + rebuild (`judge_backend`,
  `judge_calls`, `judge_spend_usd`) audit trail with presence-based
  migration for old DBs; `--sample-rate`/`--max-calls`/`--max-spend-usd`
  overrides; one spend line per live rebuild. Proven by
  `tests/judge_catalog.rs` (9 tests incl. fixture repro per backend,
  unknown-backend rejection, fail-closed caps, audit stamping) plus a
  live-binary smoke (recorded run, unknown-backend error, cap-breach
  error, audit meta read back).
  `cargo test --workspace`, clippy, fmt clean.

## Iter 16 — Provider alternatives to OpenRouter (done)

- Goal: serve Modal and Baseten as first-class upstreams alongside
  OpenRouter, on the caller's own keys.
- Value: one gateway, one caller token, three upstreams — no
  gateway-held serving keys, no billing surface.
- Shipped: provider-pinned direct routes (`modal/<model>`,
  `baseten/<model>`) on both routed endpoints; catalog bypass (no
  router needed); prefix stripped, OpenRouter `provider` object
  dropped; caller headers incl. `Authorization` forwarded unchanged;
  unset provider fails closed with `provider_not_configured` (400);
  exactly-once, `x-pennyrouter-provider` echoed. `MODAL_BASE_URL`
  (unset by default) / `BASETEN_BASE_URL` (defaults to Baseten's
  public inference API). Proven by `tests/providers.rs` (5 tests:
  per-upstream routing, auth forwarding, field stripping, default
  passthrough intact, fail-closed unset, empty-remainder boundary).
  `cargo test --workspace`, clippy, fmt clean.

## Iter 17 — Debug UIs, off by default (done)

- Goal: operator visibility into both systems without touching the
  request path: gateway ranking state, catalog runs and artifacts.
- Value: tiers and judgments become inspectable in a browser instead
  of sqlite; UIs cost nothing when off.
- Shipped: gateway `GET /ui` (snapshot, virtual menu, per-model
  offerings + default ranking order, free offers, metrics), gated by
  `GATEWAY_UI=1` with no route registered otherwise; `catalog-ui`
  binary (runs, models, endpoints, ranking, audit meta, log tail)
  gated by `CATALOG_UI=1`; localhost bind; catalog Docker image
  ships the binary. Fixed `/` landing pages in both systems show
  binary version + build hash (`build.rs`, `unknown` without git),
  config status, and links. Proven by `tests/ui.rs` (gateway, 4 tests:
  disabled falls through, enabled content, router-less, path
  intact) and `catalog-ui/tests/ui.rs` (refusal + full page tour
  against a published fixture dir).
  `cargo test --workspace`, clippy, fmt clean.

## Iter 18 — Live OpenRouter fetch (done)

- Goal: the catalog reflects the real provider landscape instead of
  three fixture models — gather from OpenRouter's public endpoints.
- Value: routing (including the free tier) decides over live data;
  fixture input stays the offline default for tests and local work.
- Shipped: `catalog-sync --fetch-openrouter` pulls `GET /v1/models`
  plus one `GET /v1/models/{id}/endpoints` per model (public, no
  auth; base from `OPENROUTER_BASE_URL`), feeding the unchanged
  `run_fixture_sync` boundary; a failed models fetch aborts with no
  publish (last good artifact keeps serving), failed per-model
  endpoints skip with a count. `FETCH_OPENROUTER=1` flips the sync
  container and compose services off fixtures (default 0). Proven by
  `crates/catalog/tests/fetch.rs` (2 tests: full pull, skip on 404,
  raw ids in paths) plus a live one-shot: 456 models, 1483
  endpoints, 18 zero-price offerings, processed and published.
  `cargo test -p pennyrouter-catalog`, clippy, fmt clean; workspace
  check clean.

## Iter 19 — `lowest` tag (done)

- Goal: a model suffix filter for the cheapest paid offering
  catalog-wide, with no quality floor — the fallback agents use
  when free turns run out.
- Value: one tag covers the "just make it cheap" case without
  curating per-model suffixes; stricter callers keep `low`.
- Shipped: `lowest` orientation (`Orientation::Lowest`) sharing the
  `low` tier's paid-only filter, budget, throttle skip, stickiness
  (full tagged-id namespace), and deterministic tie-breaks
  (cost, model id, provider), minus the judged-tier floor.
  Fail-closed (`no_eligible_offering` 503, `budget_exceeded` 400)
  on the request path; exactly-once (no tag retry); unknown base
  models pass through; first orientation wins. Proven by 7 routing
  unit tests. `cargo test`, clippy, fmt clean.

## Iter 20 — Decode gzipped upstream errors (done)

- Goal: readable error messages when upstreams gzip their 4xx/5xx
  bodies (found live: a gzipped 404 reached an agent as raw bytes).
- Value: agents see actionable errors and classify retries
  correctly; success-path bytes stay untouched.
- Shipped: `decode_error_body` (gzip-magic detect, 1 MiB bounded
  decode, raw fallback on corrupt input) on the error path only;
  decoded JSON errors forward without the encoding claim (F03
  honesty). New `flate2` dependency (pure Rust, no C deps).
  Proven by 2 passthrough tests. `cargo test`, clippy, fmt clean.

## Iter 21 — Free fallback + header-driven 429s (done)

- Goal: survive free-tier exhaustion (50/day walls hit live) without
  client changes: `openrouter/pair/free/fallback/<tier>` serves free,
  then exactly one tier pick; 429s carry real reset info.
- Value: agents ride free until it's gone, then paid, with honest
  signals throughout — no fixed timers, no invented quota state.
- Shipped: header-derived cool-down (`Retry-After` → reset epoch-ms
  → 60s default, 24h clamp); preserved `X-RateLimit-*` on 429s;
  4-segment fallback ids through shared tier dispatch (`low`,
  `think`, `lowest`; unknown fails closed); post-retry fallback leg
  skipping the burned provider; no fallback on auth/config errors.
  Multi-account quota pooling stays a documented non-goal (ToS).
  Proven by unit + integration tests. `cargo test`, clippy, fmt clean.

## Iter 22 — Tool-aware routing (done)

- Goal: end live-found "No endpoints found that support tool use"
  404s — tool-bearing agent requests pinned providers whose endpoint
  lacks tool support.
- Value: agents keep `tools`/`patch` on; the gateway steers to
  capable endpoints or fails closed with the fix, never an upstream
  404.
- Shipped: per-endpoint `supports_tools` index (schema v4, auto-
  migrate); `needs_tools` detection in `ProviderConstraints`; staged
  tool filter on `free`/`low`/`think`/`lowest`/direct paths;
  `tools_unsupported` 400; fallback still fires past starvation.
  Proven by extraction + unit + 2 integration tests.
  `cargo test`, clippy, fmt clean.

## Iter 23 — Standby router recovery (done)

- Goal: a gateway booted against a missing, mid-write, or
  schema-older catalog recovers without a restart (live-found: the
  reload task never spawned when the initial load failed, so routing
  stayed dark until a manual restart).
- Value: deploys that land before catalog-sync finishes heal
  automatically on the next reload tick.
- Shipped: `Router::standby` (empty snapshot, source set) attached
  by `from_env` on load failure; `is_ready()` drives the landing
  page; reload task always spawns when `CATALOG_DB` is set.
  Proven by a standby-heals integration test.
  `cargo test`, clippy, fmt clean.

## Iter 24 — Decision tier (done)

- Goal: serve behavior-scoring ("decision") models through
  `openrouter/pair/decision/free` with fallback — the models list
  omits them entirely, so the tier needs its own signal and sync
  path.
- Value: judge/scoring workloads ride a routed pool instead of
  hardcoding one provider; exhaustion falls through to a paid tier.
- Shipped: per-endpoint `supports_decisions` index from the parent
  model payload (schema v5, auto-migrate); `EXTRA_MODEL_IDS` fetch
  seam for unlisted ids (FK-safe: model + endpoints kept as a pair);
  `decision` tier (cheapest-capable, no floor, no class gate) with
  `/free` scope + `/fallback/<tier>`; `decision` fallback target.
  Proven by extraction + fetch + parse + 3 integration tests.
  `cargo test`, clippy, fmt clean.

## Iter 25 — OpenRouter-native interface (done)

- Goal: OpenRouter-native SDKs work through the gateway with an
  explicit `baseURL`, no DNS spoofing. See
  [proposals/2026-11-01-openrouter-interface.md](proposals/2026-11-01-openrouter-interface.md).
- Value: native clients keep their paths (`/api/v1/…`); account and
  catalog reads stop 404ing on a doubled `/api`; money/key writes
  stay denied by default.
- Shipped: `POST /api/v1/chat/completions` and
  `POST /api/v1/responses` alias to the routed handler (upstream path
  rewritten to `/v1/…`, same metrics slots); native fallback paths
  compose against the upstream root (configured base minus `/api`,
  so mocks and Ollama bases keep working); non-GET key, coinbase,
  and workspace-budget paths answer 403 `management_proxy_disabled`
  unless `ALLOW_MANAGEMENT_PROXY=1`. `GET /api/v1/models` proxies the
  full upstream catalog (the curated fourteen-id menu stays on
  `GET /v1/models`).
- Test: `crates/gateway/tests/openrouter_native.rs` (alias rewrite,
  root composition with and without the `/api` suffix, upstream
  catalog, deny-by-default + opt-in, reads unaffected) plus a
  `Route::from_path` unit test. `cargo test`, clippy, fmt clean.

## Iter 26 — Smart classifier (done)

- Goal: serve `openrouter/pair/smart[/max]` (plus `privacy/`
  variants): one decision-model leg picks the cheapest tier that can
  answer well (`free`/`lowest`/`low`, plus `think` on `smart/max`),
  then the normal tier dispatch serves it — the one explicit,
  scoped exception to the no-network routing contract.
- Value: agents stop guessing between free-first and paid quality;
  the gateway picks per request, with honest fallback + headers.
- Shipped: `smart`/`smart/max` parse + 14-id menu, `classifier.rs`
  (prompt, input extraction/truncation, strict verdict validation),
  async classifier leg (own timeout, no self-retry, BYOK preserved,
  privacy fields on both legs), fail-closed/fallback contract
  (`classifier_not_configured` 503, `classifier_auth_error` 502,
  fallback tier marked, rejected tiers never re-picked), no session
  pinning on the smart path, classifier metrics + `/ui` panel.
  Proven by `crates/gateway/tests/smart_classifier.rs` (16
  integration tests over mock classifier + mock upstream) plus
  classifier/verdict and routing unit tests.
  `cargo test`, clippy, fmt clean.

## Iter 27 — Decisions route (done)

- Goal: serve decision models for real — the tier routes, but span
  400s on chat completions; it needs the alpha decisions endpoint.
- Value: `POST /v1/decisions` answers scoring workloads through the
  routed pool with chat-grade resilience.
- Shipped: decisions handler (model-only rewrite, alpha forward,
  retry + `decision`-only fallback, provider header, 429 memory);
  `decisions` observability slot; 5 integration tests.
  `cargo test`, clippy, fmt clean.

## Iter 28 — Pinned-model routes (done)

- Goal: let a caller pin to one specific model (e.g.
  `deepseek/deepseek-v4.1-flash`) and have the gateway pick the best
  provider for it — not cross-model, just best provider within that
  model — with fallback when needed.
- Value: agents that need a particular capability don't hardcode
  one provider; the router keeps one virtual id, many providers.
- Shipped: `openrouter/pair/model/<model-id>` (and
  `privacy/model/<id>`) with `.../fallback/<tier>`; the pinned leg
  serves first, the named cross-model tier leg on exhaustion, with
  header cool-down + budget/tool checks scoped to the pinned model.
  Proven by parsing + 1 integration test file (route, fallback,
  tool/budget, unknown-model, privacy wiring).
  `cargo test --workspace`, clippy, fmt clean.

## Iter 29 — Routing inspector (done)

- Goal: show in realtime what each model combination returns for one
  key — fixed common combos top-3 plus an open model box, with the
  stored columns and the sticky-vs-fresh answer.
- Value: operators stop guessing which provider a key gets and why;
  session stickiness becomes visible instead of tribal knowledge.
- Shipped: read-only `Router::preview` (top-N ordered candidates,
  sticky pick with this key vs fresh pick without session, session
  namespace + hash, all stored columns plus effective cost, free
  eligibility, throttle availability, rank index; smart ids preview
  as an explanatory rejection; fallback legs preview the serving
  leg); `GET /ui/preview?model=<id>&top=N` JSON behind `GATEWAY_UI=1`
  (key as `Authorization`, never logged/stored, never pins; missing
  model 400, no router 503, disabled falls through); `/ui` inspector
  (key box with hash, ten fixed non-smart virtual ids top-3
  server-rendered anonymous plus keyed refresh, custom-model box
  with sticky/fresh panel and full column tables).
- Test: `crates/gateway/tests/ui.rs` preview suite (low top-1 matches
  decide, think/low/free/decision tiers, sticky-vs-fresh across a
  snapshot swap matching decide, preview-never-pins via hash,
  throttled pin falls through, endpoint JSON columns, 400/503,
  disabled falls through, page content). `cargo test`, clippy, fmt
  clean.

## Iter 30 — OpenRouter catalog judge (done)

- Goal: judge catalog endpoints through an OpenRouter-hosted model,
  like the Baseten/Modal legs — operator-picked model, shared key.
- Value: judging no longer depends on Baseten/Modal accounts; any
  OpenRouter chat model can classify offerings into quality tiers,
  still offline batch with sampling, caps, and audit trail.
- Shipped: `OpenRouterJudge` on the shared OpenAI-chat core
  (`https://openrouter.ai/api/v1`, Bearer auth, same prompt v1 +
  reply validation); model from `OPENROUTER_JUDGE_MODEL`
  (fail-closed, nothing pinned), key from the shared
  `RANKINGS_API_KEY`; `--judge-backend openrouter` in
  catalog-process plus an `openrouter` judges-catalog entry
  (`auth_env` names the shared key).
- Test: `crates/catalog/tests/judge_openrouter.rs` (stub round trip:
  POST path, Bearer header, body model/temperature/json_object/2
  messages; invalid outputs error naming the backend; from_env
  fail-closed matrix; base URL const) + catalog tests (fixture
  entry resolves, stub-backed verdict, `build_judge` from entry) +
  CLI zero-sample dispatch. `cargo test`, clippy, fmt clean.

## Iter 31 — Artificial Analysis index ingestion (done)

- Goal: make OpenRouter's per-model Artificial Analysis indexes
  queryable (per the [ingestion proposal](proposals/2026-11-03-artificial-analysis-index.md)),
  as the scope signal for on-demand judging.
- Value: judging (and later filters) can target high-quality models
  instead of the whole catalog; the gateway shows coding strength
  per model on `/ui`.
- Shipped: schema v5→v6 (`aa_intelligence/coding/agentic_index` on
  `models`, idempotent migrate); `run_processing` distills them
  from `benchmarks.artificial_analysis.*` (live ints and floats
  rounded, garbage → `NULL`); gateway snapshot carries the coding
  map, display-only (`· coding N` model headers, no routing
  change). Deviations from the draft: `ModelRow` untouched
  (`ModelPayload` + `aa_coding_map()` instead); first consumer is
  judging scope, minindex stays proposed.
- Deploy note: a v6 gateway rejects v5 artifacts — the first v6
  publish must land before (or with) the gateway image. Reloads
  keep last-good across the window; a boot on v5 stays in standby
  passthrough until the first v6 publish.
- Test: extract vectors (incl. float rounding + out-of-range),
  v1→v6 migrate incl. model cols, processing writes, snapshot
  carry, `/ui` header. Live one-shot: 469 models, coding 168,
  intelligence 89, agentic 136. `cargo test` clean locally;
  clippy/fmt via CI (sandbox lacks the toolchain).

## Iter 32 — Scoring rename + offline tiers (done)

- Goal: rename judging → scoring end to end, and make the default
  path an offline (no-AI) tier derivation so the catalog always has
  tiers without LLM spend.
- Value: every sync tiers AA-indexed models free (coding 70+
  `high`, 40+ `standard`, else `low`); on-demand online verdicts
  target only what operators pick, and always win over offline
  tiers.
- Shipped: `Scorer`/`Score`/`ScorerIdentity`, `score_*` modules,
  `--score`/`--score-backend`/`--scorers` CLI, `scorers.json` +
  `scores*.json` fixtures, `OPENROUTER_SCORE_MODEL` /
  `MODAL_SCORE_URL` env, schema v6→v7 (`scored_at`/`score_model`/
  `score_prompt` rename with verdict carry-over, legacy meta
  dropped); `OfflineScorer` (`offline` / `aa-coding-v1`) runs in
  `run_processing` on untiered rows only; sync + process CLIs
  report the offline count. Live LLM prompts stay byte-identical
  (v1 frozen).
- Deploy note: a v7 gateway rejects v6 artifacts — the first v7
  publish must land before (or with) the gateway image. Reloads
  keep last-good across the window; a boot on v6 stays in standby
  passthrough until the first v7 publish. Operators must rename
  `OPENROUTER_JUDGE_MODEL` → `OPENROUTER_SCORE_MODEL` and
  `MODAL_JUDGE_URL` → `MODAL_SCORE_URL` in `.env` before online
  scoring runs.
- Test: cutoff/clamp/identity units, processing tiers + meta,
  online-wins precedence, v6→v7 migrate (values, dropped
  columns/keys), CLI offline line. `cargo test --workspace`
  clean locally (39 suites); clippy/fmt via CI (sandbox lacks
  the toolchain).
  
## Iter 33 — `minindex` suffix filter (done)

- Goal: let callers ask for a minimum Artificial Analysis coding
  index on the model path — `openrouter/pair/low/minindex/50` — so
  "cheapest model above quality X" is one string edit.
- Value: the AA index ingested in Iter 31 becomes a routing filter
  without a new header, query param, or API surface; it composes
  with `privacy/`, `model/<id>`, `smart`, and `/fallback/`.
- Shipped: `VirtualModel`/`SmartId`/`Decision` carry
  `minindex: Option<u8>`; `parse_virtual` splits the suffix before
  `/fallback/` and rejects unusable values (`invalid_model` 400,
  never passthrough); the floor is the first eligibility gate in
  `decide_free`, `decide_ordered_virtual`, `decide_decision`,
  `decide_pinned`, and the preview lists (null/absent indexes are
  excluded; `minindex/0` means "must have any index"); fallback
  legs run unfiltered; routed responses echo
  `x-pennyrouter-minindex`; `pennyrouter_minindex_requests_total`
  counts by floor/tier/result.
- Test: parse matrix (valid/invalid/duplicate/leading zeros),
  cross-model filter, pinned gate, fallback-leg exemption, helper
  units, plus `tests/minindex.rs` (route + header, 400, 503 with
  `Retry-After: 1`, metrics). `cargo test --workspace` clean
  (40 suites); clippy/fmt clean.

## After Iter 33

Deferred ideas in [future.md](future.md): exact price prediction,
parallel fan-out, further score backends. Each becomes roadmap items
only when pulled forward deliberately.

## Iter 34 — Catalog value score + catalog UI (done)

- Goal: turn the catalog's stored facts into one deterministic
  verdict — maximum intelligence for the lowest price — and make the
  catalog UI answer "what is missing?" and "why is this ranked
  here?".
- Value: routing tiers stop being the only quality story; operators
  see coverage gaps (264/473 models have no AA index) and the exact
  arithmetic behind every ordering, with no AI or external ranking
  service in the loop.
- Shape: `rank_score = quality × efficiency × popularity` (0..100),
  quality = AA index (coding first) × precision factor, efficiency =
  `COST_HALF_CREDIT/(cost_per_task+COST_HALF_CREDIT)` anchored at
  the live median ($0.01/task), popularity a ≤10% factor; speed
  then availability break ties. Schema v7→v8 stores the components; the ranking
  document gains an optional `score` ordering the gateway ignores
  until a later iteration. UI: overview (counts, warnings, formula,
  worked example) + models tabs (`free`/`tiers`/`decision`) with a
  per-row breakdown.
- Status: implemented. Decided 2026-11-12: coding-first basis
  (170/473 models vs 92 for intelligence), `COST_HALF_CREDIT` =
  $0.01/task, free tab ranked on quality × popularity. As built:
  popularity is fetched before processing (the score reads the full
  window, not just the top-10 menu pins); `aa_cost_per_task` is
  stored but unused until AA ships it; the ranking document's
  `score` ordering is omitted when the catalog was never processed.
  Proposal: `docs/proposals/2026-11-12-catalog-ui-value-score.md`.

## After Iter 34
