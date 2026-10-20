output "tunnel_id" {
  description = "For `cf tunnels token get` (runbook 12, \"The Cloudflare Tunnel\")."
  value       = module.edge.tunnel_id
}

output "app_target" {
  description = "Where app and demo point now: the load balancer or the tunnel."
  value       = module.edge.app_target
}
