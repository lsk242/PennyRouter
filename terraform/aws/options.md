# AWS options

Prices USD, us-east-1 on-demand, October 2026.

## Compute

- **Assured pick: m7g.medium** — 1 Graviton vCPU, 4 GB RAM,
  **≈$30–35/mo** (non-burstable, guaranteed). Matches the ~1 CPU /
  1 GB ballpark with RAM headroom. ARM: needs an arm64 image
  build in CI (one extra `--platform` line); Oracle's ARM A1 is
  cheaper for more cores (see `terraform/oracle/options.md`).
- x86 alternative: **m7i-flex.large** — 2 vCPU, 8 GB, **≈$70–82/mo**.
- Budget alternative: **Lightsail 4 GB** — 2 vCPU, 4 GB RAM,
  80 GB SSD, **$24/mo flat** incl. 4 TB transfer. Burstable
  (shared) CPU — staging-grade, not assured.
- EC2 extras: EBS gp3 ≈$0.08/GB-mo (30 GB ≈ $2.40), Elastic IP
  free while attached.

## Transfer

- EC2: first **100 GB/mo free**, then **$0.09/GB** (≈$90/TB)
  for the first 10 TB. This is the bill to watch at scale.
- Lightsail: 4 TB included on the 4 GB plan; US-East overage
  $0.09/GB.

## Reverse proxy + TLS

- ALB ≈$16–23/mo minimum + LCU charges; Lightsail LB $18/mo
  flat. Optional at our scale — single box + Caddy is enough.
- **ACM certificates free** (auto-renewed) on ALB/CloudFront/
  API Gateway. API Gateway (HTTP API) ≈$1 per million requests
  if a managed front door is ever wanted.

## Security

- Security Groups (stateful firewall) + NACLs, free.
- Shield Standard (L3/L4 DDoS) free; WAF from ~$5/mo + rules;
  IAM + SSM Session Manager (ssh without open port 22).

## Terraform

- Provider `hashicorp/aws` (mature, largest resource surface).
- Key resources: `aws_instance`, `aws_ebs_volume`,
  `aws_security_group`, `aws_lb` + `aws_acm_certificate`
  (if LB), or `aws_lightsail_instance` for the flat plan.

## Monthly floor (assured, single server)

m7g.medium ≈$33 + EBS ≈$2.50 = **≈$36/mo** (+ transfer past
100 GB; + ≈$20 if an ALB is added).

## Sources

- [EC2 instance types and pricing, Oct 2026](https://www.logeshwaran.org/2026/10/ec2-instance-types-pricing.html)
- [AWS Lightsail pricing 2026](https://tech-insider.org/digitalocean-vs-vultr-vs-aws-lightsail-2026/)
- [AWS data transfer costs](https://www.cloudzero.com/blog/reduce-data-transfer-costs/)
