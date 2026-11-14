variable "region" {
  description = "AWS region for every v1 resource."
  type        = string
  default     = "us-east-2"
}

variable "domain_name" {
  description = "Hostname testers use. DNS is at Cloudflare and managed by hand."
  type        = string
  default     = "app.wheelturning.org"
}

variable "alternate_domain_names" {
  description = "Further hostnames that reach the same service. demo.wheelturning.org is the name in Spec 01 and the fetcher's user agent."
  type        = list(string)
  default     = ["demo.wheelturning.org"]
}

variable "alert_email" {
  description = "Receives CloudWatch alarm notifications."
  type        = string
  default     = "<OWNER_ALERT_EMAIL>"
}

variable "second_alert_email" {
  description = "A second address for alarms and alerts (Spec 11, PB4.2), confirmed by its own AWS email. Empty until the owner names one."
  type        = string
  default     = ""
}

variable "maintenance" {
  description = "true makes the load balancer answer every path with the fixed maintenance page (runbook 13). Set it for an outage or planned work, then false."
  type        = bool
  default     = false
}

variable "image_tag" {
  description = "Image tag the web service runs. Empty until the first image is pushed."
  type        = string
  default     = ""
}

variable "migrate_image_tag" {
  description = "Image tag for one-off migrate and admin tasks. Defaults to image_tag."
  type        = string
  default     = ""
}

# Spec 01's sizes, raised from the M1 minimum (db.t4g.micro, 0.5 vCPU / 1 GB) for the beta: the
# HNSW index over about 137,000 passages needs the database's memory, and the task holds the
# embedder and four reranker sessions.
#
# db.t4g.medium (4 GB) to db.t4g.small (2 GB) on October 17, 2026, to save $24 a month (docs/cost.md,
# "Database size"). Searches read the half-precision vector index (372 MB, migration 0043), the
# passage heap that full-text ranking reads (231 MB) and the full-text index. On a copy of the
# 143,223 servable passages, all of that stayed in memory only from about 1.5 GB of Postgres
# memory up. A small leaves Postgres about 1.3 GB, so expect about 1 MB of disk reads a
# question, roughly 0.1 to 0.15 s on gp3. The CPU is the same: 2 vCPUs at the same credit rate.
# Go back to db.t4g.medium if the page p95 in `wt report beta` passes 4.8 s, or once servable
# passages reach about 170,000.
variable "db_instance_class" {
  type    = string
  default = "db.t4g.small"
}

variable "db_deletion_protection" {
  description = "On since the October 2026 destroy-and-rebuild test proved the stack destroys cleanly. Set false and apply before any deliberate destroy."
  type        = bool
  default     = true
}

# 1 vCPU / 3 GB since October 17, 2026 (was 2 vCPU / 8 GB). With `[ranking] ranker = "model"` the
# task loads only the int8 embedder and used at most 0.88 GB on the beta, and one question's
# embedding takes 77 ms at the 95th percentile on 1 vCPU (docs/cost.md). Switching the ranker to "onnx" also
# loads rerank_sessions copies of the cross-encoder and needs 2048 / 8192 again.
variable "web_cpu" {
  type    = number
  default = 1024
}

variable "web_memory" {
  type    = number
  default = 3072
}

variable "llm_model" {
  description = "WT_LLM_MODEL for the web service; every stage falls back to it."
  type        = string
  default     = "claude-haiku-4-5"
}

variable "llm_stage_models" {
  description = "Per-stage models for the web service and the eval task (Spec 11, PB2.0): WT_LLM_MODEL_<STAGE>. A stage left out or empty falls back to llm_model."
  type = object({
    synth   = optional(string, "")
    verify  = optional(string, "")
    triage  = optional(string, "")
    excerpt = optional(string, "")
    rank    = optional(string, "")
  })
  default = {}
}

variable "anthropic_workspace_id" {
  description = "ANTHROPIC_WORKSPACE_ID. Not a secret; the API key is in Secrets Manager."
  type        = string
  default     = "wrkspc_01UH3bhoPidR6S5oJvidFSBS"
}

variable "llm_real_data_ok" {
  description = "WT_LLM_REAL_DATA_OK, set by the owner (runbook procedure 8)."
  type        = bool
  default     = true
}

variable "llm_monthly_cap_usd" {
  type    = number
  default = 60
}

variable "batch_host_enabled" {
  description = "Creates the GPU batch host when true, destroys it when false."
  type        = bool
  default     = false
}

variable "batch_host_instance_type" {
  description = "Batch host instance type. The default is the GPU type the pipeline needs; AWS declined the account's GPU quota, so v1 transcribes on the owner's own machine and this only runs for the toggle test with a small CPU type such as t3.small (Spec 01, GPU access)."
  type        = string
  default     = "g5.xlarge"
}

variable "mail_from" {
  description = "Sender of the removal request confirmation email. Its domain is the SES identity in infra/terraform/account."
  type        = string
  default     = "rights@wheelturning.org"
}

variable "cloudflare_only" {
  description = "Cloudflare proxies the app's hostnames and is the only way in: the listener needs the origin header and the security group admits only Cloudflare's ranges (docs/adr-edge-cost.md, runbook edge cutover). Set only after the hostnames are proxied and the transform rule exists."
  type        = bool
  default     = true
}

variable "alb_public_ipv4" {
  description = "Give the load balancer public IPv4 addresses. False saves two addresses ($7.30 a month) and needs cloudflare_only, since testers on IPv4 then reach the site only through Cloudflare."
  type        = bool
  default     = false

  validation {
    condition     = var.alb_public_ipv4 || var.cloudflare_only
    error_message = "alb_public_ipv4 = false needs cloudflare_only = true, or IPv4-only testers lose the site."
  }
}

variable "cloudflare_ranges" {
  description = "Cloudflare's published address ranges, https://www.cloudflare.com/ips-v4 and ips-v6, read October 17, 2026. Check them before each edge change."
  type = object({
    ipv4 = list(string)
    ipv6 = list(string)
  })
  default = {
    ipv4 = [
      "173.245.48.0/20", "103.21.244.0/22", "103.22.200.0/22", "103.31.4.0/22", "141.101.64.0/18",
      "108.162.192.0/18", "190.93.240.0/20", "188.114.96.0/20", "197.234.240.0/22", "198.41.128.0/17",
      "162.158.0.0/15", "104.16.0.0/13", "104.24.0.0/14", "172.64.0.0/13", "131.0.72.0/22",
    ]
    ipv6 = [
      "2400:cb00::/32", "2606:4700::/32", "2803:f800::/32", "2405:b500::/32", "2405:8100::/32",
      "2a06:98c0::/29", "2c0f:f248::/32",
    ]
  }
}

variable "edge" {
  description = <<-EOT
    What carries testers' traffic from Cloudflare (docs/adr-edge-cost.md, "Follow-on"; runbook 12,
    "The Cloudflare Tunnel"), in the order the stages run:
    - alb: the load balancer alone.
    - tunnel_test: the load balancer serves testers; the web task also runs cloudflared, reachable
      only on the edge stack's test hostname.
    - tunnel_live: the edge stack points the app's hostnames at the tunnel; the load balancer
      stays for a rollback. WT_FORWARDED_HOPS drops to 1.
    - tunnel: the load balancer, its certificate and its alarms are gone.
    The cloudflare_tunnel_token secret must hold a value before any tunnel stage.
  EOT
  type        = string
  default     = "tunnel_live"

  validation {
    condition     = contains(["alb", "tunnel_test", "tunnel_live", "tunnel"], var.edge)
    error_message = "edge is one of alb, tunnel_test, tunnel_live or tunnel."
  }
}

variable "db_dual_stack" {
  description = "Stage T-D1: IPv6 on the private subnets, two IPv6-only subnets for the web task, and the database in dual-stack mode. AWS says the database change may cause downtime: apply in a quiet hour with a snapshot taken and the maintenance page up (runbook 12)."
  type        = bool
  default     = false
}

variable "web_ipv6_only" {
  description = "Stage T-D2: the web task in the IPv6-only subnets with no public IPv4 address. Needs edge = tunnel and db_dual_stack."
  type        = bool
  default     = false

  validation {
    condition     = !var.web_ipv6_only || (var.edge == "tunnel" && var.db_dual_stack)
    error_message = "web_ipv6_only needs edge = \"tunnel\" and db_dual_stack = true."
  }
}

variable "pc_backup_bucket" {
  description = "The PC's nightly backup bucket (runbook §10), which the audit checks for a dump under 36 hours old (Spec 11, PB1.3). Set to \"\" once the PC keeps no database."
  type        = string
  default     = "wheel-turning-backups-<AWS_ACCOUNT_ID>-us-east-2"
}
