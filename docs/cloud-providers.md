# Cloud provider comparison

Input to the "select first cloud provider" platform decision (still
TBD — this doc compares, it does not choose). Detail per provider in
`terraform/<provider>/options.md`.

## Minimal specs

Project floor is ~1 CPU / 1 GB ([performance.md](performance.md)),
but the picks below use a **recommended floor of 2 vCPU / 4 GB**
with a CPU guarantee (dedicated or non-burstable): TLS
termination, SSE fan-out, and 1M-token request buffers all share
that box, and burstable credits are the classic 3 a.m. throttle.
Each provider also gets a cheaper shared/burstable staging note.

## Price snapshot (October 2026, cheapest assured shape)

| Provider | Assured VM | ~price/mo | Transfer |
|---|---|---|---|
| Hetzner | CCX13, 2/8 GB | €43+VAT | 20 TB incl, €1/TB |
| AWS | m7g.medium, 1/4 GB | ~$36 | 100 GB, then $0.09/GB |
| GCP | e2-standard-2, 2/8 GB | ~$52 | ~$0.12/GB, 1 GiB free |
| Azure | D2as v6, 2/8 GB | ~$75 | 100 GB, then $0.087/GB |
| DigitalOcean | CPU-Opt, 2/4 GB | $42 | TBs incl, $0.01/GB |
| Oracle | A1 Flex, 2/12 GB | ~$30 | 10 TB free, $0.0085/GB |

Totals include the small extras (disk, IPv4) where they apply;
all six can serve the gateway on one box — no managed LB needed
yet (single Caddy/Traefik + gateway compose).

### Hetzner

Cheapest staging (CPX02, 1/1 GB, €5.99/mo + IPv4; the CX line
is currently unorderable) and most generous transfer
(20 TB EU included). Dedicated CCX13 at €42.99/mo lost its old
price edge in the June 2026 adjustment, but the all-in floor
stays low. Managed Let's Encrypt certs free; firewall free;
Terraform provider official and mature. Caveats: few regions,
US/Singapore transfer allowances much smaller, VAT handling for
EU customers.

### AWS

m7g.medium (Graviton, 1 vCPU/4 GB) is the cheapest assured box
(~$36/mo all-in) but needs an arm64 image build in CI. Lightsail
$24/mo is the simplest flat staging. Deepest extras bench: ALB,
ACM free certs, API Gateway, WAF. Caveat: $0.09/GB egress past
100 GB dominates the bill the moment traffic grows.

### GCP

e2-standard-2 at ~$52/mo is a solid assured box with the best
committed-use discounts (~30–50% off). Managed certs and LB are
polished. Caveat: steepest entry egress ($0.12/GB Premium),
weakest free allowance.

### Azure

D2as v6 is the priciest assured box (~$75/mo all-in) with no
compensating transfer edge ($0.087/GB past 100 GB). Managed TLS
via Front Door/App Gateway; API Management too pricey for this
stage. Only pick: existing Microsoft footprint or credits.

### DigitalOcean

CPU-Optimized $42/mo flat is the cheapest 2-vCPU dedicated
shape, with $0.01/GB overage transfer that removes egress
anxiety. Managed LB ($12) + free Let's Encrypt + free firewall
cover the likely growth path. Smallest Terraform surface to
review. Caveat: no API gateway or WAF products.

### Oracle

A1 Flex 2 OCPU/12 GB at ~$30/mo all-in is the cheapest assured
shape, on dedicated ARM cores, with 10 TB free egress behind it —
then $0.0085/GB, an order of magnitude under everyone else. Free
tier ($0 staging) is real but capacity-constrained with idle
reclamation. Flexible LB ≈$8/mo, free L4 LB and bastion; public
TLS is bring-your-own-cert (no ACM-style one-click issuance).
Caveat: account approval friction and a weaker support
reputation; ARM needs an arm64 CI build like AWS Graviton.

## Terraform support

All six have mature, first-class providers (`hcloud`, `aws`,
`google`, `azurerm`, `digitalocean`, `oci`). No provider is
ruled in or out on Terraform grounds; the hyperscalers have
larger surfaces (more to review, more footguns), Hetzner/DO are
small and readable. Oracle additionally offers hosted Terraform
(Resource Manager) free.

## What the numbers suggest

- Egress, not compute, separates the six at scale: Oracle
  (10 TB free, then $8.50/TB), Hetzner (€1/TB) and DO ($10/TB)
  vs ~$90–120/TB on AWS/GCP/Azure.
- For the first box, the race is Oracle A1 Flex (~$30, 2 OCPU /
  12 GB ARM) vs AWS m7g.medium (~$36, 1 vCPU ARM) vs DO
  CPU-Optimized ($42) vs Hetzner CCX13 (≈€43.50).
- Staging can live on any €5–25 shared shape; keep it on the
  same provider as prod to share the Terraform patterns.

Excluded: second-tier VPS shops (Vultr, Linode/Akamai, OVHcloud
— fine hosts, smaller ecosystems). Revisit if the shortlist
fails on real requirements.
