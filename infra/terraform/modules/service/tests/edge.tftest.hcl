# docs/adr-edge-cost.md: behind Cloudflare the HTTPS listener refuses anything without the origin
# header, and the maintenance page still answers every request that has it. Offline, like
# maintenance.tftest.hcl:
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
    db_wt_web_login      = "arn:secret:web"
    db_wt_owner_login    = "arn:secret:owner"
    db_wt_pipeline_login = "arn:secret:pipeline"
    db_wt_analyst_login  = "arn:secret:analyst"
    db_wt_audit_login    = "arn:secret:audit"
    session_secret       = "arn:secret:session"
    anthropic_api_key    = "arn:secret:anthropic"
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

run "open_to_all_by_default" {
  command = plan

  assert {
    condition     = aws_lb_listener.https.default_action[0].type == "forward"
    error_message = "Without cloudflare_only the listener forwards every request."
  }

  assert {
    condition     = aws_lb_listener_rule.from_cloudflare.action[0].type == "forward"
    error_message = "The origin header rule exists from the start, so turning cloudflare_only on refuses nothing that came through Cloudflare."
  }

  assert {
    condition     = aws_lb.this.ip_address_type == "dualstack"
    error_message = "The load balancer keeps public IPv4 by default."
  }

  assert {
    condition     = one([for e in jsondecode(aws_ecs_task_definition.web.container_definitions)[1].environment : e.value if e.name == "WT_FORWARDED_HOPS"]) == "1"
    error_message = "The web task counts one proxy by default."
  }
}

run "cloudflare_only_refuses_requests_without_the_header" {
  command = plan

  variables {
    cloudflare_only = true
    alb_public_ipv4 = false
    forwarded_hops  = 2
  }

  assert {
    condition     = aws_lb_listener.https.default_action[0].type == "fixed-response" && aws_lb_listener.https.default_action[0].fixed_response[0].status_code == "403"
    error_message = "A request without the origin header gets a 403."
  }

  assert {
    condition     = aws_lb_listener_rule.from_cloudflare.action[0].type == "forward"
    error_message = "A request with the origin header reaches the web task."
  }

  assert {
    condition     = one(flatten([for c in aws_lb_listener_rule.from_cloudflare.condition : [for h in c.http_header : h.http_header_name]])) == "X-WT-Origin"
    error_message = "The rule matches the X-WT-Origin header."
  }

  assert {
    condition     = aws_lb.this.ip_address_type == "dualstack-without-public-ipv4"
    error_message = "alb_public_ipv4 = false drops the public IPv4 addresses."
  }

  assert {
    condition     = one([for e in jsondecode(aws_ecs_task_definition.web.container_definitions)[1].environment : e.value if e.name == "WT_FORWARDED_HOPS"]) == "2"
    error_message = "Behind Cloudflare the web task counts two proxies."
  }
}

run "maintenance_behind_cloudflare" {
  command = plan

  variables {
    cloudflare_only = true
    maintenance     = true
  }

  assert {
    condition     = aws_lb_listener_rule.from_cloudflare.action[0].type == "fixed-response" && aws_lb_listener_rule.from_cloudflare.action[0].fixed_response[0].status_code == "503"
    error_message = "Requests through Cloudflare get the maintenance page."
  }

  assert {
    condition     = strcontains(aws_lb_listener_rule.from_cloudflare.action[0].fixed_response[0].message_body, "Down for maintenance")
    error_message = "The maintenance page says the site is down for maintenance."
  }

  assert {
    condition     = length(local.refused_page) <= 1024
    error_message = "The load balancer allows at most 1,024 bytes of fixed response."
  }
}
