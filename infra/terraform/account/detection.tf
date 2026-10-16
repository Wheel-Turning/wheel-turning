# Spec 11, PB3.6: detection. GuardDuty, IAM Access Analyzer for external and unused access, and
# CloudTrail metric filters with alarms for root activity and IAM policy changes.
#
# The alarms publish to the v1 alarm topic (modules/observability), whose email subscription is
# already confirmed. Its ARN is built from its name rather than looked up, so this stack still
# plans if v1 is ever destroyed.

locals {
  alarm_topic_arn = coalesce(
    var.alarm_topic_arn,
    "arn:${data.aws_partition.current.partition}:sns:${var.region}:${local.account_id}:wt-v1-alarms",
  )
  security_metric_namespace = "WheelTurning/Security"
}

# ---------------------------------------------------------------------------------------------
# GuardDuty: the foundational sources (CloudTrail management events, VPC flow logs, DNS logs) and
# S3 data events, since the buckets hold transcripts and rights records. The optional sources
# that cost more and cover nothing this account runs (EKS, Lambda, runtime agents, malware scans)
# stay off.
# ---------------------------------------------------------------------------------------------

resource "aws_guardduty_detector" "this" {
  enable                       = true
  finding_publishing_frequency = "SIX_HOURS"
}

resource "aws_guardduty_detector_feature" "s3_data_events" {
  detector_id = aws_guardduty_detector.this.id
  name        = "S3_DATA_EVENTS"
  status      = "ENABLED"
}

resource "aws_guardduty_detector_feature" "off" {
  for_each = toset([
    "EKS_AUDIT_LOGS",
    "EBS_MALWARE_PROTECTION",
    "RDS_LOGIN_EVENTS",
    "LAMBDA_NETWORK_LOGS",
    "RUNTIME_MONITORING",
  ])

  detector_id = aws_guardduty_detector.this.id
  name        = each.key
  status      = "DISABLED"
}

# ---------------------------------------------------------------------------------------------
# IAM Access Analyzer: one analyzer for resources shared outside the account (free) and one for
# unused roles, keys and permissions (billed per role and user analyzed).
# ---------------------------------------------------------------------------------------------

resource "aws_accessanalyzer_analyzer" "external" {
  analyzer_name = "wt-external-access"
  type          = "ACCOUNT"
}

resource "aws_accessanalyzer_analyzer" "unused" {
  analyzer_name = "wt-unused-access"
  type          = "ACCOUNT_UNUSED_ACCESS"

  configuration {
    unused_access {
      unused_access_age = var.unused_access_age_days
    }
  }
}

# ---------------------------------------------------------------------------------------------
# CloudTrail to CloudWatch Logs, for the metric filters. The trail is multi-region, so console
# sign-ins and IAM calls (both logged in us-east-1) arrive in this one log group.
# ---------------------------------------------------------------------------------------------

resource "aws_cloudwatch_log_group" "cloudtrail" {
  name              = "/wt/cloudtrail"
  retention_in_days = var.cloudtrail_log_group_retention_days
}

data "aws_iam_policy_document" "cloudtrail_logs_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceArn"
      values   = [local.trail_arn]
    }
  }
}

resource "aws_iam_role" "cloudtrail_logs" {
  name               = "wt-cloudtrail-logs"
  assume_role_policy = data.aws_iam_policy_document.cloudtrail_logs_assume.json
}

data "aws_iam_policy_document" "cloudtrail_logs" {
  statement {
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.cloudtrail.arn}:log-stream:*"]
  }
}

resource "aws_iam_role_policy" "cloudtrail_logs" {
  name   = "deliver-to-log-group"
  role   = aws_iam_role.cloudtrail_logs.id
  policy = data.aws_iam_policy_document.cloudtrail_logs.json
}

# ---------------------------------------------------------------------------------------------
# Metric filters and alarms (CIS AWS Foundations 4.3 and 4.4). Each matching event adds 1; an
# alarm fires on any event in 5 minutes. Without events no data is published, which counts as
# not breaching.
# ---------------------------------------------------------------------------------------------

locals {
  security_alarms = {
    "root-activity" = {
      description = "The root user signed in or made an API call. Nobody should use root; check who did and why in CloudTrail at once."
      pattern     = "{ $.userIdentity.type = \"Root\" && $.userIdentity.invokedBy NOT EXISTS && $.eventType != \"AwsServiceEvent\" }"
    }
    "iam-policy-change" = {
      description = "An IAM policy was created, changed, attached or detached. Expected during a Terraform apply; otherwise check who made it in CloudTrail."
      pattern = join(" || ", [for name in [
        "DeleteGroupPolicy", "DeleteRolePolicy", "DeleteUserPolicy",
        "PutGroupPolicy", "PutRolePolicy", "PutUserPolicy",
        "CreatePolicy", "DeletePolicy", "CreatePolicyVersion", "DeletePolicyVersion",
        "SetDefaultPolicyVersion",
        "AttachRolePolicy", "DetachRolePolicy", "AttachUserPolicy", "DetachUserPolicy",
        "AttachGroupPolicy", "DetachGroupPolicy",
      ] : "($.eventName = ${name})"])
    }
  }
}

resource "aws_cloudwatch_log_metric_filter" "security" {
  for_each = local.security_alarms

  name           = "wt-${each.key}"
  log_group_name = aws_cloudwatch_log_group.cloudtrail.name
  pattern        = each.value.pattern

  metric_transformation {
    name      = each.key
    namespace = local.security_metric_namespace
    value     = "1"
  }
}

resource "aws_cloudwatch_metric_alarm" "security" {
  for_each = local.security_alarms

  alarm_name          = "wt-account-${each.key}"
  alarm_description   = each.value.description
  namespace           = local.security_metric_namespace
  metric_name         = each.key
  statistic           = "Sum"
  period              = 300
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = [local.alarm_topic_arn]

  depends_on = [aws_cloudwatch_log_metric_filter.security]
}
