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

# Sizes are the smallest that run v1, chosen to keep the monthly cost low. Spec 01's defaults are
# db.t4g.medium with 50 GB and a 2 vCPU / 8 GB task; raise these when M5 loads the models.
variable "db_instance_class" {
  type    = string
  default = "db.t4g.micro"
}

variable "db_deletion_protection" {
  type    = bool
  default = false
}

variable "web_cpu" {
  type    = number
  default = 512
}

variable "web_memory" {
  type    = number
  default = 1024
}

variable "batch_host_enabled" {
  description = "Creates the GPU batch host when true, destroys it when false."
  type        = bool
  default     = false
}
