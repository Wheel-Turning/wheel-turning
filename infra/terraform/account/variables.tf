variable "region" {
  description = "Home region for the project."
  type        = string
  default     = "us-east-2"
}

variable "monthly_budget_usd" {
  description = "Monthly cost budget for the whole account, in US dollars. Raised from 75 to 200 on October 13, 2026 for the beta on AWS (about $170 a month)."
  type        = number
  default     = 200
}

variable "alert_email" {
  description = "Address that receives budget and cost anomaly alerts."
  type        = string
  default     = "alexandercowen@gmail.com"
}

variable "anomaly_threshold_usd" {
  description = "Cost anomaly alerts fire when an anomaly's total impact reaches this many dollars."
  type        = number
  default     = 5
}

variable "existing_anomaly_monitor_arn" {
  description = "An account allows one AWS services anomaly monitor. If one already exists, pass its ARN and this stack subscribes to it instead of creating another."
  type        = string
  default     = null
}

variable "enable_anomaly_detection" {
  description = "Cost Anomaly Detection needs Cost Explorer enabled on the account first (a one-time console step)."
  type        = bool
  default     = true
}

variable "cloudtrail_retention_days" {
  description = "How long CloudTrail log files are kept in the trail bucket."
  type        = number
  default     = 365
}
