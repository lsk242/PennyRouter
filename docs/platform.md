# Platform

Parallel track to development: development delivers Docker images, platform
delivers deployments. Both iterate in small steps; neither blocks the other
longer than one iteration.

## Stages

1. **Docker (now).** Local `docker compose` dev plus a minimal runnable
   image. Unblocked by Iter 1: the gateway exposes `GET /health` so the
   image has a container check. No cloud yet.
2. **First cloud provider (later, TBD).** One provider first (Hetzner,
   AWS, Azure, … — selection deferred). Terraform-managed: VM creation,
   resource allocation, networking, deploy.
3. **More providers.** Same Terraform patterns extended; provider choice
   per environment, never hardcoded in app code.

## Principles

- Everything as code: Terraform for infra from the first cloud step.
- Resource allocation is explicit and reviewable (target ballpark ~1 CPU /
  1 GB per gateway unit, matching [performance.md](performance.md)).
- App stays provider-agnostic: no cloud SDKs on the request path.
- Secrets never live in images or Terraform state outputs; keys travel
  with requests (see [api.md](api.md)).

## Delivery pipeline (CI/CD)

- **Now:** security scanning on every push/PR (secret scan — no API keys
  or private data may land in the repo) plus Dependabot for
  Actions/Cargo/Docker.
- **Gated on Iter 1:** Docker build + push pipeline. Blocked until the
  minimal gateway (with `/health`) exists to build an image from. When
  enabled: build, push tagged image, smoke-check `/health` from the
  built image.
- **Later:** Terraform plan/apply automation per environment, gated on
  the first provider selection.

See [operations.md](operations.md) for local Docker dev and observability.
