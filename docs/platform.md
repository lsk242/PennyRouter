# Platform

Parallel track to development: development delivers Docker images, platform
delivers deployments. Both iterate in small steps; neither blocks the other
longer than one iteration.

## Stages

1. **Docker (done).** Local `docker compose` dev plus minimal runnable
   images. Dev compose builds from source; server files
   (`compose.dev.yaml`, `compose.prod.yaml`) deploy by pulling the
   GHCR images — compose file plus shipped `.env` on the server, no
   build there.
2. **First cloud provider (now): Hetzner.** Selected from the
   six-provider comparison ([cloud-providers.md](cloud-providers.md)).
   Terraform-managed (`terraform/hetzner`): CCX13 VM, primary IPv4,
   private network, firewall, Caddy TLS + pull-only gateway deploy
   via cloud-init. CI checks formatting and validates the config.
3. **More providers.** Same Terraform patterns extended; provider choice
   per environment, never hardcoded in app code.

## Principles

- Everything as code: Terraform for infra from the first cloud step.
- Resource allocation is explicit and reviewable (target ballpark ~1 CPU /
  1 GB per gateway unit, matching [performance.md](performance.md)).
- App stays provider-agnostic: no cloud SDKs on the request path.
- Secrets never live in images or Terraform state outputs. Gateway
  serving keys travel with requests (see [api.md](api.md)); config
  keys ship in the gitignored root `.env` (chmod 600 on the server).

## Delivery pipeline (CI/CD)

- **Now:** security scanning on every push/PR (secret scan — no API keys
  or private data may land in the repo) plus Dependabot for
  Actions/Cargo/Docker.
- **Done:** Docker build + push pipeline (live since v0.1.0):
  build, push tagged images, smoke-check `/health` from the built
  image.
- **Now:** Terraform fmt + validate in CI for `terraform/hetzner`.
- **Later:** Terraform plan/apply automation per environment.

See [operations.md](operations.md) for local Docker dev and observability.
