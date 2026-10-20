# Spec 11, PB4.4, behind the Cloudflare Tunnel (docs/adr-edge-cost.md, decision T1 (a)):
# `maintenance = true` routes every request for the app's hostnames to the Worker that answers 503
# with the maintenance page, and the app's hostnames move from the load balancer to the tunnel
# only with `app_via_tunnel`. Offline against a mocked provider:
#   cd infra/terraform/modules/edge && terraform init -backend=false && terraform test

mock_provider "cloudflare" {
  mock_data "cloudflare_zone" {
    defaults = {
      zone_id = "zone-0000"
      account = { id = "account-0000" }
    }
  }

  # A fixed tunnel id makes the record targets known at plan time.
  mock_resource "cloudflare_zero_trust_tunnel_cloudflared" {
    defaults = {
      id = "00000000-0000-0000-0000-000000000000"
    }
  }
}

variables {
  zone_name     = "example.org"
  name          = "wt-test"
  hostnames     = ["app.example.org", "demo.example.org"]
  test_hostname = "tunnel-test.example.org"
  alb_dns_name  = "wt-test-123.us-east-2.elb.amazonaws.com"
}

run "the_load_balancer_serves_by_default" {
  command = apply

  assert {
    condition     = alltrue([for r in cloudflare_dns_record.app : r.content == "wt-test-123.us-east-2.elb.amazonaws.com" && r.proxied])
    error_message = "Until app_via_tunnel, the app's hostnames are proxied to the load balancer."
  }

  assert {
    condition     = cloudflare_dns_record.test.content == "00000000-0000-0000-0000-000000000000.cfargotunnel.com"
    error_message = "The test hostname always reaches the tunnel."
  }

  assert {
    condition     = length(cloudflare_workers_route.maintenance) == 0
    error_message = "No maintenance route without maintenance."
  }

  assert {
    condition     = jsonencode([for i in cloudflare_zero_trust_tunnel_cloudflared_config.web.config.ingress : i.hostname]) == jsonencode(["app.example.org", "demo.example.org", "tunnel-test.example.org", null])
    error_message = "The tunnel routes the app's hostnames and the test hostname, then refuses the rest."
  }

  assert {
    condition     = alltrue([for i in slice(cloudflare_zero_trust_tunnel_cloudflared_config.web.config.ingress, 0, 3) : i.service == "http://localhost:8080"]) && cloudflare_zero_trust_tunnel_cloudflared_config.web.config.ingress[3].service == "http_status:404"
    error_message = "The tunnel sends the app's requests to wt-api inside the task."
  }
}

run "app_via_tunnel" {
  command = apply

  variables {
    app_via_tunnel = true
    alb_dns_name   = ""
  }

  assert {
    condition     = alltrue([for r in cloudflare_dns_record.app : r.content == "00000000-0000-0000-0000-000000000000.cfargotunnel.com" && r.proxied])
    error_message = "With app_via_tunnel, the app's hostnames are proxied to the tunnel."
  }
}

run "maintenance_answers_every_path_with_the_fixed_page" {
  command = apply

  variables {
    maintenance = true
  }

  assert {
    condition     = toset([for r in cloudflare_workers_route.maintenance : r.pattern]) == toset(["app.example.org/*", "demo.example.org/*"])
    error_message = "Every path of both hostnames goes to the maintenance Worker."
  }

  assert {
    condition     = alltrue([for r in cloudflare_workers_route.maintenance : r.script == "wt-test-maintenance"])
    error_message = "The routes run the maintenance Worker."
  }

  assert {
    condition     = strcontains(cloudflare_workers_script.maintenance.content, "status: 503") && strcontains(cloudflare_workers_script.maintenance.content, "Down for maintenance")
    error_message = "The Worker answers 503 with the maintenance page."
  }
}

run "the_load_balancer_name_is_needed_until_the_tunnel_serves" {
  command = plan

  variables {
    alb_dns_name = ""
  }

  expect_failures = [var.alb_dns_name]
}
