# Spec 01 service: ECR repository, ECS cluster, Fargate service, load balancer, ACM certificate,
# HTTPS listener. Health check on /healthz, HTTP redirects to HTTPS. With `tunnel`, a cloudflared
# sidecar carries the app's hostnames from Cloudflare instead, and with `load_balancer = false`
# the load balancer, its certificate and listeners are gone (the Cloudflare Tunnel follow-on in
# docs/adr-edge-cost.md).
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
#
# Edge (docs/adr-edge-cost.md): Cloudflare proxies the app's hostnames and is the only way in.
# With `cloudflare_only`, the HTTPS listener forwards only requests carrying the origin header,
# which a Cloudflare transform rule adds, and answers anything else with a 403; the security
# group admits only Cloudflare's ranges (envs/v1). Without `alb_public_ipv4` the load balancer has
# no public IPv4 address and Cloudflare reaches it over IPv6. Cloudflare's managed rules and rate
# rule replace the AWS WAF web ACL that Spec 11 PB3.6 first specified.
#
# Tunnel (docs/adr-edge-cost.md, "Follow-on"): `tunnel` adds the cloudflared container, a
# container health check on wt-api, and the mirrored outside images; `load_balancer = false`
# removes the load balancer and its pieces; `web_ipv6_only` runs the task in IPv6-only subnets
# with no public IPv4 address. envs/v1's `edge` and `web_ipv6_only` set these in stages.

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

variable "mail_from" {
  description = "SES sender for the removal request confirmation email (WT_MAIL_FROM)."
  type        = string
}

variable "llm" {
  description = "Language model settings for synthesis, verification and triage (Specs 04 and 05). The API key comes from Secrets Manager."
  type = object({
    model = string
    # Per-stage models; an empty one is left unset, so the stage falls back to `model`.
    stage_models = optional(object({
      synth   = optional(string, "")
      verify  = optional(string, "")
      triage  = optional(string, "")
      excerpt = optional(string, "")
      rank    = optional(string, "")
    }), {})
    workspace_id    = string
    real_data_ok    = bool
    monthly_cap_usd = number
  })
}

variable "alert_topic_arn" {
  description = "SNS topic for alerts to the owner (WT_ALERT_TOPIC_ARN)."
  type        = string
}

variable "maintenance" {
  description = "When true, the load balancer answers every path with the fixed maintenance page and nothing reaches the web task (Spec 11, PB4.4). The task keeps running."
  type        = bool
  default     = false
}

variable "cloudflare_only" {
  description = "Forward only requests carrying the Cloudflare origin header; answer the rest with a 403."
  type        = bool
  default     = false
}

variable "alb_public_ipv4" {
  description = "Give the load balancer public IPv4 addresses. False is dualstack-without-public-ipv4: reachable over IPv6 only, so only behind Cloudflare."
  type        = bool
  default     = true
}

variable "load_balancer" {
  description = "Run the load balancer, its certificate and listeners. False once the Cloudflare Tunnel carries all traffic."
  type        = bool
  default     = true
}

variable "tunnel" {
  description = "Run cloudflared beside wt-api, with a container health check, so the Cloudflare Tunnel can carry the app's hostnames. Needs the cloudflare_tunnel_token secret to hold a value."
  type        = bool
  default     = false

  validation {
    condition     = var.tunnel || var.load_balancer
    error_message = "Something must carry the traffic: keep the load balancer or turn on the tunnel."
  }
}

variable "web_ipv6_only" {
  description = "Run the web task in web_ipv6_subnet_ids with no public IPv4 address. Needs the tunnel and no load balancer, and a database in dual-stack mode."
  type        = bool
  default     = false

  validation {
    condition     = !var.web_ipv6_only || (var.tunnel && !var.load_balancer)
    error_message = "An IPv6-only web task needs the tunnel and no load balancer."
  }
}

variable "web_ipv6_subnet_ids" {
  description = "The IPv6-only subnets for web_ipv6_only."
  type        = list(string)
  default     = []

  validation {
    condition     = !var.web_ipv6_only || length(var.web_ipv6_subnet_ids) > 0
    error_message = "web_ipv6_only needs the IPv6-only subnets."
  }
}

variable "mirror_tags" {
  description = "Tags in this module's repository holding copies of the outside images the web task runs with the tunnel (runbook 12). Tags are immutable, so each pins one image."
  type = object({
    cloudflared = string
    aws_cli     = string
  })
  default = {
    cloudflared = "mirror-cloudflared-2026.10.0"
    aws_cli     = "mirror-aws-cli-2.31.0"
  }
}

variable "forwarded_hops" {
  description = "WT_FORWARDED_HOPS: proxies that append to X-Forwarded-For. 1 for the load balancer alone or Cloudflare through the tunnel, 2 for Cloudflare in front of the load balancer."
  type        = number
  default     = 1
}

variable "access_logs_bucket" {
  description = "Bucket for the load balancer's access logs (under alb/). Empty turns them off."
  type        = string
  default     = ""
}

variable "health_check_grace_seconds" {
  description = "Time a new task has to copy and load the models before load balancer or container health checks count."
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

variable "eval_task_role_arn" {
  type = string
}

variable "eval_cpu" {
  type    = number
  default = 2048
}

variable "eval_memory" {
  type    = number
  default = 8192
}

variable "web_task_role_arn" {
  type = string
}

variable "admin_task_role_arn" {
  description = "What one-off admin commands (wt takedown, wt audit) may call in S3."
  type        = string
}

variable "records_bucket" {
  description = "The records bucket (WT_RECORDS_BUCKET) for admin commands that store rights records."
  type        = string
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
  # The load balancer allows 1,024 bytes; tests/maintenance.tftest.hcl checks this stays under.
  maintenance_page = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Wheel Turning: down for maintenance</title></head><body style=\"font-family:Georgia,serif;max-width:34rem;margin:4rem auto;padding:0 1rem;line-height:1.5\"><h1>Down for maintenance</h1><p>Wheel Turning is down for maintenance. Your invite link and your questions are kept, and the site will be back as soon as the work is done.</p><p>Testers will hear by email when it returns.</p></body></html>"

  common_env = [
    { name = "WT_ENV", value = "prod" },
    # The name RDS holds in database_identity; prod commands that write refuse any other (PB1.1).
    { name = "WT_DATABASE_NAME", value = "rds-v1" },
    { name = "WT_LOG", value = "info" },
    { name = "WT_TRANSCRIPT_BUCKET", value = var.bucket_names["transcripts"] },
  ]
  # WT_LLM_MODEL_<STAGE> for each stage given its own model (Spec 11, PB2.0); the rest fall
  # back to WT_LLM_MODEL.
  llm_stage_env = [
    for stage, model in var.llm.stage_models :
    { name = "WT_LLM_MODEL_${upper(stage)}", value = model } if model != ""
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
    rules = [
      {
        # Copies of outside images the web task runs (runbook 12): never expired by the next
        # rule, which would otherwise push them out after 20 deploys.
        rulePriority = 1
        description  = "Keep the 10 most recent mirrored images"
        selection    = { tagStatus = "tagged", tagPrefixList = ["mirror-"], countType = "imageCountMoreThan", countNumber = 10 }
        action       = { type = "expire" }
      },
      {
        rulePriority = 2
        description  = "Keep the 20 most recent images"
        selection    = { tagStatus = "any", countType = "imageCountMoreThan", countNumber = 20 }
        action       = { type = "expire" }
      },
    ]
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
# The web task's containers
# ---------------------------------------------------------------------------------------------

locals {
  # ECR's dual-stack name, which an IPv6-only task must pull through.
  web_repository_url = var.web_ipv6_only ? replace(replace(aws_ecr_repository.this.repository_url, ".dkr.ecr.", ".dkr-ecr."), ".amazonaws.com/", ".on.aws/") : aws_ecr_repository.this.repository_url
  # With the tunnel, both outside images come from copies in this repository (`mirror-` tags,
  # pushed by hand per runbook 12), so an IPv6-only task never needs Docker Hub or public.ecr.aws.
  web_models_image = var.tunnel ? "${local.web_repository_url}:${var.mirror_tags.aws_cli}" : var.models_image

  models_container = {
    name      = "models"
    image     = local.web_models_image
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
  }

  web_container = merge({
    name         = "web"
    image        = "${local.web_repository_url}:${coalesce(local.web_tag, "none")}"
    essential    = true
    command      = ["wt-api"]
    portMappings = [{ containerPort = var.web_port, protocol = "tcp" }]
    dependsOn    = [{ containerName = "models", condition = "SUCCESS" }]
    mountPoints  = [{ sourceVolume = "models", containerPath = "/models", readOnly = true }]
    environment = concat(local.common_env, local.db_env, local.llm_stage_env, [
      { name = "WT_BIND", value = "0.0.0.0:${var.web_port}" },
      { name = "WT_DB_USER", value = "wt_web_login" },
      { name = "WT_MODEL_DIR", value = "/models" },
      { name = "WT_EMBED_MODEL", value = var.models.embed },
      { name = "WT_RERANK_MODEL", value = var.models.rerank },
      { name = "WT_LLM_MODEL", value = var.llm.model },
      { name = "ANTHROPIC_WORKSPACE_ID", value = var.llm.workspace_id },
      { name = "WT_LLM_REAL_DATA_OK", value = tostring(var.llm.real_data_ok) },
      { name = "WT_LLM_MONTHLY_CAP_USD", value = tostring(var.llm.monthly_cap_usd) },
      # Cloudflare, the load balancer or both are the only way in, so X-Forwarded-For names the
      # client: the entry forwarded_hops from the end. cloudflared adds no entry of its own.
      { name = "WT_TRUST_FORWARDED_FOR", value = "true" },
      { name = "WT_FORWARDED_HOPS", value = tostring(var.forwarded_hops) },
      # Removal requests are confirmed by an emailed link (Spec 08).
      { name = "WT_MAIL_FROM", value = var.mail_from },
      { name = "WT_PUBLIC_URL", value = "https://${var.domain_name}" },
      { name = "WT_ALERT_TOPIC_ARN", value = var.alert_topic_arn },
      ], var.web_ipv6_only ? [
      # An IPv6-only task reaches SES, SNS and S3 through their dual-stack names.
      { name = "AWS_USE_DUALSTACK_ENDPOINT", value = "true" },
    ] : [])
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
    }, { for k, v in {
      # With the tunnel there is no target group: ECS replaces a task whose /healthz fails, and a
      # deploy waits for the new task to turn healthy. The image has no shell, so wt-api checks
      # itself. An image older than this check fails it, so roll back no further than the first
      # image that has it (runbook 12, "The Cloudflare Tunnel").
      healthCheck = {
        command     = ["CMD", "/usr/local/bin/wt-api", "--healthcheck"]
        interval    = 15
        timeout     = 5
        retries     = 3
        startPeriod = 300 # the model copy and load; the most ECS allows
      }
  } : k => v if var.tunnel })

  # cloudflared dials out to Cloudflare and carries the app's hostnames to wt-api on localhost.
  # It starts only once wt-api is healthy, so a new task joins the tunnel only when it can answer,
  # and stops first, draining open requests for up to 90 seconds.
  cloudflared_container = {
    name              = "cloudflared"
    image             = "${local.web_repository_url}:${var.mirror_tags.cloudflared}"
    essential         = true
    command           = ["tunnel", "--no-autoupdate", "--metrics", "127.0.0.1:20241", "run"]
    dependsOn         = [{ containerName = "web", condition = "HEALTHY" }]
    memoryReservation = 64
    stopTimeout       = 120
    environment = [
      { name = "TUNNEL_EDGE_IP_VERSION", value = var.web_ipv6_only ? "6" : "auto" },
      { name = "TUNNEL_GRACE_PERIOD", value = "90s" },
    ]
    # The token lets whoever holds it serve the app's hostnames. It is set out of band from the
    # edge stack (runbook 12) and never appears in a task definition or a log.
    secrets = [{ name = "TUNNEL_TOKEN", valueFrom = var.secret_arns["cloudflare_tunnel_token"] }]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = var.log_group_names["web"]
        awslogs-region        = local.region
        awslogs-stream-prefix = "cloudflared"
      }
    }
  }

  web_containers = concat([local.models_container, local.web_container], var.tunnel ? [local.cloudflared_container] : [])
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

  container_definitions = jsonencode(local.web_containers)
}

# One-off evaluation runs (Spec 07) on the deployed stack: the image, models, config and database
# the site uses. The eval container's command is set per run, for example
# ["wt", "eval", "run", "--set", "practitioner@2"]; whatever it writes under /out (an
# export-review page) is copied to s3://<transcripts>/eval-reviews/ when it ends. It connects as
# the owner login, since evaluation writes eval_run and eval_result and asks as the tester eval.
# It takes the migrate tag, so a deploy's first plan moves it with the migrate and admin tasks.
resource "aws_ecs_task_definition" "eval" {
  family                   = "${var.name}-eval"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = var.eval_cpu
  memory                   = var.eval_memory
  execution_role_arn       = var.admin_exec_role_arn
  task_role_arn            = var.eval_task_role_arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  volume {
    name = "models"
  }

  volume {
    name = "out"
  }

  container_definitions = jsonencode([
    {
      name       = "models"
      image      = var.models_image
      essential  = false
      entryPoint = ["sh", "-c"]
      # /out is created writable here because the eval image runs as a non-root user.
      command = [join(" ", [
        "aws s3 sync s3://${var.bucket_names["models"]}/ /models/ --only-show-errors",
        "--exclude '*' --include '${var.models.embed}/*' --include '${var.models.rerank}/*'",
        "&& chmod 0777 /out",
      ])]
      mountPoints = [
        { sourceVolume = "models", containerPath = "/models" },
        { sourceVolume = "out", containerPath = "/out" },
      ]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = var.log_group_names["admin"]
          awslogs-region        = local.region
          awslogs-stream-prefix = "eval-models"
        }
      }
    },
    {
      name      = "eval"
      image     = "${aws_ecr_repository.this.repository_url}:${local.migrate_tag}"
      essential = false
      command   = ["wt", "eval", "--help"]
      dependsOn = [{ containerName = "models", condition = "SUCCESS" }]
      mountPoints = [
        { sourceVolume = "models", containerPath = "/models", readOnly = true },
        { sourceVolume = "out", containerPath = "/out" },
      ]
      environment = concat(local.common_env, local.db_env, local.llm_stage_env, [
        { name = "WT_DB_USER", value = "wt_owner_login" },
        { name = "WT_MODEL_DIR", value = "/models" },
        { name = "WT_EMBED_MODEL", value = var.models.embed },
        { name = "WT_RERANK_MODEL", value = var.models.rerank },
        { name = "WT_LLM_MODEL", value = var.llm.model },
        { name = "ANTHROPIC_WORKSPACE_ID", value = var.llm.workspace_id },
        { name = "WT_LLM_REAL_DATA_OK", value = tostring(var.llm.real_data_ok) },
        { name = "WT_LLM_MONTHLY_CAP_USD", value = tostring(var.llm.monthly_cap_usd) },
        { name = "WT_ALERT_TOPIC_ARN", value = var.alert_topic_arn },
      ])
      secrets = [
        { name = "WT_DB_PASSWORD", valueFrom = var.secret_arns["db_wt_owner_login"] },
        { name = "WT_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
        { name = "ANTHROPIC_API_KEY", valueFrom = var.secret_arns["anthropic_api_key"] },
      ]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = var.log_group_names["admin"]
          awslogs-region        = local.region
          awslogs-stream-prefix = "eval"
        }
      }
    },
    {
      # Runs whether the eval succeeded or not, and ends the task.
      name        = "upload"
      image       = var.models_image
      essential   = true
      command     = ["s3", "cp", "/out/", "s3://${var.bucket_names["transcripts"]}/eval-reviews/", "--recursive", "--only-show-errors"]
      dependsOn   = [{ containerName = "eval", condition = "COMPLETE" }]
      mountPoints = [{ sourceVolume = "out", containerPath = "/out", readOnly = true }]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = var.log_group_names["admin"]
          awslogs-region        = local.region
          awslogs-stream-prefix = "eval-upload"
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
  task_role_arn            = var.admin_task_role_arn

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
      { name = "WT_RECORDS_BUCKET", value = var.records_bucket },
    ])
    secrets = [
      # RDS stores the master credentials as JSON; ECS extracts one key.
      { name = "WT_DB_PASSWORD", valueFrom = "${var.db.master_secret_arn}:password::" },
      { name = "WT_SESSION_SECRET", valueFrom = var.secret_arns["session_secret"] },
      { name = "WT_LOGIN_PASSWORD_WT_OWNER_LOGIN", valueFrom = var.secret_arns["db_wt_owner_login"] },
      { name = "WT_LOGIN_PASSWORD_WT_WEB_LOGIN", valueFrom = var.secret_arns["db_wt_web_login"] },
      { name = "WT_LOGIN_PASSWORD_WT_PIPELINE_LOGIN", valueFrom = var.secret_arns["db_wt_pipeline_login"] },
      { name = "WT_LOGIN_PASSWORD_WT_ANALYST_LOGIN", valueFrom = var.secret_arns["db_wt_analyst_login"] },
      { name = "WT_LOGIN_PASSWORD_WT_AUDIT_LOGIN", valueFrom = var.secret_arns["db_wt_audit_login"] },
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
  count = var.load_balancer ? 1 : 0

  name                       = var.name
  load_balancer_type         = "application"
  internal                   = false
  ip_address_type            = var.alb_public_ipv4 ? "dualstack" : "dualstack-without-public-ipv4"
  subnets                    = var.public_subnet_ids
  security_groups            = [var.security_group_ids["alb"]]
  drop_invalid_header_fields = true
  idle_timeout               = 60

  # Roadmap UB3.9 (WA SEC04-BP01): one gzip file per node every 5 minutes, kept 30 days.
  access_logs {
    bucket  = var.access_logs_bucket
    prefix  = "alb"
    enabled = var.access_logs_bucket != ""
  }
}

resource "aws_lb_target_group" "web" {
  count = var.load_balancer ? 1 : 0

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
  count = var.load_balancer ? 1 : 0

  domain_name               = var.domain_name
  subject_alternative_names = var.alternate_domain_names
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

# Waits until the validation CNAME (output certificate_validation_records) exists at Cloudflare.
resource "aws_acm_certificate_validation" "this" {
  count = var.load_balancer ? 1 : 0

  certificate_arn = aws_acm_certificate.this[0].arn

  timeouts {
    create = "2h"
  }
}

# The header Cloudflare adds to every request it sends here (a transform rule, set by hand from
# the `cloudflare_origin_header` output). It sits in state and in the listener rule; it proves a
# request came through this zone's Cloudflare, not who sent it.
resource "random_password" "origin_header" {
  count = var.load_balancer ? 1 : 0

  length  = 40
  special = false
}

locals {
  origin_header_name = "X-WT-Origin"

  # What a request that reached the site gets: the web task, or the maintenance page.
  site_action = var.maintenance ? {
    type             = "fixed-response"
    target_group_arn = null
    status_code      = "503"
    message_body     = local.maintenance_page
    } : {
    type             = "forward"
    target_group_arn = one(aws_lb_target_group.web[*].arn)
    status_code      = null
    message_body     = null
  }

  refused_page = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Wheel Turning</title></head><body><p>Please use https://${var.domain_name}.</p></body></html>"

  # Without `cloudflare_only` the default action is the site; with it, a 403, and only the rule
  # below, which needs the origin header, reaches the site.
  default_action = var.cloudflare_only ? {
    type             = "fixed-response"
    target_group_arn = null
    status_code      = "403"
    message_body     = local.refused_page
  } : local.site_action
}

resource "aws_lb_listener" "https" {
  count = var.load_balancer ? 1 : 0

  load_balancer_arn = aws_lb.this[0].arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate_validation.this[0].certificate_arn

  # `maintenance` swaps the forward for a fixed 503 page on every path, /healthz included, so the
  # health check alarms say the site is down while it is (runbook 13).
  default_action {
    type             = local.default_action.type
    target_group_arn = local.default_action.target_group_arn

    dynamic "fixed_response" {
      for_each = local.default_action.type == "fixed-response" ? [1] : []
      content {
        content_type = "text/html"
        status_code  = local.default_action.status_code
        message_body = local.default_action.message_body
      }
    }
  }
}

# Exists before `cloudflare_only` is set, so turning it on changes only the default action and no
# request through Cloudflare is ever refused in between.
resource "aws_lb_listener_rule" "from_cloudflare" {
  count = var.load_balancer ? 1 : 0

  listener_arn = aws_lb_listener.https[0].arn
  priority     = 10

  condition {
    http_header {
      http_header_name = local.origin_header_name
      values           = [random_password.origin_header[0].result]
    }
  }

  action {
    type             = local.site_action.type
    target_group_arn = local.site_action.target_group_arn

    dynamic "fixed_response" {
      for_each = local.site_action.type == "fixed-response" ? [1] : []
      content {
        content_type = "text/html"
        status_code  = local.site_action.status_code
        message_body = local.site_action.message_body
      }
    }
  }
}

resource "aws_lb_listener" "http_redirect" {
  count = var.load_balancer ? 1 : 0

  load_balancer_arn = aws_lb.this[0].arn
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
  # Covers the load balancer's checks and the container health check alike.
  health_check_grace_period_seconds = var.health_check_grace_seconds

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  # No NAT gateway in v1: the task's own public IPv4 address is its way out, until it moves to
  # IPv6-only subnets and goes out over IPv6. The security group allows no inbound but the load
  # balancer, and nothing at all behind the tunnel.
  network_configuration {
    subnets          = var.web_ipv6_only ? var.web_ipv6_subnet_ids : var.public_subnet_ids
    security_groups  = [var.security_group_ids["web"]]
    assign_public_ip = !var.web_ipv6_only
  }

  dynamic "load_balancer" {
    for_each = aws_lb_target_group.web
    content {
      target_group_arn = load_balancer.value.arn
      container_name   = "web"
      container_port   = var.web_port
    }
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
    eval    = aws_ecs_task_definition.eval.family
  }
}

output "alb_dns_name" {
  description = "Null without the load balancer."
  value       = one(aws_lb.this[*].dns_name)
}

output "alb_arn_suffix" {
  value = one(aws_lb.this[*].arn_suffix)
}

output "target_group_arn_suffix" {
  value = one(aws_lb_target_group.web[*].arn_suffix)
}

output "cloudflare_origin_header" {
  description = "The request header a Cloudflare transform rule sets on the app's hostnames (runbook, edge cutover)."
  sensitive   = true
  value = {
    name  = local.origin_header_name
    value = one(random_password.origin_header[*].result)
  }
}

output "certificate_validation_records" {
  description = "Add each as a DNS-only CNAME at Cloudflare."
  value = [for o in flatten(aws_acm_certificate.this[*].domain_validation_options) : {
    name  = o.resource_record_name
    type  = o.resource_record_type
    value = o.resource_record_value
  }]
}

# The load balancer and its pieces gained a count (`load_balancer`); these keep them in place.
moved {
  from = aws_lb.this
  to   = aws_lb.this[0]
}

moved {
  from = aws_lb_target_group.web
  to   = aws_lb_target_group.web[0]
}

moved {
  from = aws_acm_certificate.this
  to   = aws_acm_certificate.this[0]
}

moved {
  from = aws_acm_certificate_validation.this
  to   = aws_acm_certificate_validation.this[0]
}

moved {
  from = random_password.origin_header
  to   = random_password.origin_header[0]
}

moved {
  from = aws_lb_listener.https
  to   = aws_lb_listener.https[0]
}

moved {
  from = aws_lb_listener_rule.from_cloudflare
  to   = aws_lb_listener_rule.from_cloudflare[0]
}

moved {
  from = aws_lb_listener.http_redirect
  to   = aws_lb_listener.http_redirect[0]
}
