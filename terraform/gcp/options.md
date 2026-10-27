# Google Cloud options

Prices USD, us-central1 on-demand, October 2026.

## Compute

- **Assured pick: e2-standard-2** — 2 vCPU, 8 GB RAM,
  **$48.92/mo** (guaranteed, non-shared). Committed-use
  discounts cut this ~30–50% with a 1–3 year term.
- Budget alternative: **e2-medium** — up to 2 vCPU (shared
  core, burstable), 4 GB RAM, **≈$24.46/mo**. Staging-grade,
  not assured.
- Extras: 20 GB balanced persistent disk ≈$2–4/mo; static
  IPv4 ≈$3–4/mo if reserved but idle (in-use ephemeral is
  covered by the VM).

## Transfer

- Premium Tier: **≈$0.12/GB** for the first 10 TB (first
  1 GiB free) — the steepest entry egress of the six.
- Standard Tier: ≈$0.085/GB with ~200 GB/mo free. Fine for
  API responses; pick per region at deploy time.

## Reverse proxy + TLS

- External L7 load balancer from ≈$18/mo + forwarding rules.
  Optional at our scale — single VM + Caddy is enough.
- **Google-managed SSL certificates free** (auto-renewed) on
  the LB. API Gateway (pay-per-use) if a managed front door
  is ever wanted.

## Security

- VPC firewall rules, free; Cloud Armor WAF from ~$5/mo +
  per-request; Identity-Aware Proxy can gate ssh/admin
  without a public bastion.

## Terraform

- Provider `hashicorp/google` (mature).
- Key resources: `google_compute_instance`,
  `google_compute_firewall`, `google_compute_disk`,
  `google_compute_managed_ssl_certificate` (if LB).

## Monthly floor (assured, single VM)

e2-standard-2 $48.92 + disk ≈$3 = **≈$52/mo** (+ steep
egress past the free allowance; + ≈$18 if a LB is added).

## Sources

- [GCP e2-standard-2 pricing](https://www.economize.cloud/resources/gcp/pricing/compute-engine/e2-standard-2/)
- [Google Cloud pricing 2026](https://www.eon.io/blog/google-cloud-pricing)
- [Cloud egress 2026 comparison](https://egresscost.com/)
