# Spec 11, PB4.4: `maintenance = true` makes the HTTPS listener answer every path with the fixed
# page, and false forwards to the web task. Runs offline against a mocked provider:
#   cd infra/terraform/modules/service && terraform init -backend=false && terraform test

mock_provider "aws" {}

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

run "forwards_by_default" {
  command = plan

  assert {
    condition     = aws_lb_listener.https[0].default_action[0].type == "forward"
    error_message = "Without maintenance the listener forwards to the web task."
  }
}

run "maintenance_answers_with_the_fixed_page" {
  command = plan

  variables {
    maintenance = true
  }

  assert {
    condition     = aws_lb_listener.https[0].default_action[0].type == "fixed-response"
    error_message = "maintenance = true must answer with a fixed response."
  }

  assert {
    condition     = aws_lb_listener.https[0].default_action[0].fixed_response[0].status_code == "503"
    error_message = "The maintenance page answers 503."
  }

  assert {
    condition     = strcontains(aws_lb_listener.https[0].default_action[0].fixed_response[0].message_body, "Down for maintenance")
    error_message = "The maintenance page says the site is down for maintenance."
  }

  assert {
    condition     = length(aws_lb_listener.https[0].default_action[0].fixed_response[0].message_body) <= 1024
    error_message = "The load balancer allows at most 1,024 bytes of fixed response."
  }
}
