# Spec 11, PB1.4: the nightly audit runs on RDS.
#
# - The wt-v1-audit task runs `wt audit --fail-on warning` on the deployed image (the migrate
#   tag, like the eval task) as the read-only wt_audit_login, with the eval task's network
#   settings. Its JSON report goes to the admin log group under audit/audit/<task id>.
# - EventBridge Scheduler starts it daily at 10:17 UTC.
# - An EventBridge rule emails the alarm topic when an audit task stops with a nonzero exit code
#   or fails to start, and a CloudWatch alarm does when the schedule cannot start the task.
#
# Its roles live here rather than in the iam module: each may do only what the audit needs.

variable "name" {
  type = string
}

variable "image" {
  description = "Image URI the audit runs, normally the migrate tag."
  type        = string
}

variable "cluster_name" {
  type = string
}

variable "subnet_ids" {
  description = "The eval task's subnets: public, with a public IP, since v1 has no NAT gateway."
  type        = list(string)
}

variable "security_group_id" {
  description = "The web security group, which the database admits and which may reach AWS APIs."
  type        = string
}

variable "log_group_name" {
  description = "The admin log group, where the report is written."
  type        = string
}

variable "log_group_arn" {
  type = string
}

variable "ecr_repository_arn" {
  type = string
}

variable "db" {
  type = object({
    host = string
    port = number
    name = string
  })
}

variable "secret_arns" {
  type = map(string)
}

variable "alarm_topic_arn" {
  type = string
}

variable "permissions_boundary_arn" {
  type = string
}

variable "pc_backup_bucket" {
  description = "The PC backups bucket (runbook §10). While set, the audit warns when its newest dump is over 36 hours old (Spec 11, PB1.3). Empty once the PC keeps no database."
  type        = string
  default     = ""
}

variable "schedule_expression" {
  type    = string
  default = "cron(17 10 * * ? *)"
}

data "aws_caller_identity" "current" {}
data "aws_region" "current" {}

locals {
  account     = data.aws_caller_identity.current.account_id
  region      = data.aws_region.current.region
  path        = "/${var.name}/"
  family      = "${var.name}-audit"
  cluster_arn = "arn:aws:ecs:${local.region}:${local.account}:cluster/${var.cluster_name}"
  backups_arn = "arn:aws:s3:::${var.pc_backup_bucket}"

  command = concat(
    ["wt", "audit", "--config-dir", "config", "--fail-on", "warning", "--check-links"],
    var.pc_backup_bucket == "" ? [] : ["--backup-bucket", var.pc_backup_bucket],
  )
}

# ---------------------------------------------------------------------------------------------
# Roles
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "ecs_tasks_trust" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account]
    }
  }
}

# Pulls the image, writes the report and injects only the audit login and the session secret,
# which configuration requires outside dev.
data "aws_iam_policy_document" "exec" {
  statement {
    sid       = "EcrAuth"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"] # this action has no resource-level permissions
  }
  statement {
    sid       = "PullImage"
    actions   = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer", "ecr:BatchCheckLayerAvailability"]
    resources = [var.ecr_repository_arn]
  }
  statement {
    sid       = "WriteLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${var.log_group_arn}:*"]
  }
  statement {
    sid     = "InjectAuditSecrets"
    actions = ["secretsmanager:GetSecretValue"]
    resources = [
      var.secret_arns["db_wt_audit_login"],
      var.secret_arns["session_secret"],
    ]
  }
}

resource "aws_iam_role" "exec" {
  name                 = "${var.name}-audit-exec"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "exec" {
  name   = "audit-exec"
  role   = aws_iam_role.exec.id
  policy = data.aws_iam_policy_document.exec.json
}

# What the running audit may call: list the PC backups, while they exist.
resource "aws_iam_role" "task" {
  name                 = "${var.name}-audit-task"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ecs_tasks_trust.json
}

data "aws_iam_policy_document" "task" {
  count = var.pc_backup_bucket == "" ? 0 : 1
  statement {
    sid       = "ListPcBackups"
    actions   = ["s3:ListBucket"]
    resources = [local.backups_arn]
    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = ["db/*"]
    }
  }
}

resource "aws_iam_role_policy" "task" {
  count  = var.pc_backup_bucket == "" ? 0 : 1
  name   = "audit-task"
  role   = aws_iam_role.task.id
  policy = data.aws_iam_policy_document.task[0].json
}

# EventBridge Scheduler starts the task and passes it its two roles, nothing else.
data "aws_iam_policy_document" "scheduler_trust" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["scheduler.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account]
    }
  }
}

data "aws_iam_policy_document" "scheduler" {
  statement {
    sid       = "RunTheAudit"
    actions   = ["ecs:RunTask"]
    resources = ["arn:aws:ecs:${local.region}:${local.account}:task-definition/${local.family}:*"]
    condition {
      test     = "ArnEquals"
      variable = "ecs:cluster"
      values   = [local.cluster_arn]
    }
  }
  statement {
    sid       = "PassTheAuditRoles"
    actions   = ["iam:PassRole"]
    resources = [aws_iam_role.exec.arn, aws_iam_role.task.arn]
    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "scheduler" {
  name                 = "${var.name}-audit-scheduler"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.scheduler_trust.json
}

resource "aws_iam_role_policy" "scheduler" {
  name   = "audit-scheduler"
  role   = aws_iam_role.scheduler.id
  policy = data.aws_iam_policy_document.scheduler.json
}

# ---------------------------------------------------------------------------------------------
# Task and schedule
# ---------------------------------------------------------------------------------------------

resource "aws_ecs_task_definition" "audit" {
  family                   = local.family
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = 256
  memory                   = 512
  execution_role_arn       = aws_iam_role.exec.arn
  task_role_arn            = aws_iam_role.task.arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  container_definitions = jsonencode([{
    name      = "audit"
    image     = var.image
    essential = true
    command   = local.command
    # No WT_TRANSCRIPT_BUCKET: this build reads objects locally only and refuses the key. The
    # S3 object store (PB1.2) adds it.
    environment = [
      { name = "WT_ENV", value = "prod" },
      { name = "WT_LOG", value = "info" },
      { name = "WT_DB_HOST", value = var.db.host },
      { name = "WT_DB_PORT", value = tostring(var.db.port) },
      { name = "WT_DB_NAME", value = var.db.name },
      { name = "WT_DB_USER", value = "wt_audit_login" },
    ]
    secrets = [
      { name = "WT_DB_PASSWORD", valueFrom = var.secret_arns["db_wt_audit_login"] },
      { name = "WT_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
    ]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = var.log_group_name
        awslogs-region        = local.region
        awslogs-stream-prefix = "audit"
      }
    }
  }])
}

resource "aws_scheduler_schedule" "audit" {
  name                         = "${var.name}-audit-nightly"
  description                  = "Runs wt audit against RDS every day (Spec 11, PB1.4)."
  schedule_expression          = var.schedule_expression
  schedule_expression_timezone = "UTC"

  flexible_time_window {
    mode = "OFF"
  }

  target {
    arn      = local.cluster_arn
    role_arn = aws_iam_role.scheduler.arn

    ecs_parameters {
      # Without a revision, the latest active one: a deploy moves the schedule with it.
      task_definition_arn = aws_ecs_task_definition.audit.arn_without_revision
      launch_type         = "FARGATE"
      task_count          = 1

      network_configuration {
        subnets          = var.subnet_ids
        security_groups  = [var.security_group_id]
        assign_public_ip = true # no NAT gateway in v1, as for the eval task
      }
    }

    # A failed nightly run alarms; the next night tries again.
    retry_policy {
      maximum_retry_attempts = 0
    }
  }
}

# ---------------------------------------------------------------------------------------------
# Alarms
# ---------------------------------------------------------------------------------------------

# Any audit task, scheduled or one-off, that stops with a nonzero exit code or never starts.
resource "aws_cloudwatch_event_rule" "audit_failed" {
  name        = "${var.name}-audit-failed"
  description = "The wt-v1-audit task stopped with a nonzero exit code or failed to start."
  event_pattern = jsonencode({
    source        = ["aws.ecs"]
    "detail-type" = ["ECS Task State Change"]
    detail = {
      clusterArn        = [local.cluster_arn]
      taskDefinitionArn = [{ prefix = "arn:aws:ecs:${local.region}:${local.account}:task-definition/${local.family}:" }]
      lastStatus        = ["STOPPED"]
      "$or" = [
        { containers = { exitCode = [{ "anything-but" = 0 }] } },
        { stopCode = ["TaskFailedToStart"] },
      ]
    }
  })
}

resource "aws_cloudwatch_event_target" "audit_failed" {
  rule      = aws_cloudwatch_event_rule.audit_failed.name
  target_id = "alarm-topic"
  arn       = var.alarm_topic_arn

  input_transformer {
    input_paths = {
      task   = "$.detail.taskArn"
      exit   = "$.detail.containers[0].exitCode"
      reason = "$.detail.stoppedReason"
    }
    input_template = "\"The nightly audit (${local.family}) stopped with exit code <exit>: <reason>. Task <task>. Its report is in the ${var.log_group_name} log group under audit/audit/<task id>. Exit 1 means a check has findings; see the runbook's audit alarm entry.\""
  }
}

# The schedule could not start the task at all, so no task state change is ever sent.
resource "aws_cloudwatch_metric_alarm" "audit_not_started" {
  alarm_name          = "${var.name}-audit-not-started"
  alarm_description   = "EventBridge Scheduler could not start the nightly audit task."
  namespace           = "AWS/Scheduler"
  metric_name         = "TargetErrorCount"
  dimensions          = { ScheduleGroup = aws_scheduler_schedule.audit.group_name }
  statistic           = "Sum"
  period              = 3600
  evaluation_periods  = 1
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  treat_missing_data  = "notBreaching"
  alarm_actions       = [var.alarm_topic_arn]
}

# The topic's default policy, plus EventBridge publishing for the rule above only.
data "aws_iam_policy_document" "alarm_topic" {
  statement {
    sid = "AccountOwner"
    principals {
      type        = "AWS"
      identifiers = ["*"]
    }
    actions = [
      "sns:GetTopicAttributes", "sns:SetTopicAttributes", "sns:AddPermission",
      "sns:RemovePermission", "sns:DeleteTopic", "sns:Subscribe",
      "sns:ListSubscriptionsByTopic", "sns:Publish",
    ]
    resources = [var.alarm_topic_arn]
    condition {
      test     = "StringEquals"
      variable = "AWS:SourceOwner"
      values   = [local.account]
    }
  }
  statement {
    sid = "AuditFailedRule"
    principals {
      type        = "Service"
      identifiers = ["events.amazonaws.com"]
    }
    actions   = ["sns:Publish"]
    resources = [var.alarm_topic_arn]
    condition {
      test     = "ArnEquals"
      variable = "aws:SourceArn"
      values   = [aws_cloudwatch_event_rule.audit_failed.arn]
    }
  }
}

resource "aws_sns_topic_policy" "alarm_topic" {
  arn    = var.alarm_topic_arn
  policy = data.aws_iam_policy_document.alarm_topic.json
}

output "task_definition_family" {
  value = aws_ecs_task_definition.audit.family
}

output "schedule_name" {
  value = aws_scheduler_schedule.audit.name
}

output "event_rule_name" {
  value = aws_cloudwatch_event_rule.audit_failed.name
}

output "event_rule_arn" {
  value = aws_cloudwatch_event_rule.audit_failed.arn
}

output "alarm_names" {
  value = [aws_cloudwatch_metric_alarm.audit_not_started.alarm_name]
}
