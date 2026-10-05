variable "region" {
  description = "Home region for the project."
  type        = string
  default     = "us-east-2"
}

variable "retention_mode" {
  description = "GOVERNANCE lets a person holding s3:BypassGovernanceRetention correct a mistaken record; COMPLIANCE lets nobody, root included, delete or shorten a record until its retention ends. COMPLIANCE cannot be undone for objects already written."
  type        = string
  default     = "GOVERNANCE"

  validation {
    condition     = contains(["GOVERNANCE", "COMPLIANCE"], var.retention_mode)
    error_message = "retention_mode is GOVERNANCE or COMPLIANCE."
  }
}

variable "retention_days" {
  description = "Default retention under the legal hold every record carries. The hold keeps records permanently; this period only matters if a hold is lifted. 2555 days is seven years."
  type        = number
  default     = 2555
}
