output "server_ipv4" {
  description = "Public IPv4 of the gateway server (point the domain A record here)."
  value       = hcloud_primary_ip.gateway_v4.ip_address
}

output "gateway_url" {
  description = "Where the gateway answers (HTTPS with domain_name, else HTTP on host_port)."
  value = var.domain_name != "" ? (
    "https://${var.domain_name}/health"
    ) : (
    "http://${hcloud_primary_ip.gateway_v4.ip_address}:${var.host_port}/health"
  )
}

output "ssh" {
  description = "Operator SSH target (root, key-only via the uploaded key)."
  value       = "ssh root@${hcloud_primary_ip.gateway_v4.ip_address}"
}

output "server_id" {
  description = "Hetzner server ID."
  value       = hcloud_server.gateway.id
}
