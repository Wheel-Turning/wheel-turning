variable "region" {
  type    = string
  default = "us-east-2"
}

variable "github_owner_id" {
  description = "Numeric id of the GitHub organisation. The trust policy pins ids, not names, so renaming the org or repo does not open or break it."
  type        = string
  default     = "337015568" # Wheel-Turning
}

variable "github_repo_id" {
  description = "Numeric id of the repository."
  type        = string
  default     = "1402061357" # wheel-turning
}

variable "name" {
  description = "Name prefix of the v1 stack this role deploys."
  type        = string
  default     = "wt-v1"
}

variable "state_bucket" {
  type    = string
  default = "wt-tfstate-<AWS_ACCOUNT_ID>"
}

variable "batch_instance_types" {
  description = "Instance types CI may launch for the batch host: the GPU default, and the small CPU type the M1 toggle test uses after AWS declined the GPU quota (Spec 01, GPU access)."
  type        = list(string)
  default     = ["g5.xlarge", "t3.small"]
}

variable "mail_domain" {
  description = "Domain workloads may send email from through SES (infra/terraform/account, email.tf)."
  type        = string
  default     = "wheelturning.org"
}

variable "pc_backup_bucket" {
  description = "The PC's nightly backup bucket (runbook §10), which the audit task may list (Spec 11, PB1.3)."
  type        = string
  default     = "wheel-turning-backups-<AWS_ACCOUNT_ID>-us-east-2"
}
