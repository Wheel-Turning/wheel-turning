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
variable "db_instance_class" {
  type    = string
  default = "db.t4g.medium"
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
