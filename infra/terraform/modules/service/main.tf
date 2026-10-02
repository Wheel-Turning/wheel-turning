# Spec 01 service: ECR repository, ECS cluster, Fargate service, load balancer, ACM certificate,
# HTTPS listener. Health check on /healthz, HTTP redirects to HTTPS.
#
# Two task definitions run the same image:
# - web: the always-on service, connecting as dg_web_login
# - migrate: a one-off task for `dg db migrate`, connecting as dg_owner_login
# - admin: a one-off task connecting as the RDS master user, used only to bootstrap migrations
#   and create login roles (`dg db create-login`)
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
    { name = "DG_DB_HOST", value = var.db.host },
    { name = "DG_DB_PORT", value = tostring(var.db.port) },
    { name = "DG_DB_NAME", value = var.db.name },
  ]
  common_env = [
    { name = "DG_ENV", value = "prod" },
    { name = "DG_LOG", value = "info" },
    { name = "DG_TRANSCRIPT_BUCKET", value = var.bucket_names["transcripts"] },
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

  container_definitions = jsonencode([{
    name         = "web"
    image        = "${aws_ecr_repository.this.repository_url}:${coalesce(local.web_tag, "none")}"
    essential    = true
    command      = ["dg-api"]
    portMappings = [{ containerPort = var.web_port, protocol = "tcp" }]
    environment = concat(local.common_env, local.db_env, [
      { name = "DG_BIND", value = "0.0.0.0:${var.web_port}" },
      { name = "DG_DB_USER", value = "dg_web_login" },
    ])
    secrets = [
      { name = "DG_DB_PASSWORD", valueFrom = var.secret_arns["db_dg_web_login"] },
      { name = "DG_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
    ]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = var.log_group_names["web"]
        awslogs-region        = local.region
        awslogs-stream-prefix = "web"
      }
    }
  }])
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
    command   = ["dg", "db", "migrate"]
    environment = concat(local.common_env, local.db_env, [
      { name = "DG_DB_USER", value = "dg_owner_login" },
    ])
    secrets = [
      { name = "DG_DB_PASSWORD", valueFrom = var.secret_arns["db_dg_owner_login"] },
      { name = "DG_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
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
    # Overridden per run, for example ["dg", "db", "create-login", "--role", "dg_web", ...].
    command = ["dg", "db", "migrate"]
    environment = concat(local.common_env, local.db_env, [
      { name = "DG_DB_USER", value = var.db.master_username },
    ])
    secrets = [
      # RDS stores the master credentials as JSON; ECS extracts one key.
      { name = "DG_DB_PASSWORD", valueFrom = "${var.db.master_secret_arn}:password::" },
      { name = "DG_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
      { name = "DG_LOGIN_PASSWORD_DG_OWNER_LOGIN", valueFrom = var.secret_arns["db_dg_owner_login"] },
      { name = "DG_LOGIN_PASSWORD_DG_WEB_LOGIN", valueFrom = var.secret_arns["db_dg_web_login"] },
      { name = "DG_LOGIN_PASSWORD_DG_PIPELINE_LOGIN", valueFrom = var.secret_arns["db_dg_pipeline_login"] },
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
  domain_name       = var.domain_name
  validation_method = "DNS"

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
  health_check_grace_period_seconds  = 30

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

output "certificate_validation_records" {
  description = "Add each as a DNS-only CNAME at Cloudflare."
  value = [for o in aws_acm_certificate.this.domain_validation_options : {
    name  = o.resource_record_name
    type  = o.resource_record_type
    value = o.resource_record_value
  }]
}
