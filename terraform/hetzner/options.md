# Hetzner Cloud options

Prices ex-VAT, October 2026. EU VAT (19% Germany) applies unless a
valid VAT ID is on file.

## Compute

- **Assured pick: CCX13** — 2 dedicated vCPU, 8 GB RAM, 160 GB SSD,
  **€42.99/mo**. Dedicated (non-shared) cores; the honest floor for
  "not overshared" on Hetzner.
- Budget alternative: **CPX02** — 1 shared AMD vCPU, 1 GB RAM,
  20 GB NVMe, **€5.99/mo** (+€0.50 IPv4). Exactly the project
  floor, so staging-only: shared cores, and 1 GB leaves no room
  for catalog snapshots to grow. Cheapest orderable plan right
  now — the cheaper CX cost-optimized line (CX23 €5.49) is
  currently listed as unavailable.
- Middle ground: **CPX22** — 2 shared AMD vCPU, 4 GB RAM,
  **€19.49/mo**. Fast shared cores, still no guarantee.
- All June-2026 prices reflect the 15 June 2026 price adjustment;
  dedicated tiers roughly tripled.
- IPv4 costs extra: **+€0.50/mo** per Primary IPv4 (IPv6 free).
- Regions: Nuremberg, Falkenstein, Helsinki (EU), Ashburn,
  Hillsboro (US), Singapore.

## Transfer

- Included per server: **20 TB/mo EU**, 1 TB US, 0.5 TB Singapore.
- Overage: **€1/TB** (EU + US), **€7.40/TB Singapore**.
- Gateway responses are small (chat tokens), so EU allowance is
  effectively unlimited; mind the region if serving the US/Asia.

## Reverse proxy + TLS

- Managed load balancer **LB11 from €7.49/mo** (25 services, 10k
  connections, TLS termination). Optional at our scale — a single
  server with Caddy/Traefik in front of the gateway is enough.
- **Managed Let's Encrypt certificates are free** (auto-renewed,
  requires Hetzner DNS), attachable to load balancers; or upload
  your own cert. No API-gateway product.

## Security

- Cloud Firewall: free, stateful, per-server rules (default deny
  inbound except 443/80 + ssh from operator IPs).
- Private networks free; floating IPs from ~€0.50/mo; automated
  backups +20%; snapshots billed per GB.
- DDoS protection is basic/inline; no WAF product — put
  Cloudflare in front if needed.

## Terraform

- Provider `hetznercloud/hcloud` (official, mature).
- Key resources: `hcloud_server`, `hcloud_primary_ip`,
  `hcloud_firewall`, `hcloud_network`, `hcloud_load_balancer`,
  `hcloud_managed_certificate`, `hcloud_ssh_key`.

## Monthly floor (assured, single server, EU, ex-VAT)

CCX13 €42.99 + IPv4 €0.50 = **≈€43.50/mo** (≈€51 with LB).

## Sources

- Hetzner docs: primary IPs, managed certificates, load balancers
- [Hetzner Cloud review 2026, pricing](https://www.bitdoze.com/hetzner-cloud-review-2026/)
- [Hetzner vs AWS vs DO egress math](https://www.forasoft.com/blog/article/aws-vs-digitalocean-vs-hetzner-1302)
