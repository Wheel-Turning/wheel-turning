variable "region" {
  description = "Home region for the project."
  type        = string
  default     = "us-east-2"
}

variable "noncurrent_version_retention_days" {
  description = "How long superseded state versions are kept before they expire."
  type        = number
  default     = 90
}
