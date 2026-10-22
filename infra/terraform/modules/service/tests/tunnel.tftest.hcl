# docs/adr-edge-cost.md, "Follow-on": the Cloudflare Tunnel stages. `tunnel` adds cloudflared
# beside wt-api, with a container health check and the mirrored images; `load_balancer = false`
# removes the load balancer; `web_ipv6_only` moves the task to IPv6-only subnets with no public
# IPv4 address. Offline, like the other tests here:
#   cd infra/terraform/modules/service && terraform init -backend=false && terraform test

mock_provider "aws" {}

# The task definition's JSON names the repository URL; fixing it at plan time makes the JSON known.
override_resource {
  target          = aws_ecr_repository.this
  override_during = plan
  values = {
    repository_url = "000000000000.dkr.ecr.us-east-2.amazonaws.com/wt-test"
  }
}

variables {
  name               = "wt-test"
  vpc_id             = "vpc-0000"
  public_subnet_ids  = ["subnet-a", "subnet-b"]
  security_group_ids = { alb = "sg-alb", web = "sg-web" }
  domain_name        = "app.example.org"
  mail_from          = "rights@example.org"
  alert_topic_arn    = "arn:aws:sns:us-east-2:000000000000:wt-test-alarms"
  llm = {
    model           = "fixture-model"
    workspace_id    = "wrkspc_fixture"
    real_data_ok    = false
    monthly_cap_usd = 1
  }
  web_exec_role_arn   = "arn:aws:iam::000000000000:role/web-exec"
  admin_exec_role_arn = "arn:aws:iam::000000000000:role/admin-exec"
  web_task_role_arn   = "arn:aws:iam::000000000000:role/web-task"
  eval_task_role_arn  = "arn:aws:iam::000000000000:role/eval-task"
  admin_task_role_arn = "arn:aws:iam::000000000000:role/admin-task"
  log_group_names     = { web = "/wt-test/web", admin = "/wt-test/admin" }
  secret_arns = {
    db_wt_web_login         = "arn:secret:web"
    db_wt_owner_login       = "arn:secret:owner"
    db_wt_pipeline_login    = "arn:secret:pipeline"
    db_wt_analyst_login     = "arn:secret:analyst"
    db_wt_audit_login       = "arn:secret:audit"
    session_secret          = "arn:secret:session"
    anthropic_api_key       = "arn:secret:anthropic"
    cloudflare_tunnel_token = "arn:secret:tunnel"
  }
  bucket_names   = { transcripts = "t", scratch = "s", models = "m" }
  records_bucket = "r"
  db = {
    host              = "db.example"
    port              = 5432
    name              = "wt"
    master_username   = "master"
    master_secret_arn = "arn:secret:master"
  }
}

run "alb_alone_by_default" {
  command = plan

  assert {
    condition     = length(jsondecode(aws_ecs_task_definition.web.container_definitions)) == 2
    error_message = "Without the tunnel the web task runs models and web only."
  }

  assert {
    condition     = !contains(keys(jsondecode(aws_ecs_task_definition.web.container_definitions)[1]), "healthCheck")
    error_message = "Without the tunnel the target group, not a container health check, decides health."
  }

  assert {
    condition     = jsondecode(aws_ecs_task_definition.web.container_definitions)[0].image == "public.ecr.aws/aws-cli/aws-cli:2.31.0"
    error_message = "Without the tunnel the models container keeps its public image."
  }

  assert {
    condition     = length(aws_ecs_service.web.load_balancer) == 1 && aws_ecs_service.web.health_check_grace_period_seconds == 300
    error_message = "The service registers with the target group."
  }
}

run "tunnel_beside_the_load_balancer" {
  command = plan

  variables {
    tunnel = true
  }

  assert {
    condition     = jsondecode(aws_ecs_task_definition.web.container_definitions)[2].name == "cloudflared"
    error_message = "The tunnel adds a cloudflared container."
  }

  assert {
    condition     = jsondecode(aws_ecs_task_definition.web.container_definitions)[2].dependsOn[0].containerName == "web" && jsondecode(aws_ecs_task_definition.web.container_definitions)[2].dependsOn[0].condition == "HEALTHY"
    error_message = "cloudflared joins the tunnel only once wt-api is healthy."
  }

  assert {
    condition     = jsondecode(aws_ecs_task_definition.web.container_definitions)[2].secrets[0].name == "TUNNEL_TOKEN" && jsondecode(aws_ecs_task_definition.web.container_definitions)[2].secrets[0].valueFrom == "arn:secret:tunnel"
    error_message = "The tunnel token comes from Secrets Manager, never from the task definition."
  }

  assert {
    condition     = !strcontains(aws_ecs_task_definition.web.container_definitions, "eyJ")
    error_message = "No token value in the task definition."
  }

  assert {
    condition     = jsondecode(aws_ecs_task_definition.web.container_definitions)[1].healthCheck.command[1] == "/usr/local/bin/wt-api" && jsondecode(aws_ecs_task_definition.web.container_definitions)[1].healthCheck.command[2] == "--healthcheck"
    error_message = "wt-api checks its own /healthz."
  }

  assert {
    condition     = jsondecode(aws_ecs_task_definition.web.container_definitions)[0].image == "000000000000.dkr.ecr.us-east-2.amazonaws.com/wt-test:mirror-aws-cli-2.31.0" && jsondecode(aws_ecs_task_definition.web.container_definitions)[2].image == "000000000000.dkr.ecr.us-east-2.amazonaws.com/wt-test:mirror-cloudflared-2026.10.0"
    error_message = "Both outside images come from the mirror tags in the module's own repository."
  }

  assert {
    condition     = length(aws_lb.this) == 1 && length(aws_ecs_service.web.load_balancer) == 1
    error_message = "The load balancer keeps serving until it is turned off."
  }

  assert {
    condition     = aws_ecs_service.web.network_configuration[0].assign_public_ip
    error_message = "On the public subnets the task keeps its public IPv4 address."
  }
}

run "tunnel_alone" {
  command = plan

  variables {
    tunnel         = true
    load_balancer  = false
    forwarded_hops = 1
  }

  assert {
    condition     = length(aws_lb.this) == 0 && length(aws_lb_listener.https) == 0 && length(aws_lb_target_group.web) == 0 && length(aws_acm_certificate.this) == 0
    error_message = "Without the load balancer, nothing of it remains."
  }

  assert {
    condition     = length(aws_ecs_service.web.load_balancer) == 0 && aws_ecs_service.web.health_check_grace_period_seconds == 300
    error_message = "The service has no target group, and the grace period covers the container health check."
  }

  assert {
    condition     = one([for e in jsondecode(aws_ecs_task_definition.web.container_definitions)[1].environment : e.value if e.name == "WT_FORWARDED_HOPS"]) == "1"
    error_message = "Through the tunnel the web task counts one proxy."
  }

  assert {
    condition     = one([for e in jsondecode(aws_ecs_task_definition.web.container_definitions)[2].environment : e.value if e.name == "TUNNEL_EDGE_IP_VERSION"]) == "auto"
    error_message = "With IPv4 still there, cloudflared picks either family."
  }
}

run "ipv6_only" {
  command = plan

  variables {
    tunnel              = true
    load_balancer       = false
    web_ipv6_only       = true
    web_ipv6_subnet_ids = ["subnet-v6a", "subnet-v6b"]
  }

  assert {
    condition     = !aws_ecs_service.web.network_configuration[0].assign_public_ip && tolist(aws_ecs_service.web.network_configuration[0].subnets) == tolist(["subnet-v6a", "subnet-v6b"])
    error_message = "The task runs in the IPv6-only subnets with no public IPv4 address."
  }

  assert {
    condition     = alltrue([for c in jsondecode(aws_ecs_task_definition.web.container_definitions) : startswith(c.image, "000000000000.dkr-ecr.us-east-2.on.aws/wt-test:")])
    error_message = "Every image is pulled through ECR's dual-stack name."
  }

  assert {
    condition     = one([for e in jsondecode(aws_ecs_task_definition.web.container_definitions)[1].environment : e.value if e.name == "AWS_USE_DUALSTACK_ENDPOINT"]) == "true"
    error_message = "wt-api's own AWS clients use dual-stack endpoints."
  }

  assert {
    condition     = one([for e in jsondecode(aws_ecs_task_definition.web.container_definitions)[2].environment : e.value if e.name == "TUNNEL_EDGE_IP_VERSION"]) == "6"
    error_message = "cloudflared reaches Cloudflare over IPv6."
  }
}

run "something_must_carry_the_traffic" {
  command = plan

  variables {
    tunnel        = false
    load_balancer = false
  }

  expect_failures = [var.tunnel]
}

run "ipv6_only_needs_the_tunnel_alone" {
  command = plan

  variables {
    tunnel              = true
    web_ipv6_only       = true
    web_ipv6_subnet_ids = ["subnet-v6a"]
  }

  expect_failures = [var.web_ipv6_only]
}
