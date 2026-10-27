# Oracle Cloud (OCI) options

Prices USD, pay-as-you-go list rates, October 2026. Note: 1 OCPU =
1 vCPU on ARM (Ampere), 2 vCPUs on x86 (AMD, SMT).

## Compute

- **Assured pick: A1 Flex 2 OCPU / 12 GB** (Ampere ARM, dedicated
  physical cores, no SMT) — $0.01/OCPU-hr + $0.0015/GB-hr =
  **≈$28/mo** + a few $ for the boot volume. Cheapest assured
  shape in the comparison. ARM: needs an arm64 image build in CI.
- x86 alternative at list rates (E4: $0.025/OCPU-hr +
  $0.0015/GB-hr): 1 OCPU (2 vCPU) / 8 GB ≈ **$27/mo**.
- Staging: **Always Free** A1 (2 OCPU / 12 GB total, halved from
  4/24 in mid-2026) + 200 GB storage + 10 TB egress = **$0**.
  Caveats: A1 capacity often unavailable per region, idle free
  instances get reclaimed, account approval can be slow.

## Transfer

- **10 TB/mo free, then $0.0085/GB** (≈$8.50/TB) — the best
  egress deal of the six, by far. Effectively a non-issue for a
  token gateway.

## Reverse proxy + TLS

- Flexible LB ≈$8.25/mo base + bandwidth; Network LB (L4)
  **free**. Optional at our scale — single box + Caddy is enough.
- Certificates service is free but private-CA oriented; public
  TLS on the LB is bring-your-own-cert (e.g. Let's Encrypt via
  your own automation) — no ACM-style one-click public issuance.
- API Gateway exists (usage-priced) if a managed front door is
  ever wanted.

## Security

- Security lists / NSGs free; **Bastion service free** (ssh
  without an open port 22); Cloud Guard posture monitoring;
  WAF available usage-priced.

## Terraform

- Provider `oracle/oci` (official, mature) plus OCI Resource
  Manager (hosted Terraform, free).
- Key resources: `oci_core_instance`, `oci_core_vcn`,
  `oci_core_security_list`, `oci_load_balancer_load_balancer`,
  `apigateway_gateway` (if ever needed).

## Monthly floor (assured, single server)

A1 Flex 2/12 GB ≈$28 + boot volume ≈$1–2 = **≈$30/mo** with
10 TB egress included.

## Sources

- [Oracle cloud price list (compute)](https://www.oracle.com/apac/engineered-systems/exadata/database-service/pricing/)
- [Oracle LB pricing](https://www.oracle.com/cis/cloud/networking/load-balancing/pricing/)
- [OCI egress terms](https://www.oracle.com/ie/cloud/data-egress-costs/)
- [Free tier halved to 2/12, Aug 2026](https://dev.to/cash602cmd/how-to-check-if-your-oracle-cloud-a1-free-tier-instance-just-got-quietly-downgraded-2h6m)
