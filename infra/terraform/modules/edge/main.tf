# The Cloudflare side of the edge (docs/adr-edge-cost.md, "Follow-on", decision T1 (a)): the
# Cloudflare Tunnel the web task's cloudflared connects to, its routes, the app's DNS records and
# the maintenance page. Applied only by the owner, from infra/terraform/edge, with a Cloudflare API
# token that never reaches CI.
#
# - The tunnel is remotely managed: its routes live here, and cloudflared needs only the token,
#   which `cf tunnels token get` prints and runbook 12 copies into Secrets Manager. Cloudflare
#   generates the tunnel's secret, so no credential sits in this state.
# - The app's hostnames are proxied CNAMEs to the load balancer until `app_via_tunnel`, then to
#   the tunnel. A test hostname always points at the tunnel, so it can be tried first.
# - `maintenance = true` routes every request for the app's hostnames to a Worker that answers
#   503 with the maintenance page, /healthz included, so the health check alarm fires while it is
#   up (Spec 11, PB4.4). The Worker script stays deployed; only its routes come and go.
#
# The zone's other settings (TLS, the managed ruleset, the rate rule, the transform rule) are set
# by hand per runbook 12, "The edge".

variable "zone_name" {
  description = "The Cloudflare zone, for example wheelturning.org."
  type        = string
}

variable "name" {
  description = "Name of the tunnel and prefix of the Worker, for example wt-v1."
  type        = string
}

variable "hostnames" {
  description = "The app's hostnames, for example app.wheelturning.org and demo.wheelturning.org."
  type        = list(string)
}

variable "test_hostname" {
  description = "A hostname that always reaches the tunnel, for trying it before the app's hostnames move."
  type        = string
}

variable "origin" {
  description = "Where cloudflared sends requests inside the web task."
  type        = string
  default     = "http://localhost:8080"
}

variable "app_via_tunnel" {
  description = "Point the app's hostnames at the tunnel instead of the load balancer (stage T-B)."
  type        = bool
  default     = false
}

variable "alb_dns_name" {
  description = "The load balancer's DNS name, which the app's hostnames point at until app_via_tunnel."
  type        = string
  default     = ""

  validation {
    condition     = var.app_via_tunnel || var.alb_dns_name != ""
    error_message = "Until app_via_tunnel, the app's hostnames need the load balancer's DNS name."
  }
}

variable "maintenance" {
  description = "Answer every request for the app's hostnames with the maintenance page (Spec 11, PB4.4)."
  type        = bool
  default     = false
}

data "cloudflare_zone" "this" {
  filter = {
    name = var.zone_name
  }
}

locals {
  account_id = data.cloudflare_zone.this.account.id
  zone_id    = data.cloudflare_zone.this.zone_id
}

resource "cloudflare_zero_trust_tunnel_cloudflared" "web" {
  account_id = local.account_id
  name       = "${var.name}-web"
  config_src = "cloudflare"
}

resource "cloudflare_zero_trust_tunnel_cloudflared_config" "web" {
  account_id = local.account_id
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.web.id

  config = {
    ingress = concat(
      [for h in concat(var.hostnames, [var.test_hostname]) : { hostname = h, service = var.origin }],
      # Anything else that reaches the tunnel.
      [{ service = "http_status:404" }],
    )
  }
}

locals {
  tunnel_target = "${cloudflare_zero_trust_tunnel_cloudflared.web.id}.cfargotunnel.com"
}

resource "cloudflare_dns_record" "app" {
  for_each = toset(var.hostnames)
  zone_id  = local.zone_id
  name     = each.value
  type     = "CNAME"
  content  = var.app_via_tunnel ? local.tunnel_target : var.alb_dns_name
  proxied  = true
  ttl      = 1 # automatic, as for every proxied record
}

resource "cloudflare_dns_record" "test" {
  zone_id = local.zone_id
  name    = var.test_hostname
  type    = "CNAME"
  content = local.tunnel_target
  proxied = true
  ttl     = 1
}

resource "cloudflare_workers_script" "maintenance" {
  account_id         = local.account_id
  script_name        = "${var.name}-maintenance"
  main_module        = "worker.js"
  compatibility_date = "2026-10-01"
  content = templatefile("${path.module}/maintenance.js.tftpl", {
    page = jsonencode(trimspace(file("${path.module}/maintenance.html")))
  })
}

resource "cloudflare_workers_route" "maintenance" {
  for_each = var.maintenance ? toset(var.hostnames) : toset([])
  zone_id  = local.zone_id
  pattern  = "${each.value}/*"
  script   = cloudflare_workers_script.maintenance.script_name
}

output "tunnel_id" {
  description = "For `cf tunnels token get` (runbook 12, \"The Cloudflare Tunnel\")."
  value       = cloudflare_zero_trust_tunnel_cloudflared.web.id
}

output "app_target" {
  description = "Where the app's hostnames point now."
  value       = var.app_via_tunnel ? local.tunnel_target : var.alb_dns_name
}
