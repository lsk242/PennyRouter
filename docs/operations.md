# Operations

## Run it

Local run needs a Rust toolchain (rustup, stable); the container needs
only Docker.

```sh
cargo run -p pennyrouter-gateway   # serves :8080, PORT=... to override
curl localhost:8080/health         # {"status":"ok"}
```

Settings come in three files: [`.env.example`](../.env.example)
(documents every variable), the committed `.env.dev` (working dev
values, generic only), and the gitignored `.env` overlay (secrets +
custom URLs only). Every variable has a sane default so the commands
below also work without any of them.

```sh
make dev   # local dev: builds from source (loads .env.dev + .env)
```

Server deployment pulls the CI-built images from GitHub Packages
and never builds — one command copies the pull-only compose file
plus a merged `.env` (`.env.dev` base overlaid with `.env`
secrets) to the server, pulls, restarts, and health-checks.
`compose.dev.yaml` loads generic config from the repo's `.env.dev`
via `env_file:` and takes secrets from `environment:`
interpolation, so the scp deploy also ships the base file as
`.env.dev` whenever the compose file references it:

```sh
DEPLOY_HOST=user@example.com make deploy   # or ./scripts/deploy.sh
```

The server stack publishes no ports: gateway and catalog-ui join
the front-proxy network (`PROXY_NETWORK`, default
`nginx-proxy_default`, which must already exist on the host) and
expose 8080 there, so create Nginx Proxy Manager hosts for
`gateway:8080` and `catalog-ui:8080` (stable container names).
Container port is 8080 everywhere — `PORT` in `.env.dev` must stay
8080. The post-deploy health check runs from a one-off curl
container on that same network, since there is no host port to
probe.

Deploy targets are env vars (see the header of
[`scripts/deploy.sh`](../scripts/deploy.sh)), or the gitignored
`.env.deploy` file (see [`.env.deploy.sample`](../.env.deploy.sample)),
which both scripts load automatically when present — explicit
environment still wins over file values: `DEPLOY_HOST`
(default `apps.lan`), `DEPLOY_DIR` (default
`/opt/stacks/pennyrouter`), `COMPOSE_FILE` (default
`compose.dev.yaml`); app config comes from the merged shipped
`.env` (base `DEPLOY_ENV_FILE`, default `.env.dev`, plus secrets
overlay `DEPLOY_SECRETS_FILE`, default `.env`).
`DRY_RUN=1` prints what would run (env redacted) without touching
the server. The dev compose file runs `catalog-sync` beside the
gateway on the shared volume; it is still a separate process that
shares only the versioned DB artifact (see
[architecture.md](architecture.md)). Both compose files load generic
config from `.env.dev` via `env_file:` and take secrets + custom
URLs from `environment:` interpolation, with code defaults behind
every variable. An empty catalog volume means standby passthrough
until the first artifact lands.

On hosts with a git-stack manager the same pull-only file deploys
as a git stack instead: create a stack from this repo (branch
`main`, compose path `compose.dev.yaml`, env file `.env.dev`),
paste the secret/custom-URL values from the local `.env` into the
stack environment, and enable the stack webhook on the repo —
then `git push` deploys. `compose.dev.yaml` reads that same file via
`env_file:` for generic config and interpolates its small
`environment:` block (secrets + custom URLs) from the stack
environment, so nothing needs duplicating into the compose file.
[`scripts/deploy-webhook.sh`](../scripts/deploy-webhook.sh)
(`make deploy-webhook`) POSTs a JSON payload to a configured
endpoint to force a redeploy without pushing, e.g.
`DEPLOY_URL=http://<host>:3000/api/git/stacks/<id>/deploy-stream`
with `DEPLOY_PAYLOAD='{"pull":true,"forceRecreate":true}'`
(`DEPLOY_API_TOKEN` adds a bearer token when set, omitted when
empty). Either deploy method per host, never both — both stacks
use the same container names.

CI builds `ghcr.io/lsk242/pennyrouter:latest` and smoke-tests `/health`
on every PR, and pushes it on merges to main. It also rejects any
`build:` section in `compose.dev.yaml`, keeping deploys pull-only.

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

Keep config minimal and env-based (`.env.dev` + `.env` overlay,
see [`.env.example`](../.env.example)):
`PORT` (container port, 8080 everywhere on the server stack),
`HOST_PORT` (local-dev published port; the server stack publishes
nothing), `IMAGE` (local: tag for the build; server: image to
pull), `OPENROUTER_BASE_URL`
(upstream for chat completions, default `https://openrouter.ai/api`),
`MODAL_BASE_URL` (upstream for `modal/…` routes, unset by default),
`BASETEN_BASE_URL` (upstream for `baseten/…` routes, default
`https://inference.baseten.co`, no `/v1` prefix — the gateway appends
the request path itself and strips a trailing `/v1`), `GATEWAY_UI` (set to `1` to
register the read-only `GET /ui` debug page on the gateway, unset
means the route does not exist),
`CATALOG_DB` (catalog artifact for routing: directory, manifest, or DB
file — unset means plain passthrough). A directory also serves the
newest verified ranking artifact when one exists (see
[ranking-index.md](ranking-index.md)).
`UPSTREAM_MAX_CONCURRENT` (default 64: max simultaneous upstream calls)
and `UPSTREAM_MAX_QUEUED` (default 128: max requests waiting for a
slot); beyond both, the gateway sheds with OpenAI-shaped 503 +
`Retry-After`, counted in `pennyrouter_load_shed_total`.
`ALLOW_MANAGEMENT_PROXY` (unset default: management writes through the
fallback proxy — key creation, credit purchases, workspace budget
changes — answer 403 `management_proxy_disabled` without touching
upstream; set to `1` to proxy them. Reads are unaffected).
`RELOAD_INTERVAL_SECS` (catalog reload cadence, default 60; 0 disables
the background reload).
Smart classifier leg (Iter 26): `CLASSIFIER_MODEL` (unset disables the
smart ids, which fail closed; every other id keeps pure-local
routing), `CLASSIFIER_BASE_URL` (default: the OpenRouter base),
`CLASSIFIER_TIMEOUT_MS`, `CLASSIFIER_FALLBACK_TIER`
(`low`/`lowest`), `CLASSIFIER_MAX_TOKENS`, `CLASSIFIER_MAX_INPUT_CHARS`.
No stored key, ever (Q08): the leg uses the caller's `Authorization`
header, optionally overridden per-request by
`x-pennyrouter-classifier-key`.
Catalog pipeline: `CATALOG_IMAGE` (sync image tag), `SYNC_INTERVAL`
(seconds between sync cycles, 0 = run once), `FETCH_OPENROUTER`
(1 = live fetch, 0 = baked fixtures), `EXTRA_MODEL_IDS`
(comma-separated ids fetched individually), `RANKINGS_API_KEY` (menu
popularity pins via the Data API, plus the OpenRouter scoring leg;
unset = published without pins), `OPENROUTER_SCORE_MODEL` (scorer
model for `--score-backend openrouter`; unset = that backend fails
closed), score keys (`BASETEN_API_KEY`,
`MODAL_KEY_ID`/`MODAL_KEY_SECRET`, `MODAL_SCORE_URL`; unset = online
scoring fails closed while offline tiers still apply), `CATALOG_UI`
(`1` lets the catalog-ui browser start). Gateway menu lists:
`MODAL_MODELS` / `BASETEN_MODELS` (comma-separated, 1–3 ids each,
advertised only when the provider base is configured). Popularity
data: OpenRouter Data API (CC-BY). Exact defaults live in
[`.env.example`](../.env.example), which stays canonical.

## OpenRouter-native clients (Iter 25)

Point OpenAI-compatible harnesses at the gateway with an explicit
`baseURL` — no DNS tricks needed (detail:
[proposals/2026-11-01-openrouter-interface.md](proposals/2026-11-01-openrouter-interface.md)):

- OpenAI-shaped clients: `baseURL http://gateway:8080/v1` (curated
  `openrouter/pair/…` menu, routing, budget, retry).
- OpenRouter-native SDKs: `baseURL http://gateway:8080/api/v1` (same
  routed chat/responses handler; account/catalog paths proxy to
  OpenRouter; `GET /api/v1/models` returns the full upstream catalog).

DNS-intercepting `openrouter.ai` toward the gateway stays a local-dev
trick only: the client expects a real `openrouter.ai` certificate, so
it needs either plain `http://` (LAN-only, many SDKs refuse non-TLS)
or a privately distributed CA (e.g. Caddy `tls internal`). Prefer the
explicit `baseURL` above.
No local key store:
backend keys travel with requests, never in config or fixtures. The
one exception is catalog scoring (Phase 4): the offline
`catalog-process --score-backend baseten|modal|openrouter` reads score
keys from the environment (`BASETEN_API_KEY`, `MODAL_SCORE_URL`, `MODAL_KEY_ID`,
`MODAL_KEY_SECRET`, shared `RANKINGS_API_KEY` for the OpenRouter leg,
`OPENROUTER_SCORE_MODEL` for its model — see
[`.env.example`](../.env.example)). The gateway never reads them;
without them online scoring fails closed while offline tiers still
apply. Never commit real values.

## Decision-model catalog (Iter 15)

Which model does the scoring is config, not tribal knowledge
(renamed judging → scoring in Iter 32). Tiers come from two paths:

- Offline (default, Iter 32): every sync derives deterministic
  tiers from the AA coding index — 70+ `high`, 40+ `standard`,
  else `low` — free, no model calls. Models without an index
  stay untiered.
- Online (on demand): `catalog-process --score-backend
  baseten|modal|openrouter` scores through an LLM leg. An online
  verdict always wins over the offline tier on its row.

A scorers file (`catalog-process --scorers <file>`) lists one entry
per backend — `recorded`, `baseten`, `modal`, `openrouter` — with
the scorer model, base URL, the *names* of the secret variables,
the expected prompt version, and the optional known per-call cost;
plus a policy section (`sample_rate`, `only_models`/`only_providers`
scope, `max_calls`, `max_spend_usd`). Recording paths resolve from
the working directory. Selection rules:

- Online scorers run only when explicitly selected with
  `--score-backend`, and only on the policy-selected subset
  (deterministic per-endpoint sampling, so rebuilds re-score the
  same endpoints). `recorded` replays a scores file (fixtures and
  tests); it never calls a model.
- Call and spend caps are checked *before* any score is written:
  a breached cap fails closed with the DB untouched. A spend cap
  with unknown per-call cost fails rather than guessing. Every live
  rebuild prints one spend line (`calls x unit = total (caps …)`).
- Every score stores its audit identity (`score_model`,
  `score_prompt` columns; `score_backend`, `score_model`,
  `score_prompt`, `score_calls`, `score_spend_usd` in `sync_meta`),
  so a quality tier always traces back to the exact scorer + prompt
  behind it. Offline rows read `offline` / `aa-coding-v1`. Rows
  scored before Iter 15 read back `NULL` provenance.
- `--sample-rate`, `--max-calls`, `--max-spend-usd` override the
  file policy. Scorers never run on the request path — this is all
  offline batch, before publish.

## Debug UIs (Iter 17)

Both UIs are read-only and completely off unless explicitly enabled
with an env var — no routes, no background work, no request-path
code runs when they are off.

- Gateway: `GATEWAY_UI=1` registers `GET /ui` on the gateway port:
  snapshot version and counts, the virtual menu, per-model
  offerings with the ranking's default-orientation provider order,
  free offers, a routing inspector, and the metrics text. Rendered
  on demand from short read locks; routing never waits on it and it
  never calls upstream. Unset (the default) leaves `/ui` (and
  `/ui/preview`) unregistered — the paths fall through to the
  fallback proxy like any unknown path. `GET /` always serves a
  fixed landing page (version, build hash, config status, links)
  instead of proxying upstream. The gateway binds 0.0.0.0: `/ui`,
  `/metrics`, and `/` are unauthenticated by design, so expose the
  port beyond localhost only behind your own firewall (same rule as
  catalog-ui above); beyond a LAN, terminate TLS in front.
- Public surface (Q09-A): no gateway route requires an operator
  credential — routing is pure BYOK (the caller's key rides each
  request), smart/preview need only the caller's key, and `/metrics`
  is always open. That is the product choice, stated loudly: run it
  on localhost, or behind firewall + TLS with source CIDRs (the
  Terraform stage does this). There is no shared-secret gate; do not
  invent one per deploy.
- Inspector (Iter 29): `/ui` shows ten fixed non-smart virtual combos
  top-3 plus a custom-model box. Paste the exact `Authorization`
  header value; the page sends it as the `Authorization` header to
  `GET /ui/preview?model=<id>&top=N`, which hashes it with the same
  seeded hash the request path pins with and returns the ordered
  candidates with every stored column (prices, effective cost,
  class, tier/score, tools, decisions, promo, free, availability,
  rank) plus the sticky pick (with this key) vs the fresh pick (no
  session) and the session hash. The raw key is never logged and
  never stored; previews never create pins. Plain-request view: tools,
  budget, and only/ignore constraints are not applied, so a capped
  request can fail closed where the preview shows a pick. Smart ids
  preview as an explanatory rejection (their tier comes from the live
  classifier leg). Missing `model` answers 400, no router answers 503.
- Catalog: `CATALOG_UI=1 catalog-ui --dir <artifact-dir>
  [--port 8080] [--bind 127.0.0.1] [--log-file <path>]` serves a
  browser over the published artifacts: run history (manifests
  newest-first with verify status), models, endpoints (with score
  provenance), ranking order, rebuild/audit meta, and the last 200
  lines of the log file when given. The binary refuses to start
  without the flag. Nothing is cached, and the only write is the
  `.resync-requested` trigger behind the overview's Force re-sync
  button (`POST /resync`): the sync loop polls for it every 10s
  between scheduled cycles and starts the next cycle early, so the
  UI never touches the DB and the loop stays the single writer. A
  stale read-only mount answers 500 naming the fix. Bind is
  localhost-only by
  default; pass `--bind 0.0.0.0` only behind a firewall or reverse
  proxy. In Docker the `catalog-ui` compose service (profile `ui`,
  off by default) serves the shared volume:
  `docker compose --profile ui up catalog-ui --build`, then open
  http://localhost:8080 (host port from `CATALOG_UI_PORT`). The
  container port comes from `PORT` (default 8080, `--port` wins).
  The deployed stack (`compose.dev.yaml`) runs catalog-ui with no
  published port on the front-proxy network (`PROXY_NETWORK`,
  default `nginx-proxy_default`, which must already exist on the
  host): create a proxy host in the NPM UI for `catalog-ui:8080`
  (stable container name, like `gateway`).

## Ranking

From Iter 6: `catalog-rank` rebuilds the provider ranking offline from
the catalog DB plus optional trends and recorded AI adjustments, and
publishes a versioned `ranking-<version>.json` artifact beside the DB.
The sync container runs it every cycle (`TRENDS`/`ASSIST`/`PROMOS`
inputs default to the `/input` fixtures when present). The gateway
re-resolves its `CATALOG_DB` directory every `RELOAD_INTERVAL_SECS`
(default 60s, 0 disables) and atomically publishes the new snapshot
and ranking, keeping the last good ones when a reload fails or an
artifact is missing; reload attempts are counted in
`pennyrouter_catalog_reload_total{result="ok|error"}`. Last-good
retention and joint atomicity are different properties: each of
snapshot and ranking keeps its own last good copy, but the two swap
in sequence, so a reader can briefly see a new snapshot with the
previous ranking.
From Iter 7, "cheapest" means effective cost under the balanced
workload profile (see [pricing.md](pricing.md)), and `openrouter/free`
routes to any free offering.

## Guards and timeouts

From Iter 2: request bodies estimating over 1M tokens (~4 MiB at 4
bytes/token) are rejected with 413 before contacting the backend.
From Iter 11, uploads are also bounded in time (30s idle per chunk,
60s total; stalled uploads answer 408) and admitted before
collection, so below-cap incomplete uploads cannot accumulate.
Upstream budgets are connect 10s, first response 30s, stream idle 60s
per chunk, and 300s total for buffered responses; timeouts answer 504.
Buffered upstream bodies are capped at 16 MiB (oversize answers 502).
Upstream JSON errors pass through; anything else is wrapped in an
OpenAI-shaped error keeping the upstream status. End-to-end response
headers (encoding, range, retry-after, auth challenges, validators,
redirect targets) are preserved; rewritten bodies keep negotiation
metadata but get truthful representation headers. Redirects pass
through with `Location` instead of being followed. A stalled or
broken stream ends with a terminal SSE `error` event after the intact
prefix — truncation never looks like clean EOF. Beyond the
concurrency/queue bounds the gateway sheds with OpenAI-shaped 503 +
`Retry-After`.

## Observability

From Iter 3: every request logs one JSON line to stdout with request ID
(incoming `x-request-id` or generated, also echoed back), method, route,
status, latency, and outcome. `GET /metrics` serves Prometheus counters
(requests by route/status, upstream errors by kind, guard rejections,
in-flight gauge) plus a latency histogram. The chosen provider rides
the `x-pennyrouter-provider` response header, not the log line.
Never log bodies, prompts, completions, or Authorization headers. From Iter 8, fallback-proxied
calls emit an extra `{"fallback_proxy":true,...}` line with method and
full path (see [api.md](api.md)). Later, readiness/snapshot health
may complement `/health` without changing its liveness contract.
