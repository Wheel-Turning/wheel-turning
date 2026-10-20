# The Cloudflare side of the edge (docs/adr-edge-cost.md, "Follow-on", decision T1 (a)), in its
# own stack and state so that only the owner applies it: deploy.yml never plans it, and the
# Cloudflare API token it needs never reaches CI. Run from this directory with the owner's AWS
# profile and CLOUDFLARE_API_TOKEN set (runbook 12, "The Cloudflare Tunnel").

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project = "wheel-turning"
      env     = "v1"
    }
  }
}

# Reads CLOUDFLARE_API_TOKEN from the environment.
provider "cloudflare" {}

# Until the app's hostnames move to the tunnel they point at the load balancer, read here by
# name so the two stacks never disagree.
data "aws_lb" "web" {
  count = var.app_via_tunnel ? 0 : 1
  name  = "wt-v1"
}

module "edge" {
  source         = "../modules/edge"
  zone_name      = var.zone_name
  name           = "wt-v1"
  hostnames      = var.hostnames
  test_hostname  = var.test_hostname
  app_via_tunnel = var.app_via_tunnel
  alb_dns_name   = try(data.aws_lb.web[0].dns_name, "")
  maintenance    = var.maintenance
}
