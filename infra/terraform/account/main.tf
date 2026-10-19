# Account guardrails, applied before anything else: a monthly cost budget with email alerts and
# Cost Anomaly Detection. Neither costs anything to run.

locals {
  tags = {
    project = "wheel-turning"
    env     = "v1"
  }
}

provider "aws" {
  region = var.region

  default_tags {
    tags = local.tags
  }
}

# Cost Explorer, which owns anomaly detection, is served from us-east-1 only.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = local.tags
  }
}

resource "aws_budgets_budget" "monthly" {
  name         = "dharma-gates-monthly"
  budget_type  = "COST"
  limit_amount = format("%.2f", var.monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  notification {
    comparison_operator        = "GREATER_THAN"
    notification_type          = "ACTUAL"
    threshold                  = 50
    threshold_type             = "PERCENTAGE"
    subscriber_email_addresses = [var.alert_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    notification_type          = "ACTUAL"
    threshold                  = 80
    threshold_type             = "PERCENTAGE"
    subscriber_email_addresses = [var.alert_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    notification_type          = "FORECASTED"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    subscriber_email_addresses = [var.alert_email]
  }
}

resource "aws_ce_anomaly_monitor" "services" {
  count    = var.enable_anomaly_detection && var.existing_anomaly_monitor_arn == null ? 1 : 0
  provider = aws.us_east_1

  name              = "dharma-gates-services"
  monitor_type      = "DIMENSIONAL"
  monitor_dimension = "SERVICE"
}

resource "aws_ce_anomaly_subscription" "email" {
  count    = var.enable_anomaly_detection ? 1 : 0
  provider = aws.us_east_1

  name      = "dharma-gates-anomalies"
  frequency = "DAILY" # email subscribers receive daily or weekly summaries

  monitor_arn_list = [
    coalesce(var.existing_anomaly_monitor_arn, one(aws_ce_anomaly_monitor.services[*].arn)),
  ]

  subscriber {
    type    = "EMAIL"
    address = var.alert_email
  }

  threshold_expression {
    dimension {
      key           = "ANOMALY_TOTAL_IMPACT_ABSOLUTE"
      match_options = ["GREATER_THAN_OR_EQUAL"]
      values        = [tostring(var.anomaly_threshold_usd)]
    }
  }
}

# Roadmap UB3.9 (WA COST03-BP02): the default_tags every stack sets become cost allocation tags,
# so Cost Explorer can group spend by project and environment. A tag can be activated only after
# it has appeared on a billed resource, which both have since October 2026.
resource "aws_ce_cost_allocation_tag" "this" {
  for_each = toset(keys(local.tags))
  provider = aws.us_east_1

  tag_key = each.key
  status  = "Active"
}
