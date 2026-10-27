# Hetzner deployment (Terraform)

First cloud provider (selected from
[docs/cloud-providers.md](../../docs/cloud-providers.md)): one assured
VM (default CCX13, 2 dedicated vCPU / 8 GB, ≈€43.50/mo ex-VAT) plus
primary IPv4, private network, and firewall. Cloud-init installs Docker
and starts the pull-only gateway image; with `domain_name` set, Caddy
terminates TLS automatically.

## Use

```sh
cd terraform/hetzner
export TF_VAR_hcloud_token=...        # Hetzner Cloud API token
export TF_VAR_ssh_public_key="$(cat ~/.ssh/id_ed25519.pub)"
export TF_VAR_ssh_allowed_cidrs='["203.0.113.7/32"]'
export TF_VAR_domain_name=gw.example.com   # optional; empty = HTTP-only

terraform init
terraform plan -out plan
terraform apply plan
```

Then point the domain's A record at the `server_ipv4` output (Caddy
fetches its certificate on first HTTPS hit) and open the `gateway_url`
output. Redeploys ship the `:latest` image on boot; to roll a new
image onto a running box, reuse the Docker-stage flow
(`DEPLOY_HOST=... make deploy`) or re-apply.

## Notes

- The GHCR image must be pullable without credentials (public
  package): cloud-init runs no registry login, by design — no secrets
  in user data, and backend keys travel with requests, never in config.
- Firewall: 80/443 world-open; SSH and the direct gateway port only
  from `ssh_allowed_cidrs`. With a domain, the gateway binds
  localhost and Caddy owns the edge.
- State is local (`terraform.tfstate`, gitignored) until a shared
  backend is chosen. Never commit `.tfvars` or state files.
- Staging: same module with `server_type = "cpx22"` (or `cpx02`) in a
  second workspace/directory — patterns stay shared.
