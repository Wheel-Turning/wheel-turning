# Spec 01 observability: CloudWatch log groups with 30-day retention, and alarms on the load
# balancer 5xx rate, database CPU and free storage, and a scratch bucket that is not empty after
# 24 hours, plus two alarms that fire with no traffic at all: no healthy web task behind the load
# balancer, and a Route 53 health check that cannot reach /healthz over HTTPS from outside AWS.
# Alarms on the answer path read the web service's JSON log events (Spec 11, PB4.3). Alarms
# email the alert address, and the second address when set, through one SNS topic per region;
# the code's own alerts (WT_ALERT_TOPIC_ARN) use the us-east-2 topic. Runbook procedure 13 has an
# entry per alarm, and crates/wt-core/tests/runbook_alarms.rs keeps the two lists equal.
#
# Route 53 publishes health check metrics only in us-east-1, so that alarm and its topic live
# there, through the aws.us_east_1 provider.

variable "name" {
  type = string
}

variable "alert_email" {
  type = string
}

variable "second_alert_email" {
  description = "A second recipient of every alarm and alert (Spec 11, PB4.2). Empty adds none."
  type        = string
  default     = ""
}

variable "log_retention_days" {
  type    = number
  default = 30
}

variable "alb_arn_suffix" {
  type = string
}

variable "target_group_arn_suffix" {
  type = string
}

variable "health_check_fqdn" {
  description = "Hostname the external health check requests /healthz from, over HTTPS."
  type        = string
}

variable "db_identifier" {
  type = string
}

variable "scratch_bucket" {
  type = string
}

variable "db_free_storage_threshold_bytes" {
  description = "Alarm when free database storage drops below this. Default 2 GiB of the 20 GiB volume."
  type        = number
  default     = 2147483648
}

resource "aws_cloudwatch_log_group" "this" {
  for_each          = toset(["web", "admin", "batch"])
  name              = "/${var.name}/${each.key}"
  retention_in_days = var.log_retention_days
}

resource "aws_sns_topic" "alarms" {
  name = "${var.name}-alarms"
}

# AWS emails a confirmation link; alarms are delivered once it is clicked.
resource "aws_sns_topic_subscription" "email" {
  topic_arn = aws_sns_topic.alarms.arn
  protocol  = "email"
  endpoint  = var.alert_email
}

resource "aws_sns_topic_subscription" "second_email" {
  count     = var.second_alert_email == "" ? 0 : 1
  topic_arn = aws_sns_topic.alarms.arn
  protocol  = "email"
  endpoint  = var.second_alert_email
}

resource "aws_cloudwatch_metric_alarm" "alb_5xx_rate" {
  alarm_name          = "${var.name}-alb-5xx-rate"
  alarm_description   = "More than 5% of requests returned a 5xx over 10 minutes."
  comparison_operator = "GreaterThanThreshold"
  threshold           = 5
  evaluation_periods  = 2
  datapoints_to_alarm = 2
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]

  metric_query {
    id          = "rate"
    expression  = "IF(requests > 0, 100 * (elb5xx + target5xx) / requests, 0)"
    label       = "5xx percent"
    return_data = true
  }

  metric_query {
    id = "requests"
    metric {
      namespace   = "AWS/ApplicationELB"
      metric_name = "RequestCount"
      dimensions  = { LoadBalancer = var.alb_arn_suffix }
      period      = 300
      stat        = "Sum"
    }
  }

  metric_query {
    id = "elb5xx"
    metric {
      namespace   = "AWS/ApplicationELB"
      metric_name = "HTTPCode_ELB_5XX_Count"
      dimensions  = { LoadBalancer = var.alb_arn_suffix }
      period      = 300
      stat        = "Sum"
    }
  }

  metric_query {
    id = "target5xx"
    metric {
      namespace   = "AWS/ApplicationELB"
      metric_name = "HTTPCode_Target_5XX_Count"
      dimensions  = { LoadBalancer = var.alb_arn_suffix }
      period      = 300
      stat        = "Sum"
    }
  }
}

resource "aws_cloudwatch_metric_alarm" "db_cpu" {
  alarm_name          = "${var.name}-db-cpu"
  alarm_description   = "Database CPU above 80% for 15 minutes."
  namespace           = "AWS/RDS"
  metric_name         = "CPUUtilization"
  dimensions          = { DBInstanceIdentifier = var.db_identifier }
  statistic           = "Average"
  period              = 300
  evaluation_periods  = 3
  comparison_operator = "GreaterThanThreshold"
  threshold           = 80
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
}

resource "aws_cloudwatch_metric_alarm" "db_free_storage" {
  alarm_name          = "${var.name}-db-free-storage"
  alarm_description   = "Database free storage below the threshold."
  namespace           = "AWS/RDS"
  metric_name         = "FreeStorageSpace"
  dimensions          = { DBInstanceIdentifier = var.db_identifier }
  statistic           = "Minimum"
  period              = 300
  evaluation_periods  = 1
  comparison_operator = "LessThanThreshold"
  threshold           = var.db_free_storage_threshold_bytes
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
}

# S3 publishes NumberOfObjects once a day. Objects expire one day after creation, so a count above
# zero on two consecutive daily datapoints means something has sat in scratch for over 24 hours.
resource "aws_cloudwatch_metric_alarm" "scratch_not_empty" {
  alarm_name          = "${var.name}-scratch-not-empty"
  alarm_description   = "The scratch bucket has held objects for more than 24 hours. Audio must not persist."
  namespace           = "AWS/S3"
  metric_name         = "NumberOfObjects"
  dimensions          = { BucketName = var.scratch_bucket, StorageType = "AllStorageTypes" }
  statistic           = "Average"
  period              = 86400
  evaluation_periods  = 2
  datapoints_to_alarm = 2
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
}

# The 5xx alarm stays quiet when nobody is using the app. This one does not: the target group
# reports zero healthy hosts whenever the web task is down or failing /healthz.
resource "aws_cloudwatch_metric_alarm" "no_healthy_web_task" {
  alarm_name          = "${var.name}-no-healthy-web-task"
  alarm_description   = "No healthy web task behind the load balancer for 5 minutes."
  namespace           = "AWS/ApplicationELB"
  metric_name         = "HealthyHostCount"
  dimensions          = { LoadBalancer = var.alb_arn_suffix, TargetGroup = var.target_group_arn_suffix }
  statistic           = "Maximum"
  period              = 60
  evaluation_periods  = 5
  datapoints_to_alarm = 5
  comparison_operator = "LessThanThreshold"
  threshold           = 1
  treat_missing_data  = "breaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
}

# Checks the path a tester takes: public DNS, the certificate, the load balancer and the app.
# Three checker regions, the minimum, keep the request rate on /healthz low.
resource "aws_route53_health_check" "healthz" {
  fqdn              = var.health_check_fqdn
  type              = "HTTPS"
  port              = 443
  resource_path     = "/healthz"
  request_interval  = 30
  failure_threshold = 3
  regions           = ["us-east-1", "us-west-2", "eu-west-1"]

  tags = {
    Name = "${var.name}-healthz"
  }
}

resource "aws_sns_topic" "alarms_us_east_1" {
  provider = aws.us_east_1
  name     = "${var.name}-alarms"
}

# A second subscription, so a second confirmation email.
resource "aws_sns_topic_subscription" "email_us_east_1" {
  provider  = aws.us_east_1
  topic_arn = aws_sns_topic.alarms_us_east_1.arn
  protocol  = "email"
  endpoint  = var.alert_email
}

resource "aws_sns_topic_subscription" "second_email_us_east_1" {
  count     = var.second_alert_email == "" ? 0 : 1
  provider  = aws.us_east_1
  topic_arn = aws_sns_topic.alarms_us_east_1.arn
  protocol  = "email"
  endpoint  = var.second_alert_email
}

resource "aws_cloudwatch_metric_alarm" "healthz_unreachable" {
  provider            = aws.us_east_1
  alarm_name          = "${var.name}-healthz-unreachable"
  alarm_description   = "https://${var.health_check_fqdn}/healthz has failed the Route 53 health check for 3 minutes."
  namespace           = "AWS/Route53"
  metric_name         = "HealthCheckStatus"
  dimensions          = { HealthCheckId = aws_route53_health_check.healthz.id }
  statistic           = "Minimum"
  period              = 60
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  comparison_operator = "LessThanThreshold"
  threshold           = 1
  treat_missing_data  = "breaching"
  alarm_actions       = [aws_sns_topic.alarms_us_east_1.arn]
  ok_actions          = [aws_sns_topic.alarms_us_east_1.arn]
}

# ---------------------------------------------------------------------------------------------
# Spec 11, PB4.3: alarms on the answer path, from the web service's structured log events. Logs
# are one JSON object per line with the fields at the top level, and no event carries question
# text. A metric gets a datapoint only when an event matches, so a quiet hour is missing data,
# which counts as not breaching.
# ---------------------------------------------------------------------------------------------

locals {
  answer_namespace = "WheelTurning/${var.name}"

  # Metric name => filter pattern; each matching line adds 1, or its `ms` for AskMilliseconds.
  answer_metrics = {
    LlmCalls        = "{ ($.event = \"llm_call_succeeded\") || ($.event = \"llm_call_failed\") }"
    LlmCallFailures = "{ $.event = \"llm_call_failed\" }"
    LlmCapReached   = "{ $.event = \"llm_cap_reached\" }"
    AskMilliseconds = "{ $.event = \"ask_completed\" }"
    # PB2.4 and PB2.1's events: one summary_settled and one ranker_served per answer.
    SummariesSettled = "{ $.event = \"summary_settled\" }"
    SummariesFailed  = "{ ($.event = \"summary_settled\") && ($.summary_status != \"shown\") && ($.summary_status != \"not_attempted\") }"
    RankerFallback   = "{ ($.event = \"ranker_served\") && ($.ranker = \"none (fallback)\") }"
  }
}

resource "aws_cloudwatch_log_metric_filter" "answer" {
  for_each       = local.answer_metrics
  name           = "${var.name}-${each.key}"
  log_group_name = aws_cloudwatch_log_group.this["web"].name
  pattern        = each.value

  metric_transformation {
    name      = each.key
    namespace = local.answer_namespace
    value     = each.key == "AskMilliseconds" ? "$.ms" : "1"
    unit      = each.key == "AskMilliseconds" ? "Milliseconds" : "Count"
  }
}

resource "aws_cloudwatch_metric_alarm" "model_failures" {
  alarm_name          = "${var.name}-model-failures"
  alarm_description   = "More than 20% of language-model calls failed in 15 minutes, with at least 5 calls. Runbook 13."
  comparison_operator = "GreaterThanThreshold"
  threshold           = 20
  evaluation_periods  = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]

  metric_query {
    id          = "rate"
    expression  = "IF(calls >= 5, 100 * FILL(failures, 0) / calls, 0)"
    label       = "Failed calls percent"
    return_data = true
  }

  metric_query {
    id = "calls"
    metric {
      namespace   = local.answer_namespace
      metric_name = "LlmCalls"
      period      = 900
      stat        = "Sum"
    }
  }

  metric_query {
    id = "failures"
    metric {
      namespace   = local.answer_namespace
      metric_name = "LlmCallFailures"
      period      = 900
      stat        = "Sum"
    }
  }

  depends_on = [aws_cloudwatch_log_metric_filter.answer]
}

resource "aws_cloudwatch_metric_alarm" "cap_reached" {
  alarm_name          = "${var.name}-cap-reached"
  alarm_description   = "The monthly language-model spend cap refused a call. Runbook 13."
  namespace           = local.answer_namespace
  metric_name         = "LlmCapReached"
  statistic           = "Sum"
  period              = 300
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]

  depends_on = [aws_cloudwatch_log_metric_filter.answer]
}

resource "aws_cloudwatch_metric_alarm" "summaries" {
  alarm_name          = "${var.name}-summaries"
  alarm_description   = "More than 30% of answers in an hour ended with a summary status other than shown or not_attempted, with at least 5 answers. Runbook 13."
  comparison_operator = "GreaterThanThreshold"
  threshold           = 30
  evaluation_periods  = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]

  metric_query {
    id          = "rate"
    expression  = "IF(settled >= 5, 100 * FILL(failed, 0) / settled, 0)"
    label       = "Summaries not shown percent"
    return_data = true
  }

  metric_query {
    id = "settled"
    metric {
      namespace   = local.answer_namespace
      metric_name = "SummariesSettled"
      period      = 3600
      stat        = "Sum"
    }
  }

  metric_query {
    id = "failed"
    metric {
      namespace   = local.answer_namespace
      metric_name = "SummariesFailed"
      period      = 3600
      stat        = "Sum"
    }
  }

  depends_on = [aws_cloudwatch_log_metric_filter.answer]
}

resource "aws_cloudwatch_metric_alarm" "ranker_fallback" {
  alarm_name          = "${var.name}-ranker-fallback"
  alarm_description   = "An answer in the last hour was served without the relevance check (ranker none (fallback)). Runbook 13."
  namespace           = local.answer_namespace
  metric_name         = "RankerFallback"
  statistic           = "Sum"
  period              = 3600
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]

  depends_on = [aws_cloudwatch_log_metric_filter.answer]
}

resource "aws_cloudwatch_metric_alarm" "slow_answers" {
  alarm_name          = "${var.name}-slow-answers"
  alarm_description   = "p95 time to the answer page above 8 seconds over an hour. Runbook 13."
  namespace           = local.answer_namespace
  metric_name         = "AskMilliseconds"
  extended_statistic  = "p95"
  period              = 3600
  evaluation_periods  = 1
  comparison_operator = "GreaterThanThreshold"
  threshold           = 8000
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]

  depends_on = [aws_cloudwatch_log_metric_filter.answer]
}

output "log_group_names" {
  value = { for k, g in aws_cloudwatch_log_group.this : k => g.name }
}

output "log_group_arns" {
  value = { for k, g in aws_cloudwatch_log_group.this : k => g.arn }
}

output "alarm_topic_arn" {
  value = aws_sns_topic.alarms.arn
}
