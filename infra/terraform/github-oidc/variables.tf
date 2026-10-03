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
  default     = "dg-v1"
}

variable "state_bucket" {
  type    = string
  default = "dharma-gates-tfstate-904883674947"
}
