# The nightly audit on RDS (Spec 11, PB1.4): what its workload roles may do at most, and what the
# deploy role needs to manage its schedule and failure rule. In their own policy, since the
# inline deploy policy is close to IAM's size limit.

# Joined into the workload boundary (main.tf).
data "aws_iam_policy_document" "workload_boundary_audit" {
  statement {
    sid       = "AuditListsPcBackups"
    actions   = ["s3:ListBucket"]
    resources = ["arn:aws:s3:::${var.pc_backup_bucket}"]
  }
  statement {
    sid       = "SchedulerRunsTheAudit"
    actions   = ["ecs:RunTask"]
    resources = ["arn:aws:ecs:${local.region}:${local.account}:task-definition/${var.name}-audit:*"]
  }
  statement {
    sid       = "SchedulerPassesTheAuditRoles"
    actions   = ["iam:PassRole"]
    resources = [local.role_arns]
    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ecs-tasks.amazonaws.com"]
    }
  }
}

data "aws_iam_policy_document" "deploy_audit" {
  statement {
    sid = "AuditFailedRule"
    actions = [
      "events:PutRule", "events:DeleteRule", "events:DescribeRule", "events:EnableRule",
      "events:DisableRule", "events:PutTargets", "events:RemoveTargets", "events:ListTargetsByRule",
      "events:TagResource", "events:UntagResource", "events:ListTagsForResource",
    ]
    resources = ["arn:aws:events:${local.region}:${local.account}:rule/${var.name}-*"]
  }
  statement {
    sid = "AuditSchedule"
    actions = [
      "scheduler:CreateSchedule", "scheduler:UpdateSchedule", "scheduler:DeleteSchedule",
      "scheduler:GetSchedule", "scheduler:TagResource", "scheduler:UntagResource",
      "scheduler:ListTagsForResource",
    ]
    resources = ["arn:aws:scheduler:${local.region}:${local.account}:schedule/default/${var.name}-*"]
  }
  statement {
    sid       = "PassWorkloadRolesToSchedulerOnly"
    actions   = ["iam:PassRole"]
    resources = [local.role_arns]
    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["scheduler.amazonaws.com"]
    }
  }
}

resource "aws_iam_policy" "deploy_audit" {
  name        = "deploy-${var.name}-audit"
  path        = "/ci/"
  description = "The nightly audit's schedule and failure rule for the ${var.name} deploy role."
  policy      = data.aws_iam_policy_document.deploy_audit.json
}

resource "aws_iam_role_policy_attachment" "deploy_audit" {
  role       = aws_iam_role.deploy.name
  policy_arn = aws_iam_policy.deploy_audit.arn
}
