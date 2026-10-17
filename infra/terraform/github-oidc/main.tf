# GitHub Actions OIDC provider and the CI deploy role for the v1 stack.
#
# Applied by a person, never by CI, so CI can never widen its own permissions.
#
# Trust: only workflows in this repository (pinned by numeric owner and repo id) running on the
# main branch or a v* tag, with the sts.amazonaws.com audience.
#
# Permissions: what deploy.yml needs to push an image, run migrations, and plan and apply the v1
# stack. The action list started from IAM Policy Autopilot's analysis of the v1 plan JSON, then:
# - every write is scoped to the wt-v1 names or the /wt-v1/ IAM path
# - iam:PassRole is limited to /wt-v1/ roles and the ECS tasks and EC2 services
# - roles under /wt-v1/ must carry the workload permissions boundary, so CI cannot grant a
#   workload role more than the boundary allows
# - no secretsmanager:GetSecretValue: CI never reads secret values
# - no delete of the VPC, subnets, buckets, database, secrets, cluster or load balancer: CI
#   cannot destroy the stack; only a person can
# - added: ECR image push, one-off migrate tasks, reading migrate logs, the v1 state key, and the
#   updates and replacements a routine apply makes (new task definitions, batch host toggling)

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project = "wheel-turning"
      env     = "v1"
    }
  }
}

data "aws_caller_identity" "current" {}

locals {
  account = data.aws_caller_identity.current.account_id
  region  = var.region
  path    = "/${var.name}/"

  # Owner and repo names are wildcards; the numeric ids are what pin the repository.
  repo_subject = "repo:*@${var.github_owner_id}/*@${var.github_repo_id}"

  role_arns    = "arn:aws:iam::${local.account}:role${local.path}*"
  profile_arns = "arn:aws:iam::${local.account}:instance-profile${local.path}*"
  boundary_arn = "arn:aws:iam::${local.account}:policy${local.path}${var.name}-workload-boundary"
  # The records bucket lives in its own stack (infra/terraform/records), named by account only.
  bucket_arns = concat(
    [for b in ["transcripts", "scratch", "models"] : "arn:aws:s3:::${var.name}-${b}-${local.account}"],
    ["arn:aws:s3:::wt-records-${local.account}"],
  )
  secret_arns   = "arn:aws:secretsmanager:${local.region}:${local.account}:secret:${var.name}/*"
  log_group_arn = "arn:aws:logs:${local.region}:${local.account}:log-group:/${var.name}/*"
}

# ---------------------------------------------------------------------------------------------
# OIDC provider
# ---------------------------------------------------------------------------------------------

resource "aws_iam_openid_connect_provider" "github" {
  url            = "https://token.actions.githubusercontent.com"
  client_id_list = ["sts.amazonaws.com"]
}

# ---------------------------------------------------------------------------------------------
# Workload permissions boundary: the most any /wt-v1/ role may ever do, whatever CI writes into
# its policies. It covers exactly the services the workloads use, inside this account.
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "workload_boundary" {
  # The nightly audit's scheduler and backup listing (audit.tf, Spec 11 PB1.4).
  source_policy_documents = [data.aws_iam_policy_document.workload_boundary_audit.json]

  statement {
    sid = "WorkloadServices"
    actions = [
      "s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:AbortMultipartUpload", "s3:ListBucket",
      # Takedown deletes every version under a talk's prefix; records are written with a legal
      # hold (Spec 11, PB1.2). No workload policy grants a delete on the records bucket.
      "s3:ListBucketVersions", "s3:DeleteObjectVersion", "s3:PutObjectLegalHold",
      "secretsmanager:GetSecretValue",
      "logs:CreateLogStream", "logs:PutLogEvents", "logs:DescribeLogStreams",
      "ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer", "ecr:BatchCheckLayerAvailability",
    ]
    resources = concat(
      local.bucket_arns,
      [for b in local.bucket_arns : "${b}/*"],
      [local.secret_arns, "arn:aws:secretsmanager:${local.region}:${local.account}:secret:rds!*"],
      ["${local.log_group_arn}:*"],
      ["arn:aws:ecr:${local.region}:${local.account}:repository/${var.name}"],
    )
  }

  statement {
    sid       = "SendRightsMail"
    actions   = ["ses:SendEmail"]
    resources = ["arn:aws:ses:${local.region}:${local.account}:identity/*"]
    condition {
      test     = "StringLike"
      variable = "ses:FromAddress"
      values   = ["*@${var.mail_domain}"]
    }
  }
  # Alerts to the owner (Spec 11, PB4.2): the web and batch roles publish to the alarm topic.
  statement {
    sid       = "PublishAlerts"
    actions   = ["sns:Publish"]
    resources = ["arn:aws:sns:${local.region}:${local.account}:${var.name}-alarms"]
  }
  statement {
    sid       = "RegistryLogin"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"] # this action has no resource-level permissions
  }

  # What AmazonSSMManagedInstanceCore grants the batch host for Session Manager.
  statement {
    sid = "SessionManager"
    actions = [
      "ssm:DescribeAssociation", "ssm:GetDeployablePatchSnapshotForInstance", "ssm:GetDocument",
      "ssm:DescribeDocument", "ssm:GetManifest", "ssm:GetParameter", "ssm:GetParameters",
      "ssm:ListAssociations", "ssm:ListInstanceAssociations", "ssm:PutInventory",
      "ssm:PutComplianceItems", "ssm:PutConfigurePackageResult", "ssm:UpdateAssociationStatus",
      "ssm:UpdateInstanceAssociationStatus", "ssm:UpdateInstanceInformation",
      "ssmmessages:CreateControlChannel", "ssmmessages:CreateDataChannel",
      "ssmmessages:OpenControlChannel", "ssmmessages:OpenDataChannel",
      "ec2messages:AcknowledgeMessage", "ec2messages:DeleteMessage", "ec2messages:FailMessage",
      "ec2messages:GetEndpoint", "ec2messages:GetMessages", "ec2messages:SendReply",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_policy" "workload_boundary" {
  name        = "${var.name}-workload-boundary"
  path        = local.path
  description = "Permissions boundary for every ${var.name} workload role."
  policy      = data.aws_iam_policy_document.workload_boundary.json
}

# ---------------------------------------------------------------------------------------------
# Deploy role
# ---------------------------------------------------------------------------------------------

data "aws_iam_policy_document" "trust" {
  statement {
    sid     = "GitHubActionsMainAndVersionTags"
    actions = ["sts:AssumeRoleWithWebIdentity"]
    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github.arn]
    }
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }
    condition {
      test     = "StringLike"
      variable = "token.actions.githubusercontent.com:sub"
      values = [
        "${local.repo_subject}:ref:refs/heads/main",
        "${local.repo_subject}:ref:refs/tags/v*",
      ]
    }
  }
}

resource "aws_iam_role" "deploy" {
  name                 = "${var.name}-github-deploy"
  path                 = "/ci/"
  description          = "Assumed by GitHub Actions (main and v* tags) to deploy the ${var.name} stack."
  assume_role_policy   = data.aws_iam_policy_document.trust.json
  max_session_duration = 3600
}

data "aws_iam_policy_document" "deploy" {
  # ---- Terraform state: the v1 key only ------------------------------------------------------
  statement {
    sid       = "StateList"
    actions   = ["s3:ListBucket"]
    resources = ["arn:aws:s3:::${var.state_bucket}"]
    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = ["v1/*"]
    }
  }
  statement {
    sid       = "StateReadWrite"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["arn:aws:s3:::${var.state_bucket}/v1/*"]
  }

  # ---- Read-only calls Terraform makes to refresh state --------------------------------------
  statement {
    sid = "ReadForRefresh"
    actions = [
      "ec2:Describe*",
      "elasticloadbalancing:Describe*",
      "ecs:Describe*", "ecs:List*",
      "rds:Describe*", "rds:ListTagsForResource",
      "acm:DescribeCertificate", "acm:ListCertificates", "acm:ListTagsForCertificate",
      "logs:DescribeLogGroups", "logs:ListTagsForResource", "logs:ListTagsLogGroup",
      "cloudwatch:DescribeAlarms", "cloudwatch:ListTagsForResource",
      "ecr:GetAuthorizationToken",
    ]
    resources = ["*"] # these actions do not support resource-level permissions
  }
  statement {
    sid       = "ReadAmiParameter"
    actions   = ["ssm:GetParameter", "ssm:GetParameters"]
    resources = ["arn:aws:ssm:${local.region}::parameter/aws/service/deeplearning/*"]
  }

  # ---- Network (VPC, subnets, routes, endpoint, security groups) -----------------------------
  statement {
    sid = "NetworkCreateAndUpdate"
    actions = [
      "ec2:CreateVpc", "ec2:ModifyVpcAttribute",
      "ec2:CreateSubnet", "ec2:ModifySubnetAttribute",
      "ec2:CreateInternetGateway", "ec2:AttachInternetGateway",
      "ec2:CreateRouteTable", "ec2:CreateRoute", "ec2:ReplaceRoute", "ec2:AssociateRouteTable",
      "ec2:CreateVpcEndpoint", "ec2:ModifyVpcEndpoint",
      "ec2:CreateSecurityGroup",
      "ec2:AuthorizeSecurityGroupIngress", "ec2:AuthorizeSecurityGroupEgress",
      "ec2:RevokeSecurityGroupIngress", "ec2:RevokeSecurityGroupEgress",
      "ec2:ModifySecurityGroupRules",
      "ec2:UpdateSecurityGroupRuleDescriptionsIngress", "ec2:UpdateSecurityGroupRuleDescriptionsEgress",
      "ec2:CreateTags", "ec2:DeleteTags",
    ]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:RequestedRegion"
      values   = [local.region]
    }
  }

  # ---- Batch host: launch template and the instance (GPU, or CPU for the toggle test) --------
  statement {
    sid = "BatchLaunchTemplate"
    actions = [
      "ec2:CreateLaunchTemplate", "ec2:CreateLaunchTemplateVersion", "ec2:ModifyLaunchTemplate",
      "ec2:DeleteLaunchTemplate",
    ]
    resources = ["arn:aws:ec2:${local.region}:${local.account}:launch-template/*"]
  }
  statement {
    sid       = "BatchRunOnlyTheConfiguredInstanceType"
    actions   = ["ec2:RunInstances"]
    resources = ["arn:aws:ec2:${local.region}:${local.account}:instance/*"]
    condition {
      test     = "StringEquals"
      variable = "ec2:InstanceType"
      values   = var.batch_instance_types
    }
  }
  statement {
    sid     = "BatchRunSupportingResources"
    actions = ["ec2:RunInstances"]
    resources = [
      "arn:aws:ec2:${local.region}:${local.account}:subnet/*",
      "arn:aws:ec2:${local.region}:${local.account}:security-group/*",
      "arn:aws:ec2:${local.region}:${local.account}:network-interface/*",
      "arn:aws:ec2:${local.region}:${local.account}:volume/*",
      "arn:aws:ec2:${local.region}:${local.account}:launch-template/*",
      "arn:aws:ec2:${local.region}::image/*",
    ]
  }
  statement {
    sid       = "BatchStopStartTerminateTaggedOnly"
    actions   = ["ec2:TerminateInstances", "ec2:StopInstances", "ec2:StartInstances", "ec2:ModifyInstanceAttribute"]
    resources = ["arn:aws:ec2:${local.region}:${local.account}:instance/*"]
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/project"
      values   = ["wheel-turning"]
    }
  }

  # ---- Storage buckets ------------------------------------------------------------------------
  statement {
    sid = "Buckets"
    actions = [
      "s3:CreateBucket", "s3:ListBucket",
      "s3:GetBucket*", "s3:GetEncryptionConfiguration", "s3:GetLifecycleConfiguration",
      "s3:GetReplicationConfiguration", "s3:GetAccelerateConfiguration",
      "s3:PutBucketPolicy", "s3:PutBucketPublicAccessBlock", "s3:PutBucketOwnershipControls",
      "s3:PutEncryptionConfiguration", "s3:PutBucketVersioning", "s3:PutLifecycleConfiguration",
      "s3:PutBucketTagging",
    ]
    resources = local.bucket_arns
  }

  # ---- Secrets: create and describe, never read ----------------------------------------------
  statement {
    sid = "Secrets"
    actions = [
      "secretsmanager:CreateSecret", "secretsmanager:UpdateSecret", "secretsmanager:PutSecretValue",
      "secretsmanager:DescribeSecret", "secretsmanager:GetResourcePolicy",
      "secretsmanager:ListSecretVersionIds", "secretsmanager:TagResource", "secretsmanager:UntagResource",
    ]
    resources = [local.secret_arns]
  }
  statement {
    sid       = "RdsManagedMasterSecret"
    actions   = ["secretsmanager:CreateSecret", "secretsmanager:TagResource", "secretsmanager:DescribeSecret"]
    resources = ["arn:aws:secretsmanager:${local.region}:${local.account}:secret:rds!*"]
  }

  # ---- Database -------------------------------------------------------------------------------
  statement {
    sid = "Database"
    actions = [
      "rds:CreateDBInstance", "rds:ModifyDBInstance", "rds:RebootDBInstance",
      "rds:CreateDBParameterGroup", "rds:ModifyDBParameterGroup",
      "rds:CreateDBSubnetGroup", "rds:ModifyDBSubnetGroup",
      "rds:AddTagsToResource", "rds:RemoveTagsFromResource",
    ]
    resources = [
      "arn:aws:rds:${local.region}:${local.account}:db:${var.name}",
      "arn:aws:rds:${local.region}:${local.account}:pg:${var.name}-*",
      "arn:aws:rds:${local.region}:${local.account}:subgrp:${var.name}",
    ]
  }

  # ---- Container registry: manage the repo and push images -----------------------------------
  statement {
    sid = "Registry"
    actions = [
      "ecr:CreateRepository", "ecr:DescribeRepositories", "ecr:DescribeImages", "ecr:ListImages",
      "ecr:GetLifecyclePolicy", "ecr:PutLifecyclePolicy", "ecr:ListTagsForResource",
      "ecr:TagResource", "ecr:UntagResource",
      "ecr:PutImageScanningConfiguration", "ecr:PutImageTagMutability",
      "ecr:BatchCheckLayerAvailability", "ecr:InitiateLayerUpload", "ecr:UploadLayerPart",
      "ecr:CompleteLayerUpload", "ecr:PutImage", "ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer",
    ]
    resources = ["arn:aws:ecr:${local.region}:${local.account}:repository/${var.name}"]
  }

  # ---- ECS: cluster, service, task definitions, one-off migrate tasks ------------------------
  statement {
    sid = "EcsClusterAndService"
    actions = [
      "ecs:CreateCluster", "ecs:UpdateCluster", "ecs:UpdateClusterSettings",
      "ecs:CreateService", "ecs:UpdateService",
      "ecs:TagResource", "ecs:UntagResource",
    ]
    resources = [
      "arn:aws:ecs:${local.region}:${local.account}:cluster/${var.name}",
      "arn:aws:ecs:${local.region}:${local.account}:service/${var.name}/*",
    ]
  }
  statement {
    sid       = "EcsTaskDefinitions"
    actions   = ["ecs:RegisterTaskDefinition", "ecs:DeregisterTaskDefinition", "ecs:TagResource"]
    resources = ["*"] # RegisterTaskDefinition does not support resource-level permissions
  }
  statement {
    sid       = "EcsRunMigrations"
    actions   = ["ecs:RunTask"]
    resources = ["arn:aws:ecs:${local.region}:${local.account}:task-definition/${var.name}-migrate:*"]
    condition {
      test     = "ArnEquals"
      variable = "ecs:cluster"
      values   = ["arn:aws:ecs:${local.region}:${local.account}:cluster/${var.name}"]
    }
  }
  statement {
    sid       = "EcsWatchTasks"
    actions   = ["ecs:DescribeTasks"]
    resources = ["arn:aws:ecs:${local.region}:${local.account}:task/${var.name}/*"]
  }

  # ---- Load balancer and certificate ---------------------------------------------------------
  statement {
    sid = "LoadBalancer"
    actions = [
      "elasticloadbalancing:CreateLoadBalancer", "elasticloadbalancing:CreateTargetGroup",
      "elasticloadbalancing:CreateListener", "elasticloadbalancing:ModifyListener",
      "elasticloadbalancing:ModifyLoadBalancerAttributes", "elasticloadbalancing:ModifyTargetGroup",
      "elasticloadbalancing:ModifyTargetGroupAttributes", "elasticloadbalancing:ModifyListenerAttributes",
      "elasticloadbalancing:SetSecurityGroups", "elasticloadbalancing:SetSubnets",
      "elasticloadbalancing:AddTags", "elasticloadbalancing:RemoveTags",
    ]
    resources = [
      "arn:aws:elasticloadbalancing:${local.region}:${local.account}:loadbalancer/app/${var.name}/*",
      "arn:aws:elasticloadbalancing:${local.region}:${local.account}:targetgroup/${var.name}-web/*",
      "arn:aws:elasticloadbalancing:${local.region}:${local.account}:listener/app/${var.name}/*",
    ]
  }
  statement {
    sid       = "CertificateRequest"
    actions   = ["acm:RequestCertificate", "acm:AddTagsToCertificate"]
    resources = ["*"] # the certificate ARN does not exist until the request
  }
  statement {
    sid       = "CertificateReplace"
    actions   = ["acm:DeleteCertificate", "acm:RemoveTagsFromCertificate"]
    resources = ["arn:aws:acm:${local.region}:${local.account}:certificate/*"]
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/project"
      values   = ["wheel-turning"]
    }
  }

  # ---- IAM: workload roles under /wt-v1/, always inside the boundary -------------------------
  statement {
    sid       = "WorkloadRolesRead"
    actions   = ["iam:GetRole", "iam:GetRolePolicy", "iam:ListRolePolicies", "iam:ListAttachedRolePolicies", "iam:ListRoleTags", "iam:ListInstanceProfilesForRole", "iam:GetInstanceProfile", "iam:ListInstanceProfileTags", "iam:GetPolicy", "iam:GetPolicyVersion"]
    resources = [local.role_arns, local.profile_arns, local.boundary_arn, "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"]
  }
  statement {
    sid = "WorkloadRolesWriteWithinBoundary"
    actions = [
      "iam:CreateRole", "iam:PutRolePolicy", "iam:DeleteRolePolicy",
      "iam:AttachRolePolicy", "iam:DetachRolePolicy",
    ]
    resources = [local.role_arns]
    condition {
      test     = "StringEquals"
      variable = "iam:PermissionsBoundary"
      values   = [local.boundary_arn]
    }
  }
  statement {
    sid = "WorkloadRolesMetadata"
    actions = [
      "iam:TagRole", "iam:UntagRole", "iam:UpdateRole", "iam:UpdateRoleDescription",
      "iam:UpdateAssumeRolePolicy",
      "iam:CreateInstanceProfile", "iam:AddRoleToInstanceProfile", "iam:RemoveRoleFromInstanceProfile",
      "iam:TagInstanceProfile", "iam:UntagInstanceProfile",
    ]
    resources = [local.role_arns, local.profile_arns]
  }
  statement {
    sid       = "PassWorkloadRolesToEcsAndEc2Only"
    actions   = ["iam:PassRole"]
    resources = [local.role_arns]
    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ecs-tasks.amazonaws.com", "ec2.amazonaws.com"]
    }
  }

  # ---- Hard limits ----------------------------------------------------------------------------
  statement {
    sid    = "NeverRemoveTheBoundary"
    effect = "Deny"
    actions = [
      "iam:DeleteRolePermissionsBoundary", "iam:PutRolePermissionsBoundary",
      "iam:CreatePolicyVersion", "iam:SetDefaultPolicyVersion", "iam:DeletePolicy",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_role_policy" "deploy" {
  name   = "deploy-${var.name}"
  role   = aws_iam_role.deploy.id
  policy = data.aws_iam_policy_document.deploy.json
}

# Observability permissions sit in their own managed policy: the inline deploy policy is close to
# the 10,240-character limit IAM sets on a role's inline policies.
data "aws_iam_policy_document" "deploy_observability" {
  statement {
    sid = "Logs"
    actions = [
      "logs:CreateLogGroup", "logs:PutRetentionPolicy", "logs:TagLogGroup", "logs:TagResource",
      "logs:UntagResource", "logs:GetLogEvents", "logs:FilterLogEvents",
      # The answer-path metric filters (Spec 11, PB4.3).
      "logs:PutMetricFilter", "logs:DeleteMetricFilter",
    ]
    resources = [local.log_group_arn, "${local.log_group_arn}:*"]
  }
  statement {
    sid       = "ReadMetricFilters"
    actions   = ["logs:DescribeMetricFilters"]
    resources = ["*"] # Terraform's refresh lists filters across log groups
  }
  # The web ACL in front of the load balancer (Spec 11, PB3.6) is changed with the owner's
  # profile; the deploy role only reads it, so a deploy's plan can refresh it.
  statement {
    sid = "ReadWebAcl"
    actions = [
      "wafv2:GetWebACL", "wafv2:ListTagsForResource",
      "wafv2:GetLoggingConfiguration", "wafv2:ListResourcesForWebACL",
    ]
    resources = [
      "arn:aws:wafv2:${local.region}:${local.account}:regional/webacl/${var.name}/*",
      "arn:aws:elasticloadbalancing:${local.region}:${local.account}:loadbalancer/app/${var.name}/*",
    ]
  }
  # GetWebACLForResource is authorized against any web ACL (regional/webacl/*/*), since the
  # caller does not know which one is attached; the load balancer stays scoped to this stack.
  statement {
    sid     = "ReadWebAclForLoadBalancer"
    actions = ["wafv2:GetWebACLForResource"]
    resources = [
      "arn:aws:wafv2:${local.region}:${local.account}:regional/webacl/*/*",
      "arn:aws:elasticloadbalancing:${local.region}:${local.account}:loadbalancer/app/${var.name}/*",
    ]
  }
  statement {
    sid     = "Alarms"
    actions = ["cloudwatch:PutMetricAlarm", "cloudwatch:DeleteAlarms", "cloudwatch:TagResource", "cloudwatch:UntagResource"]
    # us-east-1 holds the alarm on the Route 53 health check, whose metrics exist only there.
    resources = [
      "arn:aws:cloudwatch:${local.region}:${local.account}:alarm:${var.name}-*",
      "arn:aws:cloudwatch:us-east-1:${local.account}:alarm:${var.name}-*",
    ]
  }
  statement {
    sid       = "HealthCheckCreate"
    actions   = ["route53:CreateHealthCheck"]
    resources = ["*"] # CreateHealthCheck does not support resource-level permissions
  }
  statement {
    sid = "HealthCheck"
    actions = [
      "route53:GetHealthCheck", "route53:UpdateHealthCheck", "route53:DeleteHealthCheck",
      "route53:ChangeTagsForResource", "route53:ListTagsForResource",
    ]
    resources = ["arn:aws:route53:::healthcheck/*"]
  }
  statement {
    sid = "AlarmTopic"
    actions = [
      "sns:CreateTopic", "sns:GetTopicAttributes", "sns:SetTopicAttributes", "sns:Subscribe",
      "sns:ListSubscriptionsByTopic", "sns:GetSubscriptionAttributes", "sns:ListTagsForResource",
      "sns:TagResource", "sns:UntagResource", "sns:Unsubscribe",
    ]
    resources = [
      "arn:aws:sns:${local.region}:${local.account}:${var.name}-alarms",
      "arn:aws:sns:${local.region}:${local.account}:${var.name}-alarms:*",
      "arn:aws:sns:us-east-1:${local.account}:${var.name}-alarms",
      "arn:aws:sns:us-east-1:${local.account}:${var.name}-alarms:*",
    ]
  }
}

resource "aws_iam_policy" "deploy_observability" {
  name        = "deploy-${var.name}-observability"
  path        = "/ci/"
  description = "Log groups, alarms, the alarm topics and the Route 53 health check for the ${var.name} deploy role."
  policy      = data.aws_iam_policy_document.deploy_observability.json
}

resource "aws_iam_role_policy_attachment" "deploy_observability" {
  role       = aws_iam_role.deploy.name
  policy_arn = aws_iam_policy.deploy_observability.arn
}

output "deploy_role_arn" {
  value = aws_iam_role.deploy.arn
}

output "workload_boundary_arn" {
  value = aws_iam_policy.workload_boundary.arn
}

output "oidc_provider_arn" {
  value = aws_iam_openid_connect_provider.github.arn
}
