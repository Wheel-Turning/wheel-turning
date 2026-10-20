variable "region" {
  type    = string
  default = "us-east-2"
}

variable "zone_name" {
  type    = string
  default = "wheelturning.org"
}

variable "hostnames" {
  description = "The app's hostnames: envs/v1's domain_name and alternate_domain_names."
  type        = list(string)
  default     = ["app.wheelturning.org", "demo.wheelturning.org"]
}

variable "test_hostname" {
  type    = string
  default = "tunnel-test.wheelturning.org"
}

variable "app_via_tunnel" {
  description = "Stage T-B: point the app's hostnames at the tunnel instead of the load balancer. Set together with envs/v1's edge = \"tunnel_live\" (runbook 12, \"The Cloudflare Tunnel\")."
  type        = bool
  default     = false
}

variable "maintenance" {
  description = "Answer every request for the app's hostnames with the maintenance page (Spec 11, PB4.4; runbook 13, \"The maintenance page\")."
  type        = bool
  default     = false
}
