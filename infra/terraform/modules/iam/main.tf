# Spec 01 iam: least privilege per the Secrets, IAM and network rules.
# - web task role: reads the models bucket; cannot write transcripts or scratch
# - batch host role: reads and writes transcripts and scratch, reads models, reads the database,
#   Anthropic and YouTube secrets, and is reachable through SSM only
# - execution roles: pull the image, write logs, and inject the secrets each task needs at start
# The CI deploy role lives in infra/terraform/github-oidc so CI can never widen its own rights.
# Every role sits under the /<name>/ path so the deploy role's PassRole can be scoped to it.

variable "name" {
  type = string
}

variable "bucket_arns" {
  description = "Bucket ARNs by purpose: transcripts, scratch, models."
  type        = map(string)
}

variable "secret_arns" {
  description = "Secret ARNs by key, from the secrets module."
  type        = map(string)
}

variable "db_master_secret_arn" {
  type = string
}

variable "ecr_repository_arn" {
  type = string
}

variable "permissions_boundary_arn" {
  description = "Workload boundary from the github-oidc stack. Every role here must carry it, or the CI deploy role cannot manage it."
  type        = string
}

variable "log_group_arns" {
  description = "Log group ARNs by purpose: web, admin, batch."
  type        = map(string)
}

data "aws_caller_identity" "current" {}

locals {
  path = "/${var.name}/"
}

# ---------------------------------------------------------------------------------------------
# Trust policies
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "ecs_tasks_trust" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
    # Confused-deputy protection: only tasks in this account.
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [data.aws_caller_identity.current.account_id]
    }
  }
}

data "aws_iam_policy_document" "ec2_trust" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

# ---------------------------------------------------------------------------------------------
# Execution roles: one for the web service, one for one-off admin tasks (migrations, logins),
# so the always-on web service can never read the master or owner credentials.
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "exec_common" {
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
}

data "aws_iam_policy_document" "web_exec" {
  source_policy_documents = [data.aws_iam_policy_document.exec_common.json]
  statement {
    sid       = "WriteLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${var.log_group_arns["web"]}:*"]
  }
  statement {
    sid     = "InjectWebSecrets"
    actions = ["secretsmanager:GetSecretValue"]
    # ECS refuses to start a task whose secret has no value yet: the Anthropic key is set out of
    # band before the first deploy that uses it.
    resources = [
      var.secret_arns["db_wt_web_login"],
      var.secret_arns["session_secret"],
      var.secret_arns["anthropic_api_key"],
    ]
  }
}

data "aws_iam_policy_document" "admin_exec" {
  source_policy_documents = [data.aws_iam_policy_document.exec_common.json]
  statement {
    sid       = "WriteLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${var.log_group_arns["admin"]}:*"]
  }
  statement {
    sid     = "InjectAdminSecrets"
    actions = ["secretsmanager:GetSecretValue"]
    resources = [
      var.db_master_secret_arn,
      var.secret_arns["session_secret"],
      var.secret_arns["db_wt_owner_login"],
      var.secret_arns["db_wt_web_login"],
      var.secret_arns["db_wt_pipeline_login"],
      # The eval task asks questions through the language model, as the site does.
      var.secret_arns["anthropic_api_key"],
    ]
  }
}

resource "aws_iam_role" "web_exec" {
  name                 = "${var.name}-web-exec"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "web_exec" {
  name   = "web-exec"
  role   = aws_iam_role.web_exec.id
  policy = data.aws_iam_policy_document.web_exec.json
}

resource "aws_iam_role" "admin_exec" {
  name                 = "${var.name}-admin-exec"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "admin_exec" {
  name   = "admin-exec"
  role   = aws_iam_role.admin_exec.id
  policy = data.aws_iam_policy_document.admin_exec.json
}

# ---------------------------------------------------------------------------------------------
# Web task role: what the running web code may call.
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "web_task" {
  statement {
    sid       = "ListModels"
    actions   = ["s3:ListBucket"]
    resources = [var.bucket_arns["models"]]
  }
  statement {
    sid       = "ReadModels"
    actions   = ["s3:GetObject"]
    resources = ["${var.bucket_arns["models"]}/*"]
  }
}

resource "aws_iam_role" "web_task" {
  name                 = "${var.name}-web-task"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "web_task" {
  name   = "web-task"
  role   = aws_iam_role.web_task.id
  policy = data.aws_iam_policy_document.web_task.json
}

# ---------------------------------------------------------------------------------------------
# Eval task role: the evaluation runs on the site's models and writes its review page, which
# quotes transcripts, under eval-reviews/ in the transcripts bucket and nowhere else.
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "eval_task" {
  source_policy_documents = [data.aws_iam_policy_document.web_task.json]
  statement {
    sid       = "WriteReviewPages"
    actions   = ["s3:PutObject"]
    resources = ["${var.bucket_arns["transcripts"]}/eval-reviews/*"]
  }
}

resource "aws_iam_role" "eval_task" {
  name                 = "${var.name}-eval-task"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "eval_task" {
  name   = "eval-task"
  role   = aws_iam_role.eval_task.id
  policy = data.aws_iam_policy_document.eval_task.json
}

# ---------------------------------------------------------------------------------------------
# Batch host role.
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "batch" {
  statement {
    sid     = "ListBuckets"
    actions = ["s3:ListBucket"]
    resources = [
      var.bucket_arns["transcripts"],
      var.bucket_arns["scratch"],
      var.bucket_arns["models"],
    ]
  }
  statement {
    sid     = "ReadWriteTranscriptsAndScratch"
    actions = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:AbortMultipartUpload"]
    resources = [
      "${var.bucket_arns["transcripts"]}/*",
      "${var.bucket_arns["scratch"]}/*",
    ]
  }
  statement {
    sid       = "ReadModels"
    actions   = ["s3:GetObject"]
    resources = ["${var.bucket_arns["models"]}/*"]
  }
  statement {
    sid     = "ReadPipelineSecrets"
    actions = ["secretsmanager:GetSecretValue"]
    resources = [
      var.secret_arns["db_wt_pipeline_login"],
      var.secret_arns["anthropic_api_key"],
      var.secret_arns["youtube_api_key"],
    ]
  }
  statement {
    sid       = "WriteLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents", "logs:DescribeLogStreams"]
    resources = ["${var.log_group_arns["batch"]}:*"]
  }
}

resource "aws_iam_role" "batch" {
  name                 = "${var.name}-batch"
  path                 = local.path
  permissions_boundary = var.permissions_boundary_arn
  assume_role_policy   = data.aws_iam_policy_document.ec2_trust.json
}

resource "aws_iam_role_policy" "batch" {
  name   = "batch"
  role   = aws_iam_role.batch.id
  policy = data.aws_iam_policy_document.batch.json
}

# Session Manager access instead of SSH.
resource "aws_iam_role_policy_attachment" "batch_ssm" {
  role       = aws_iam_role.batch.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "batch" {
  name = "${var.name}-batch"
  path = local.path
  role = aws_iam_role.batch.name
}

output "web_exec_role_arn" {
  value = aws_iam_role.web_exec.arn
}

output "admin_exec_role_arn" {
  value = aws_iam_role.admin_exec.arn
}

output "web_task_role_arn" {
  value = aws_iam_role.web_task.arn
}

output "eval_task_role_arn" {
  value = aws_iam_role.eval_task.arn
}

output "batch_instance_profile_name" {
  value = aws_iam_instance_profile.batch.name
}

output "role_path" {
  value = local.path
}
