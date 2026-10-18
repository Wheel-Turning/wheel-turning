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
  default     = "alexandercowen@gmail.com"
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

variable "waf_block" {
  description = "Switch the load balancer's WAF rules from count to block. Spec 11 PB3.6: count for 7 days after the first apply, then set true once the sampled requests show no false positives."
  type        = bool
  default     = false
}

variable "pc_backup_bucket" {
  description = "The PC's nightly backup bucket (runbook §10), which the audit checks for a dump under 36 hours old (Spec 11, PB1.3). Set to \"\" once the PC keeps no database."
  type        = string
  default     = "wheel-turning-backups-904883674947-us-east-2"
}
