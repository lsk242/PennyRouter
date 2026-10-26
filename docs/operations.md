# Operations

## Run it

Local run needs a Rust toolchain (rustup, stable); the container needs
only Docker.

```sh
cargo run -p pennyrouter-gateway   # serves :3000, PORT=... to override
curl localhost:3000/health         # {"status":"ok"}
```

```sh
docker compose -f docker/compose.yaml up --build
```

CI builds `ghcr.io/lsk242/pennyrouter:latest` and smoke-tests `/health`
on every PR, and pushes it on merges to main.

## Local development

Docker-based local dev (see `docker/`):

- One command to build and run the gateway locally.
- A resource-capped profile matching the ~1 CPU / 1 GB target for perf
  runs (Iter 3).
- No secrets in the repo: backend keys travel with requests, not config.

## Configuration

Keep config minimal and env-based. No local key store for now. Anything the
gateway needs (ports, upstream URLs, sync intervals) must have sane
defaults so a bare `docker compose up` works.

## Observability

From Iter 3: structured logs, a latency histogram, and basic counters
(requests, errors, per-provider selections). Enough to prove the perf
baseline and to debug routing decisions later (which provider, which
index version — never request bodies).
