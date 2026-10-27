# Terraform

One folder per candidate cloud provider. Each folder holds an
`options.md` note: the cheapest viable server shapes for PennyRouter,
transfer pricing, TLS/reverse-proxy options, security primitives, and
the Terraform provider plus the key resources an implementation uses.

Hetzner is the selected first provider: `hetzner/` holds the live
`.tf` config (see its README). The other folders stay comparison
notes until a second provider is pulled forward. The cross-provider
comparison lives in [docs/cloud-providers.md](../docs/cloud-providers.md).
