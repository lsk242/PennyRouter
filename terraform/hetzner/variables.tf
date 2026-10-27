# Hetzner Cloud: first PennyRouter deployment (single gateway box).
#
# One assured VM (default CCX13: 2 dedicated vCPU / 8 GB) in a private
# network, behind a firewall that opens 80/443 to the world and SSH plus
# the gateway port to operator IPs only. Cloud-init installs Docker and
# starts the pull-only gateway image; with `domain_name` set, Caddy
# terminates TLS automatically (point the domain's A record at the
# server IP first). Nothing is built on the server.

variable "hcloud_token" {
  description = "Hetzner Cloud API token (sensitive; export TF_VAR_hcloud_token)."
  type        = string
  sensitive   = true
}

variable "location" {
  description = "Hetzner location (EU keeps the 20 TB transfer allowance)."
  type        = string
  default     = "nbg1"

  validation {
    condition     = contains(["nbg1", "fsn1", "hel1", "ash", "hil", "sin"], var.location)
    error_message = "Unknown Hetzner location."
  }
}

variable "server_type" {
  description = "Server plan (CCX13 is the assured pick; CPX22/CPX02 for staging)."
  type        = string
  default     = "ccx13"
}

variable "image" {
  description = "Server image."
  type        = string
  default     = "ubuntu-24.04"
}

variable "ssh_public_key" {
  description = "Operator SSH public key (single line, e.g. ssh-ed25519 ...)."
  type        = string

  validation {
    condition     = can(regex("^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp[0-9]+) [A-Za-z0-9+/=]+", var.ssh_public_key))
    error_message = "Must look like an SSH public key."
  }
}

variable "ssh_allowed_cidrs" {
  description = "CIDRs allowed SSH + direct gateway-port access (e.g. [\"203.0.113.7/32\"]). No default on purpose."
  type        = list(string)

  validation {
    condition     = length(var.ssh_allowed_cidrs) > 0
    error_message = "Set at least one operator CIDR."
  }
}

variable "domain_name" {
  description = "Public domain for automatic HTTPS via Caddy. Empty means HTTP-only on host_port."
  type        = string
  default     = ""
}

variable "acme_email" {
  description = "Contact email for Let's Encrypt (optional; Caddy registers without one)."
  type        = string
  default     = ""
}

variable "gateway_image" {
  description = "Gateway image to pull (CI pushes :latest on every main merge)."
  type        = string
  default     = "ghcr.io/lsk242/pennyrouter:latest"
}

variable "host_port" {
  description = "Published gateway port (bound to localhost when TLS is on)."
  type        = number
  default     = 8003
}

variable "openrouter_base_url" {
  description = "Upstream base URL for chat completions."
  type        = string
  default     = "https://openrouter.ai"
}

variable "enable_backups" {
  description = "Automated server backups (+20% server cost)."
  type        = bool
  default     = false
}
