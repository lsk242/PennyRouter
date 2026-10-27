# DigitalOcean options

Prices USD, October 2026. Flat monthly caps, no per-service
metering surprises.

## Compute

- **Assured pick: CPU-Optimized 2 vCPU / 4 GB** — dedicated
  vCPU, NVMe, **$42/mo**. The cheapest dedicated-CPU x86 shape
  in the comparison (Oracle's ARM A1 is cheaper — see
  `terraform/oracle/options.md`).
- Budget alternative: **Basic 2 vCPU / 4 GB** — shared CPU,
  80 GB SSD, **$24/mo** incl. 4 TB pooled transfer.
  Staging-grade, not assured.
- Larger assured step: General Purpose 2 vCPU / 8 GB, $63/mo.

## Transfer

- Generous pooled allowance per team (Basic $24 plan: 4 TB
  verified); overage **$0.01/GB** (≈$10/TB) — 9x cheaper
  than AWS/Azure/GCP entry tiers.

## Reverse proxy + TLS

- Managed load balancer **from $12/mo/node** with health
  checks and HTTP/3. Optional at our scale — single
  droplet + Caddy is enough.
- **Let's Encrypt certificates free** (auto-renewed,
  managed) on the LB. No API-gateway product.

## Security

- Cloud Firewall: free, stateful, per-droplet rules.
- No managed WAF — Cloudflare in front is the usual answer.
  Backups +20%, snapshots $0.05/GB-mo.

## Terraform

- Provider `digitalocean/digitalocean` (mature, small
  surface — fast to review).
- Key resources: `digitalocean_droplet`,
  `digitalocean_firewall`, `digitalocean_loadbalancer`,
  `digitalocean_certificate`, `digitalocean_ssh_key`.

## Monthly floor (assured, single droplet)

CPU-Optimized **$42/mo** flat (+ $12 if a LB is added;
transfer effectively included).

## Sources

- [DigitalOcean review 2026](https://betterstack.com/community/guides/web-servers/digitalocean-review/)
- [Dedicated vs shared vCPU 2026](https://dev.to/hostingsift/dedicated-vs-shared-vcpu-what-the-premium-actually-costs-in-2026-5cge)
- [DigitalOcean load balancers](https://www.digitalocean.com/products/load-balancers)
