# The v1 environment: one module per row of the Spec 01 Terraform modules table.

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project = "wheel-turning"
      env     = "v1"
    }
  }
}

# Route 53 health check metrics exist only in us-east-1, so the alarm on them is created there.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      project = "wheel-turning"
      env     = "v1"
    }
  }
}

data "aws_caller_identity" "current" {}

locals {
  name = "wt-v1"
  # Created by infra/terraform/github-oidc, which is applied before this stack.
  workload_boundary_arn = "arn:aws:iam::${data.aws_caller_identity.current.account_id}:policy/${local.name}/${local.name}-workload-boundary"
}

module "network" {
  source = "../../modules/network"
  name   = local.name
}

module "storage" {
  source = "../../modules/storage"
  name   = local.name
}

module "secrets" {
  source = "../../modules/secrets"
  name   = local.name
}

module "database" {
  source              = "../../modules/database"
  name                = local.name
  private_subnet_ids  = module.network.private_subnet_ids
  security_group_id   = module.network.security_group_ids["db"]
  instance_class      = var.db_instance_class
  deletion_protection = var.db_deletion_protection
}

module "observability" {
  source      = "../../modules/observability"
  name        = local.name
  alert_email = var.alert_email
  # Spec 11, PB4.2: a second recipient, once the owner names one.
  second_alert_email = var.second_alert_email
  alb_arn_suffix     = module.service.alb_arn_suffix
  db_identifier      = module.database.identifier
  scratch_bucket     = module.storage.bucket_names["scratch"]

  target_group_arn_suffix = module.service.target_group_arn_suffix
  health_check_fqdn       = var.domain_name

  providers = {
    aws           = aws
    aws.us_east_1 = aws.us_east_1
  }
}

module "iam" {
  source                   = "../../modules/iam"
  name                     = local.name
  permissions_boundary_arn = local.workload_boundary_arn
  bucket_arns              = module.storage.bucket_arns
  secret_arns              = module.secrets.arns
  db_master_secret_arn     = module.database.master_secret_arn
  ecr_repository_arn       = module.service.ecr_repository_arn
  log_group_arns           = module.observability.log_group_arns
  mail_from                = var.mail_from
  alert_topic_arn          = module.observability.alarm_topic_arn
}

module "service" {
  source                 = "../../modules/service"
  name                   = local.name
  vpc_id                 = module.network.vpc_id
  public_subnet_ids      = module.network.public_subnet_ids
  security_group_ids     = module.network.security_group_ids
  domain_name            = var.domain_name
  alternate_domain_names = var.alternate_domain_names
  image_tag              = var.image_tag
  migrate_image_tag      = var.migrate_image_tag
  web_cpu                = var.web_cpu
  web_memory             = var.web_memory
  mail_from              = var.mail_from
  alert_topic_arn        = module.observability.alarm_topic_arn
  maintenance            = var.maintenance

  llm = {
    model           = var.llm_model
    workspace_id    = var.anthropic_workspace_id
    real_data_ok    = var.llm_real_data_ok
    monthly_cap_usd = var.llm_monthly_cap_usd
  }

  web_exec_role_arn   = module.iam.web_exec_role_arn
  admin_exec_role_arn = module.iam.admin_exec_role_arn
  web_task_role_arn   = module.iam.web_task_role_arn
  eval_task_role_arn  = module.iam.eval_task_role_arn
  log_group_names     = module.observability.log_group_names
  secret_arns         = module.secrets.arns
  bucket_names        = module.storage.bucket_names

  db = {
    host              = module.database.address
    port              = module.database.port
    name              = module.database.db_name
    master_username   = module.database.master_username
    master_secret_arn = module.database.master_secret_arn
  }
}

module "batch_host" {
  source                = "../../modules/batch_host"
  name                  = local.name
  enabled               = var.batch_host_enabled
  instance_type         = var.batch_host_instance_type
  subnet_id             = module.network.public_subnet_ids[0]
  security_group_id     = module.network.security_group_ids["batch"]
  instance_profile_name = module.iam.batch_instance_profile_name
}
