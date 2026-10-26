# Operations

## Local development

Docker-based local dev (images and compose arrive with Iter 1; the `docker/`
directory is reserved now):

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
