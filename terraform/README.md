# Terraform (scaffold)

One folder per candidate cloud provider. Each folder holds an
`options.md` note: the cheapest viable server shapes for PennyRouter,
transfer pricing, TLS/reverse-proxy options, security primitives, and
the Terraform provider plus the key resources a future implementation
would use.

No `.tf` code yet — provider selection is still TBD. The
cross-provider comparison lives in
[docs/cloud-providers.md](../docs/cloud-providers.md).
