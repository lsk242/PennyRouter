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
  `OPENROUTER_BASE_URL` (default `https://openrouter.ai`).

## Iter 2 — Streaming + guards

- Goal: SSE streaming passthrough, timeouts, upstream error mapping,
  1M-token guard.
- Value: real clients (which stream) work; oversized requests fail fast
  without touching the backend.
- Test: chunked fixture streams through intact; oversize request rejected
  pre-backend; backend errors map to sane OpenAI-shaped errors.

## Iter 3 — Observability + perf baseline

- Goal: latency histogram, basic metrics/logs, load harness, 1 CPU / 1 GB
  Docker profile.
- Value: every later change can prove it did not regress latency or
  resource use.
- Test: load run reporting gateway overhead vs. direct-to-backend.

## Iter 4 — Catalog sync track

- Goal: a separate `catalog-sync` process (own binary, schedule, host)
  delivers a versioned catalog DB artifact to gateway hosts. Breakdown:
  [catalog-sync-roadmap.md](catalog-sync-roadmap.md).
- Value: the data foundation routing and ranking need; gathering and
  serving evolve independently — they share only the DB artifact.
- Test: fixture-produced artifact validates and loads; detail in the
  catalog-sync roadmap.

## Iter 5 — Provider routing

- Goal: model-name suffix filters plus a good default; one fast local
  lookup; sticky provider per session. See [routing.md](routing.md).
- Value: callers get cheaper/faster/more-private providers with zero
  client changes; the default needs no tags.
- Test: routing unit tests over fixture data; passthrough test with a
  forced provider; stickiness test keeps the same provider mid-session.

## Iter 6 — Ranking index

- Goal: periodic AI-assisted rebuild of provider ranking, blended with
  per-app usage trends. See [ranking-index.md](ranking-index.md).
- Value: routes track realtime fluctuations instead of static sorts;
  trends beat raw metrics where metrics lag.
- Test: rebuild from fixtures yields the expected stable ordering;
  realtime path remains a single lookup (routing tests still pass).

## Iter 7 — Price model v1

- Goal: input/output/cache price abstraction, free/promo windows, generic
  `openrouter/free` handling — no classifier. See [pricing.md](pricing.md).
- Value: cheapest-provider selection that survives promo expiry and
  cache-token asymmetry.
- Test: pricing fixtures (promo active/expired, cache-heavy/mixed) select
  the expected provider.

## Platform track (parallel to development)

Runs alongside the iterations above; see [platform.md](platform.md).
Development delivers Docker images, platform delivers deployments.

- **Now:** Docker local dev + minimal image (Dockerfile + compose added
  in 1a; verified with a local build + `/health` smoke test), plus a
  dev/prod compose split with `.env` settings (prod pulls the GHCR
  image, never builds) and an `scp` server deploy (`scripts/deploy.sh`
  + `make deploy`, gateway only on host port 8003). CI runs secret
  scanning + Dependabot.
- **Docker build + push pipeline:** workflow added in 1a; first executes
  on next push to main.
- **Later:** first cloud provider (TBD — Hetzner / AWS / Azure / …),
  Terraform-managed (VMs, resource allocation, networking); then more
  providers. Terraform automation gated on provider selection.

## After Iter 7

Deferred ideas in [future.md](future.md): more backends, parallel fan-out,
judge models, classifier routing. Each becomes roadmap items only when
pulled forward deliberately.
