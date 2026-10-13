# Spec 01 service: ECR repository, ECS cluster, Fargate service, load balancer, ACM certificate,
# HTTPS listener. Health check on /healthz, HTTP redirects to HTTPS.
#
# Three task definitions run the same image:
# - web: the always-on service, connecting as wt_web_login. Its models container copies the ONNX
#   models from the models bucket into a task volume first (Spec 01: models are not baked into
#   the image), and wt-api starts only after that copy succeeds.
# - migrate: a one-off task for `wt db migrate`, connecting as wt_owner_login
# - admin: a one-off task connecting as the RDS master user, used only to bootstrap migrations
#   and create login roles (`wt db create-login`)
#
# DNS for the domain lives at Cloudflare and is managed by hand: the certificate's validation
# record and the app's CNAME are outputs to copy there.

variable "name" {
  type = string
}

variable "vpc_id" {
  type = string
}

variable "public_subnet_ids" {
  type = list(string)
}

variable "security_group_ids" {
  description = "Security group ids by role: alb, web."
  type        = map(string)
}

variable "domain_name" {
  description = "Hostname testers use, for example app.wheelturning.org."
  type        = string
}

variable "alternate_domain_names" {
  description = "Further hostnames the certificate covers, each pointed at the load balancer by a DNS-only CNAME."
  type        = list(string)
  default     = []
}

variable "image_tag" {
  description = "Image tag for the web service. Empty means no image has been pushed yet: the service runs zero tasks."
  type        = string
  default     = ""
}

variable "migrate_image_tag" {
  description = "Image tag for the migrate and admin tasks. Defaults to image_tag. deploy.yml sets it ahead of image_tag so migrations run before the new web revision takes traffic."
  type        = string
  default     = ""
}

variable "web_cpu" {
  type    = number
  default = 512
}

variable "web_memory" {
  type    = number
  default = 1024
}

variable "models" {
  description = "Model directory names inside the models bucket, copied into /models at task start."
  type = object({
    embed  = string
    rerank = string
  })
  default = {
    embed  = "bge-m3-int8"
    rerank = "bge-reranker-v2-m3-int8"
  }
}

variable "models_image" {
  description = "Image that copies the models from S3. Public, pinned."
  type        = string
  default     = "public.ecr.aws/aws-cli/aws-cli:2.31.0"
}

variable "llm" {
  description = "Language model settings for synthesis, verification and triage (Specs 04 and 05). The API key comes from Secrets Manager."
  type = object({
    model           = string
    workspace_id    = string
    real_data_ok    = bool
    monthly_cap_usd = number
  })
}

variable "health_check_grace_seconds" {
  description = "Time a new task has to copy and load the models before load balancer checks count."
  type        = number
  default     = 300
}

variable "web_port" {
  type    = number
  default = 8080
}

variable "web_exec_role_arn" {
  type = string
}

variable "admin_exec_role_arn" {
  type = string
}

variable "web_task_role_arn" {
  type = string
}

variable "log_group_names" {
  description = "Log group names by purpose: web, admin."
  type        = map(string)
}

variable "db" {
  description = "Database connection facts."
  type = object({
    host              = string
    port              = number
    name              = string
    master_username   = string
    master_secret_arn = string
  })
}

variable "secret_arns" {
  type = map(string)
}

variable "bucket_names" {
  type = map(string)
}

data "aws_region" "current" {}

locals {
  web_tag     = var.image_tag
  migrate_tag = coalesce(var.migrate_image_tag, var.image_tag, "none")
  region      = data.aws_region.current.region

  db_env = [
    { name = "WT_DB_HOST", value = var.db.host },
    { name = "WT_DB_PORT", value = tostring(var.db.port) },
    { name = "WT_DB_NAME", value = var.db.name },
  ]
  common_env = [
    { name = "WT_ENV", value = "prod" },
    { name = "WT_LOG", value = "info" },
    { name = "WT_TRANSCRIPT_BUCKET", value = var.bucket_names["transcripts"] },
  ]
}

# ---------------------------------------------------------------------------------------------
# Registry and cluster
# ---------------------------------------------------------------------------------------------

resource "aws_ecr_repository" "this" {
  name                 = var.name
  image_tag_mutability = "IMMUTABLE"
  force_delete         = true # lets terraform destroy remove the stack cleanly

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "AES256"
  }
}

resource "aws_ecr_lifecycle_policy" "this" {
  repository = aws_ecr_repository.this.name
  policy = jsonencode({
    rules = [{
      rulePriority = 1
      description  = "Keep the 20 most recent images"
      selection    = { tagStatus = "any", countType = "imageCountMoreThan", countNumber = 20 }
      action       = { type = "expire" }
    }]
  })
}

resource "aws_ecs_cluster" "this" {
  name = var.name

  setting {
    name  = "containerInsights"
    value = "disabled" # costs extra; logs and ALB metrics cover v1
  }
}

# ---------------------------------------------------------------------------------------------
# Task definitions
# ---------------------------------------------------------------------------------------------

resource "aws_ecs_task_definition" "web" {
  family                   = "${var.name}-web"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = var.web_cpu
  memory                   = var.web_memory
  execution_role_arn       = var.web_exec_role_arn
  task_role_arn            = var.web_task_role_arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  # Task storage shared by the two containers; gone when the task stops.
  volume {
    name = "models"
  }

  container_definitions = jsonencode([
    {
      name      = "models"
      image     = var.models_image
      essential = false
      command = [
        "s3", "sync", "s3://${var.bucket_names["models"]}/", "/models/", "--only-show-errors",
        "--exclude", "*",
        "--include", "${var.models.embed}/*",
        "--include", "${var.models.rerank}/*",
      ]
      mountPoints = [{ sourceVolume = "models", containerPath = "/models" }]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = var.log_group_names["web"]
          awslogs-region        = local.region
          awslogs-stream-prefix = "models"
        }
      }
    },
    {
      name         = "web"
      image        = "${aws_ecr_repository.this.repository_url}:${coalesce(local.web_tag, "none")}"
      essential    = true
      command      = ["wt-api"]
      portMappings = [{ containerPort = var.web_port, protocol = "tcp" }]
      dependsOn    = [{ containerName = "models", condition = "SUCCESS" }]
      mountPoints  = [{ sourceVolume = "models", containerPath = "/models", readOnly = true }]
      environment = concat(local.common_env, local.db_env, [
        { name = "WT_BIND", value = "0.0.0.0:${var.web_port}" },
        { name = "WT_DB_USER", value = "wt_web_login" },
        { name = "WT_MODEL_DIR", value = "/models" },
        { name = "WT_EMBED_MODEL", value = var.models.embed },
        { name = "WT_RERANK_MODEL", value = var.models.rerank },
        { name = "WT_LLM_MODEL", value = var.llm.model },
        { name = "ANTHROPIC_WORKSPACE_ID", value = var.llm.workspace_id },
        { name = "WT_LLM_REAL_DATA_OK", value = tostring(var.llm.real_data_ok) },
        { name = "WT_LLM_MONTHLY_CAP_USD", value = tostring(var.llm.monthly_cap_usd) },
        # The load balancer is the only way in, so its X-Forwarded-For names the client.
        { name = "WT_TRUST_FORWARDED_FOR", value = "true" },
      ])
      secrets = [
        { name = "WT_DB_PASSWORD", valueFrom = var.secret_arns["db_wt_web_login"] },
        { name = "WT_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
        { name = "ANTHROPIC_API_KEY", valueFrom = var.secret_arns["anthropic_api_key"] },
      ]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = var.log_group_names["web"]
          awslogs-region        = local.region
          awslogs-stream-prefix = "web"
        }
      }
    },
  ])
}

resource "aws_ecs_task_definition" "migrate" {
  family                   = "${var.name}-migrate"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = 256
  memory                   = 512
  execution_role_arn       = var.admin_exec_role_arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  container_definitions = jsonencode([{
    name      = "migrate"
    image     = "${aws_ecr_repository.this.repository_url}:${local.migrate_tag}"
    essential = true
    command   = ["wt", "db", "migrate"]
    environment = concat(local.common_env, local.db_env, [
      { name = "WT_DB_USER", value = "wt_owner_login" },
    ])
    secrets = [
      { name = "WT_DB_PASSWORD", valueFrom = var.secret_arns["db_wt_owner_login"] },
      { name = "WT_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
    ]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = var.log_group_names["admin"]
        awslogs-region        = local.region
        awslogs-stream-prefix = "migrate"
      }
    }
  }])
}

resource "aws_ecs_task_definition" "admin" {
  family                   = "${var.name}-admin"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = 256
  memory                   = 512
  execution_role_arn       = var.admin_exec_role_arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  container_definitions = jsonencode([{
    name      = "admin"
    image     = "${aws_ecr_repository.this.repository_url}:${local.migrate_tag}"
    essential = true
    # Overridden per run, for example ["wt", "db", "create-login", "--role", "wt_web", ...].
    command = ["wt", "db", "migrate"]
    environment = concat(local.common_env, local.db_env, [
      { name = "WT_DB_USER", value = var.db.master_username },
    ])
    secrets = [
      # RDS stores the master credentials as JSON; ECS extracts one key.
      { name = "WT_DB_PASSWORD", valueFrom = "${var.db.master_secret_arn}:password::" },
      { name = "WT_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
      { name = "WT_LOGIN_PASSWORD_WT_OWNER_LOGIN", valueFrom = var.secret_arns["db_wt_owner_login"] },
      { name = "WT_LOGIN_PASSWORD_WT_WEB_LOGIN", valueFrom = var.secret_arns["db_wt_web_login"] },
      { name = "WT_LOGIN_PASSWORD_WT_PIPELINE_LOGIN", valueFrom = var.secret_arns["db_wt_pipeline_login"] },
    ]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = var.log_group_names["admin"]
        awslogs-region        = local.region
        awslogs-stream-prefix = "admin"
      }
    }
  }])
}

# ---------------------------------------------------------------------------------------------
# Load balancer, certificate, listeners
# ---------------------------------------------------------------------------------------------

resource "aws_lb" "this" {
  name                       = var.name
  load_balancer_type         = "application"
  internal                   = false
  subnets                    = var.public_subnet_ids
  security_groups            = [var.security_group_ids["alb"]]
  drop_invalid_header_fields = true
  idle_timeout               = 60
}

resource "aws_lb_target_group" "web" {
  name                 = "${var.name}-web"
  vpc_id               = var.vpc_id
  target_type          = "ip"
  protocol             = "HTTP"
  port                 = var.web_port
  deregistration_delay = 30

  health_check {
    path                = "/healthz"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

resource "aws_acm_certificate" "this" {
  domain_name               = var.domain_name
  subject_alternative_names = var.alternate_domain_names
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

# Waits until the validation CNAME (output certificate_validation_records) exists at Cloudflare.
resource "aws_acm_certificate_validation" "this" {
  certificate_arn = aws_acm_certificate.this.arn

  timeouts {
    create = "2h"
  }
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.this.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate_validation.this.certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.web.arn
  }
}

resource "aws_lb_listener" "http_redirect" {
  load_balancer_arn = aws_lb.this.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"
    redirect {
      protocol    = "HTTPS"
      port        = "443"
      status_code = "HTTP_301"
    }
  }
}

# ---------------------------------------------------------------------------------------------
# Service
# ---------------------------------------------------------------------------------------------

resource "aws_ecs_service" "web" {
  name            = "${var.name}-web"
  cluster         = aws_ecs_cluster.this.id
  task_definition = aws_ecs_task_definition.web.arn
  launch_type     = "FARGATE"
  # No image yet means nothing to run.
  desired_count = local.web_tag == "" ? 0 : 1

  deployment_minimum_healthy_percent = 100
  deployment_maximum_percent         = 200
  health_check_grace_period_seconds  = var.health_check_grace_seconds

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  network_configuration {
    subnets          = var.public_subnet_ids
    security_groups  = [var.security_group_ids["web"]]
    assign_public_ip = true # no NAT gateway in v1; the security group allows no inbound but the ALB
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.web.arn
    container_name   = "web"
    container_port   = var.web_port
  }

  depends_on = [aws_lb_listener.https]
}

output "ecr_repository_url" {
  value = aws_ecr_repository.this.repository_url
}

output "ecr_repository_arn" {
  value = aws_ecr_repository.this.arn
}

output "cluster_name" {
  value = aws_ecs_cluster.this.name
}

output "service_name" {
  value = aws_ecs_service.web.name
}

output "task_definition_families" {
  value = {
    web     = aws_ecs_task_definition.web.family
    migrate = aws_ecs_task_definition.migrate.family
    admin   = aws_ecs_task_definition.admin.family
  }
}

output "alb_dns_name" {
  value = aws_lb.this.dns_name
}

output "alb_arn_suffix" {
  value = aws_lb.this.arn_suffix
}

output "target_group_arn_suffix" {
  value = aws_lb_target_group.web.arn_suffix
}

output "certificate_validation_records" {
  description = "Add each as a DNS-only CNAME at Cloudflare."
  value = [for o in aws_acm_certificate.this.domain_validation_options : {
    name  = o.resource_record_name
    type  = o.resource_record_type
    value = o.resource_record_value
  }]
}
