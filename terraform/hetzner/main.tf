# Networking + firewall + the gateway server. One box, no load
# balancer yet: a single Caddy + gateway serves this stage.

resource "hcloud_ssh_key" "operator" {
  name       = "pennyrouter-operator"
  public_key = var.ssh_public_key
}

resource "hcloud_network" "main" {
  name     = "pennyrouter"
  ip_range = "10.0.0.0/16"
}

resource "hcloud_network_subnet" "servers" {
  network_id   = hcloud_network.main.id
  type         = "cloud"
  network_zone = "eu-central"
  ip_range     = "10.0.1.0/24"
}

resource "hcloud_primary_ip" "gateway_v4" {
  name        = "pennyrouter-gw-v4"
  type        = "ipv4"
  location    = var.location
  auto_delete = true
}

resource "hcloud_firewall" "gateway" {
  name = "pennyrouter-gateway"

  rule {
    description = "HTTP for ACME challenges"
    direction   = "in"
    protocol    = "tcp"
    port        = "80"
    source_ips = [
      "0.0.0.0/0",
      "::/0",
    ]
  }

  rule {
    description = "Public HTTPS"
    direction   = "in"
    protocol    = "tcp"
    port        = "443"
    source_ips = [
      "0.0.0.0/0",
      "::/0",
    ]
  }

  rule {
    description = "Operator SSH"
    direction   = "in"
    protocol    = "tcp"
    port        = "22"
    source_ips  = var.ssh_allowed_cidrs
  }

  rule {
    description = "Direct gateway port for operators (TLS serves the world)"
    direction   = "in"
    protocol    = "tcp"
    port        = tostring(var.host_port)
    source_ips  = var.ssh_allowed_cidrs
  }
}

resource "hcloud_server" "gateway" {
  name        = "pennyrouter-gw"
  image       = var.image
  server_type = var.server_type
  location    = var.location
  backups     = var.enable_backups
  ssh_keys    = [hcloud_ssh_key.operator.id]

  network {
    network_id = hcloud_network.main.id
  }

  public_net {
    ipv4_enabled = true
    ipv4         = hcloud_primary_ip.gateway_v4.id
    ipv6_enabled = true
  }

  user_data = templatefile("${path.module}/templates/cloud-init.yaml", {
    gateway_image       = var.gateway_image
    host_port           = var.host_port
    openrouter_base_url = var.openrouter_base_url
    domain              = var.domain_name
    acme_email          = var.acme_email
  })
}

# Firewall attachment lives outside the server resource so the firewall
# can exist (and be reviewed) without the server depending on it inline.
resource "hcloud_firewall_attachment" "gateway" {
  firewall_id = hcloud_firewall.gateway.id
  server_ids  = [hcloud_server.gateway.id]
}
