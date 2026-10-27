# Azure options

Prices USD, East US Linux pay-as-you-go, October 2026.

## Compute

- **Assured pick: D2as v6** — 2 AMD vCPU, 8 GB RAM,
  **$66.28/mo** (guaranteed, non-burstable). 1-year savings
  plan brings it to ≈$46/mo.
- Budget alternative: **B2ls v2 / B2s** — 2 burstable vCPU,
  4 GB RAM, **≈$31–37/mo** (40% CPU baseline, credit-capped
  bursts). Staging-grade, not assured.
- Extras: 32 GB managed disk ≈$5/mo, static IPv4 ≈$3.65/mo.

## Transfer

- First **100 GB/mo free**, then **$0.087/GB** (≈$87/TB)
  for the first 10 TB (North America / Europe).

## Reverse proxy + TLS

- Application Gateway (L7) or Front Door from tens of $/mo;
  overkill at our scale — single VM + Caddy is enough.
- **Managed TLS**: Front Door and App Gateway both serve
  auto-renewed managed certificates. API Management is the
  full gateway product but starts ≈$50/mo (Developer) —
  not a budget fit.

## Security

- Network Security Groups (stateful firewall), free.
- DDoS Network Protection and Firewall/Defender tiers are
  $$$ enterprise SKUs — out of scope; NSG + no open ssh
  (Azure Bastion/Just-in-time) is the budget posture.

## Terraform

- Provider `hashicorp/azurerm` (mature).
- Key resources: `azurerm_linux_virtual_machine`,
  `azurerm_network_security_group`, `azurerm_public_ip`,
  `azurerm_managed_disk`.

## Monthly floor (assured, single VM)

D2as v6 $66.28 + disk ≈$5 + IP ≈$3.65 = **≈$75/mo**
(+ egress past 100 GB).

## Sources

- [Azure Linux VM pricing](https://azure.microsoft.com/en-us/pricing/details/virtual-machines/)
- [Azure bandwidth pricing](https://azure.microsoft.com/pricing/details/bandwidth)
- [Azure vs AWS egress](https://intercept.cloud/en-gb/blogs/azure-vs-aws-pricing)
