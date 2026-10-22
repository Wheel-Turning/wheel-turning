# Well-Architected guardrails for the network (WA SEC05-BP01, SEC05-BP02): the database has no
# route out and admits only the two compute security groups, and no security group allows every
# protocol out. Offline, like the service module's tests:
#   cd infra/terraform/modules/network && terraform init -backend=false && terraform test

mock_provider "aws" {}

override_data {
  target = data.aws_availability_zones.available
  values = {
    names = ["us-east-2a", "us-east-2b", "us-east-2c"]
  }
}

override_data {
  target = data.aws_region.current
  values = {
    region = "us-east-2"
  }
}

variables {
  name = "wt-test"
}

run "two_zones_public_and_private" {
  command = plan

  assert {
    condition     = length(aws_subnet.public) == 2 && length(aws_subnet.private) == 2
    error_message = "The VPC must span two availability zones, public and private."
  }

  assert {
    condition     = alltrue([for s in aws_subnet.public : s.map_public_ip_on_launch == false])
    error_message = "Public subnets must not hand out public IPv4 addresses by default."
  }

  assert {
    condition     = alltrue([for s in aws_subnet.private : s.cidr_block != null && s.assign_ipv6_address_on_creation != true])
    error_message = "Private subnets carry IPv4 only."
  }
}

run "egress_is_scoped_by_port" {
  command = plan

  assert {
    condition = (
      aws_vpc_security_group_egress_rule.web_https_out.ip_protocol == "tcp" &&
      aws_vpc_security_group_egress_rule.web_https_out.from_port == 443 &&
      aws_vpc_security_group_egress_rule.web_https_out.to_port == 443
    )
    error_message = "The web tasks may reach the internet on tcp 443 only."
  }

  assert {
    condition = (
      aws_vpc_security_group_egress_rule.web_to_db.from_port == 5432 &&
      aws_vpc_security_group_egress_rule.web_to_db.cidr_ipv4 == null
    )
    error_message = "The web tasks reach the database by security group, on 5432 only."
  }

  assert {
    condition = alltrue([
      for r in aws_vpc_security_group_egress_rule.batch_web_out :
      r.ip_protocol == "tcp" && contains([80, 443], r.from_port) && r.from_port == r.to_port
    ])
    error_message = "The batch host may reach the internet on tcp 80 and 443 only."
  }

  assert {
    condition = (
      aws_vpc_security_group_egress_rule.batch_to_db.from_port == 5432 &&
      aws_vpc_security_group_egress_rule.batch_to_db.cidr_ipv4 == null
    )
    error_message = "The batch host reaches the database by security group, on 5432 only."
  }

  assert {
    condition = (
      aws_vpc_security_group_egress_rule.alb_to_web.from_port == var.web_port &&
      aws_vpc_security_group_egress_rule.alb_to_web.cidr_ipv4 == null
    )
    error_message = "The load balancer talks only to the web tasks."
  }
}

run "the_database_admits_only_compute" {
  command = plan

  assert {
    condition = alltrue([
      for r in [aws_vpc_security_group_ingress_rule.db_from_web, aws_vpc_security_group_ingress_rule.db_from_batch] :
      r.from_port == 5432 && r.to_port == 5432 && r.cidr_ipv4 == null && r.cidr_ipv6 == null
    ])
    error_message = "The database must admit 5432 from security groups only, never from an address range."
  }

  assert {
    condition = (
      aws_vpc_security_group_ingress_rule.web_from_alb[0].from_port == var.web_port &&
      aws_vpc_security_group_ingress_rule.web_from_alb[0].cidr_ipv4 == null
    )
    error_message = "The web tasks must admit only the load balancer."
  }
}

run "cloudflare_only_admits_its_ranges_on_https" {
  command = plan

  variables {
    alb_ingress = {
      ipv4  = []
      ipv6  = ["2400:cb00::/32"]
      ports = [443]
    }
  }

  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.alb_ipv4) == 0
    error_message = "Behind Cloudflare the load balancer admits no IPv4 range."
  }

  assert {
    condition     = alltrue([for r in aws_vpc_security_group_ingress_rule.alb_ipv6 : r.from_port == 443])
    error_message = "Behind Cloudflare the load balancer admits HTTPS only."
  }
}

run "behind_the_tunnel_the_web_tasks_admit_nothing" {
  command = plan

  variables {
    web_from_alb  = false
    tunnel_egress = { ipv4 = ["198.41.128.0/17"], ipv6 = ["2606:4700::/32"] }
  }

  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.web_from_alb) == 0
    error_message = "Once the Cloudflare Tunnel carries all traffic, nothing may reach the web tasks."
  }

  assert {
    condition     = alltrue([for r in aws_vpc_security_group_egress_rule.web_tunnel_out : r.from_port == 7844 && r.to_port == 7844])
    error_message = "cloudflared reaches Cloudflare on 7844 only."
  }

  assert {
    condition     = length(aws_vpc_security_group_egress_rule.web_tunnel_out) == 4
    error_message = "TCP and UDP to each of Cloudflare's ranges given, and nowhere else."
  }
}
