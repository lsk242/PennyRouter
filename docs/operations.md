# Operations

## Run it

Local run needs a Rust toolchain (rustup, stable); the container needs
only Docker.

```sh
cargo run -p pennyrouter-gateway   # serves :3000, PORT=... to override
curl localhost:3000/health         # {"status":"ok"}
```

Settings live in `.env` (copy [`.env.example`](../.env.example)); every
variable has a sane default so the commands below also work without one.

```sh
cp -n .env.example .env                 # once; optional
docker compose up --build               # local dev: builds from source
```

Server deployment pulls the CI-built image from GitHub Packages and
never builds — one command copies the pull-only compose file plus a
generated `.env` to the server, pulls, restarts, and health-checks:

```sh
DEPLOY_HOST=user@example.com make deploy   # or ./scripts/deploy.sh
```

A dev deployment ships both services (gateway + catalog-sync) from
the same pull-only images under its own directory and host port:

```sh
COMPOSE_FILE=compose.dev.yaml DEPLOY_DIR=/opt/stacks/pennyrouter-dev \
  HOST_PORT=8003 DEPLOY_HOST=user@example.com ./scripts/deploy.sh
```

Deploy settings are env vars (see the header of
[`scripts/deploy.sh`](../scripts/deploy.sh)): `DEPLOY_HOST` (required),
`DEPLOY_DIR` (default `pennyrouter`), `HOST_PORT` (default `3000`,
published on the server), `PORT` (default `3000`, in-container),
`IMAGE` (default the CI-built `ghcr.io` image), and
`OPENROUTER_BASE_URL`. `DRY_RUN=1` prints what would run without
touching the server. Gateway only: `catalog-sync` is a separate
process on its own host and shares only the versioned DB artifact
(see [architecture.md](architecture.md)), so it is not part of this
deploy.

CI builds `ghcr.io/lsk242/pennyrouter:latest` and smoke-tests `/health`
on every PR, and pushes it on merges to main. It also rejects any
`build:` section in `compose.prod.yaml`, keeping deploys pull-only.

## Local development

Docker-based local dev (see `docker/`):

- One command to build and run the gateway locally.
- A resource-capped profile matching the ~1 CPU / 1 GB target for perf
  runs (`compose.perf.yaml`, full harness in Iter 3).
- A health-only load scaffold (`tests/load/run.sh`) for session, latency,
  and resource smoke runs before chat workloads exist.
- An overhead comparison harness (`tests/load/compare.sh`): same chat
  workload against a mock backend directly and through the capped
  gateway, producing a markdown/JSON report. Re-run it to prove later
  iterations did not regress the baseline.
- An opt-in live integration check (`tests/integration/<lang>/`, each
  with its own README) that calls OpenRouter directly and through the
  gateway, then compares. It needs `OPENROUTER_API_KEY` in that
  folder's `.env` (gitignored, never committed) and a running gateway;
  without a key it skips. Never runs in CI.
- No secrets in the repo: backend keys travel with requests, not config.

## Configuration

Keep config minimal and env-based (`.env`, see [`.env.example`](../.env.example)):
`PORT` (container port), `HOST_PORT` (published port), `IMAGE` (dev:
tag for the local build; prod: image to pull), `OPENROUTER_BASE_URL`
(upstream for chat completions, default `https://openrouter.ai/api`),
`CATALOG_DB` (catalog artifact for routing: directory, manifest, or DB
file — unset means plain passthrough). A directory also serves the
newest verified ranking artifact when one exists (see
[ranking-index.md](ranking-index.md)).
No local key store:
backend keys travel with requests, never in config or fixtures.

## Ranking

From Iter 6: `catalog-rank` rebuilds the provider ranking offline from
the catalog DB plus optional trends and recorded AI adjustments, and
publishes a versioned `ranking-<version>.json` artifact beside the DB.
The sync container runs it every cycle (`TRENDS`/`ASSIST`/`PROMOS`
inputs default to the `/input` fixtures when present). The gateway
picks the ranking up from its `CATALOG_DB` directory automatically and
keeps serving the last good one when a rebuild fails or is missing.
From Iter 7, "cheapest" means effective cost under the balanced
workload profile (see [pricing.md](pricing.md)), and `openrouter/free`
routes to any free offering.

## Guards and timeouts

From Iter 2: request bodies estimating over 1M tokens (~4 MiB at 4
bytes/token) are rejected with 413 before contacting the backend.
Upstream budgets are connect 10s, first response 30s, stream idle 60s
per chunk, and 300s total for buffered responses; timeouts answer 504.
Upstream JSON errors pass through; anything else is wrapped in an
OpenAI-shaped error keeping the upstream status. A stalled stream ends
the client stream — the 200 was already sent, so there is nothing left
to map it to.

## Observability

From Iter 3: every request logs one JSON line to stdout with request ID
(incoming `x-request-id` or generated, also echoed back), method, route,
status, latency, and outcome. `GET /metrics` serves Prometheus counters
(requests by route/status, upstream errors by kind, guard rejections,
in-flight gauge) plus a latency histogram. Provider, model, and snapshot
fields join the log line once routing lands. Never log bodies, prompts,
completions, or Authorization headers. From Iter 8, fallback-proxied
calls emit an extra `{"fallback_proxy":true,...}` line with method and
full path (see [api.md](api.md)). Later, readiness/snapshot health
may complement `/health` without changing its liveness contract.
