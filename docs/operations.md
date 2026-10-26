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
never builds — copy one file plus `.env` to the server:

```sh
docker compose -f compose.prod.yaml pull
docker compose -f compose.prod.yaml up -d
```

CI builds `ghcr.io/lsk242/pennyrouter:latest` and smoke-tests `/health`
on every PR, and pushes it on merges to main. It also rejects any
`build:` section in `compose.prod.yaml`, keeping deploys pull-only.

## Local development

Docker-based local dev (see `docker/`):

- One command to build and run the gateway locally.
- A resource-capped profile matching the ~1 CPU / 1 GB target for perf
  runs (Iter 3).
- No secrets in the repo: backend keys travel with requests, not config.

## Configuration

Keep config minimal and env-based (`.env`, see [`.env.example`](../.env.example)):
`PORT` (container port), `HOST_PORT` (published port), `IMAGE` (dev:
tag for the local build; prod: image to pull). No local key store:
backend keys travel with requests, never in config or fixtures.

## Observability

From Iter 3: structured logs, a latency histogram, and basic counters
(requests, errors, per-provider selections). Enough to prove the perf
baseline and to debug routing decisions later (which provider, which
index version — never request bodies).
