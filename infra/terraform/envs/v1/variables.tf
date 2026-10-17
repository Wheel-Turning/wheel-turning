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
# The database went from db.t4g.medium (4 GB) to db.t4g.small (2 GB) on October 17, 2026. Since
# migration 0043 the vector index is half precision (372 MB). Searches read about 1.1 to 1.2 GB:
# that index, the passage heap that full-text ranking reads, and the full-text index. On a copy
# of the 143,223 servable passages with Postgres held to 1.6 GB, a second pass over 242
# questions read nothing from disk, and the vector search p95 was 79 ms against 66 ms at 3.6 GB.
# At 1.1 GB it began reading from disk. The CPU is the same: 2 vCPUs and the same credit rate.
# Go back to db.t4g.medium when servable passages pass about 190,000, or when ReadIOPS climbs
# during questions.
variable "db_instance_class" {
  type    = string
  default = "db.t4g.small"
}

variable "db_deletion_protection" {
  description = "On since the October 2026 destroy-and-rebuild test proved the stack destroys cleanly. Set false and apply before any deliberate destroy."
  type        = bool
  default     = true
}

variable "web_cpu" {
  type    = number
  default = 2048
}

variable "web_memory" {
  type    = number
  default = 8192
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
