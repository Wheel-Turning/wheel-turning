# Spec 01 network: a VPC over two availability zones with public and private subnets and an S3
# gateway endpoint. No NAT gateway in v1: compute runs in public subnets behind security groups
# that allow no inbound traffic except the load balancer to the web service, and the database
# sits in private subnets with no route to the internet.
#
# The VPC and the public subnets also carry an Amazon-provided IPv6 block, so the load balancer can
# drop its public IPv4 addresses behind Cloudflare (docs/adr-edge-cost.md). Tasks and the batch
# host keep IPv4 only, until `ipv6_web`: then the private subnets gain IPv6 for a dual-stack
# database, and two IPv6-only subnets take the web task, which reaches the internet over IPv6 with
# no public IPv4 address (the Cloudflare Tunnel follow-on, stages T-D1 and T-D2).

variable "name" {
  description = "Name prefix for every resource."
  type        = string
}

variable "cidr_block" {
  type    = string
  default = "10.40.0.0/16"
}

variable "ipv6_web" {
  description = "IPv6 on the private subnets (for a dual-stack database) and two IPv6-only subnets for the web task."
  type        = bool
  default     = false
}

data "aws_availability_zones" "available" {
  state = "available"
}

data "aws_region" "current" {}

locals {
  azs = slice(data.aws_availability_zones.available.names, 0, 2)
}

resource "aws_vpc" "this" {
  cidr_block           = var.cidr_block
  enable_dns_support   = true
  enable_dns_hostnames = true
  # Amazon's IPv6 block carries no charge; only the load balancer uses it.
  assign_generated_ipv6_cidr_block = true
  tags                             = { Name = var.name }
}

# The default security group denies everything, so nothing can use it by accident.
resource "aws_default_security_group" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-default-deny" }
}

resource "aws_internet_gateway" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = var.name }
}

resource "aws_subnet" "public" {
  count                   = length(local.azs)
  vpc_id                  = aws_vpc.this.id
  availability_zone       = local.azs[count.index]
  cidr_block              = cidrsubnet(var.cidr_block, 8, count.index)
  map_public_ip_on_launch = false
  ipv6_cidr_block         = cidrsubnet(aws_vpc.this.ipv6_cidr_block, 8, count.index)
  # Tasks and the batch host keep IPv4 only.
  assign_ipv6_address_on_creation = false
  tags                            = { Name = "${var.name}-public-${local.azs[count.index]}", tier = "public" }
}

resource "aws_subnet" "private" {
  count             = length(local.azs)
  vpc_id            = aws_vpc.this.id
  availability_zone = local.azs[count.index]
  cidr_block        = cidrsubnet(var.cidr_block, 8, 100 + count.index)
  # RDS's dual-stack mode needs IPv6 on every subnet of its subnet group. Still no route out.
  ipv6_cidr_block = var.ipv6_web ? cidrsubnet(aws_vpc.this.ipv6_cidr_block, 8, 100 + count.index) : null
  tags            = { Name = "${var.name}-private-${local.azs[count.index]}", tier = "private" }
}

# The web task's subnets once it gives up its public IPv4 address: IPv6 only, routed to the
# internet gateway through the public route table. Fargate supports IPv6-only tasks in us-east-2;
# its image pulls go through ECR's dual-stack names.
resource "aws_subnet" "web_ipv6" {
  count                                          = var.ipv6_web ? length(local.azs) : 0
  vpc_id                                         = aws_vpc.this.id
  availability_zone                              = local.azs[count.index]
  ipv6_native                                    = true
  ipv6_cidr_block                                = cidrsubnet(aws_vpc.this.ipv6_cidr_block, 8, 50 + count.index)
  assign_ipv6_address_on_creation                = true
  enable_resource_name_dns_aaaa_record_on_launch = true
  private_dns_hostname_type_on_launch            = "resource-name"
  tags                                           = { Name = "${var.name}-web-ipv6-${local.azs[count.index]}", tier = "web-ipv6" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-public" }
}

resource "aws_route" "public_internet" {
  route_table_id         = aws_route_table.public.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.this.id
}

resource "aws_route" "public_internet_ipv6" {
  route_table_id              = aws_route_table.public.id
  destination_ipv6_cidr_block = "::/0"
  gateway_id                  = aws_internet_gateway.this.id
}

resource "aws_route_table_association" "public" {
  count          = length(aws_subnet.public)
  subnet_id      = aws_subnet.public[count.index].id
  route_table_id = aws_route_table.public.id
}

resource "aws_route_table_association" "web_ipv6" {
  count          = length(aws_subnet.web_ipv6)
  subnet_id      = aws_subnet.web_ipv6[count.index].id
  route_table_id = aws_route_table.public.id
}

# Private subnets get no default route at all.
resource "aws_route_table" "private" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-private" }
}

resource "aws_route_table_association" "private" {
  count          = length(aws_subnet.private)
  subnet_id      = aws_subnet.private[count.index].id
  route_table_id = aws_route_table.private.id
}

# Free gateway endpoint: S3 traffic stays inside AWS from every subnet.
resource "aws_vpc_endpoint" "s3" {
  vpc_id            = aws_vpc.this.id
  service_name      = "com.amazonaws.${data.aws_region.current.region}.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = [aws_route_table.public.id, aws_route_table.private.id]
  tags              = { Name = "${var.name}-s3" }
}

# Security groups, per the Spec 01 network rules:
# - the load balancer accepts HTTPS (and HTTP, for the redirect) from the addresses below and
#   talks only to the web tasks. Once Cloudflare fronts it, those are Cloudflare's ranges and
#   HTTPS only (envs/v1 `cloudflare_only`)
# - the web tasks accept traffic only from the load balancer, and from nothing at all once the
#   Cloudflare Tunnel replaces it (`web_from_alb = false`): cloudflared dials out instead
# - the batch host accepts no inbound traffic
# - the database accepts 5432 only from the web and batch security groups
# One-off tasks (migrations, logins) run in the web security group.

variable "web_port" {
  description = "Port the web container listens on."
  type        = number
  default     = 8080
}

variable "alb_ingress" {
  description = "Who may reach the load balancer: IPv4 and IPv6 ranges, and the ports they may use."
  type = object({
    ipv4  = list(string)
    ipv6  = list(string)
    ports = list(number)
  })
  default = {
    ipv4  = ["0.0.0.0/0"]
    ipv6  = ["::/0"]
    ports = [443, 80]
  }
}

variable "web_from_alb" {
  description = "Admit the load balancer to the web tasks. False once the Cloudflare Tunnel is the only way in."
  type        = bool
  default     = true
}

variable "tunnel_egress" {
  description = "Cloudflare's ranges the web task's cloudflared may reach on 7844 (TCP and UDP, the tunnel's port). Empty without the tunnel."
  type = object({
    ipv4 = list(string)
    ipv6 = list(string)
  })
  default = { ipv4 = [], ipv6 = [] }
}

locals {
  tunnel_egress = merge(
    { for p in setproduct(["tcp", "udp"], var.tunnel_egress.ipv4) : "${p[0]} ${p[1]}" => { protocol = p[0], ipv4 = p[1], ipv6 = null } },
    { for p in setproduct(["tcp", "udp"], var.tunnel_egress.ipv6) : "${p[0]} ${p[1]}" => { protocol = p[0], ipv4 = null, ipv6 = p[1] } },
  )
  alb_ingress_ipv4 = { for p in setproduct(var.alb_ingress.ports, var.alb_ingress.ipv4) : "${p[0]} ${p[1]}" => p }
  alb_ingress_ipv6 = { for p in setproduct(var.alb_ingress.ports, var.alb_ingress.ipv6) : "${p[0]} ${p[1]}" => p }
}

# The security group's description cannot change in place, so it keeps its first wording.
resource "aws_security_group" "alb" {
  name        = "${var.name}-alb"
  description = "Load balancer: HTTP and HTTPS from the internet"
  vpc_id      = aws_vpc.this.id
  tags        = { Name = "${var.name}-alb" }
}

resource "aws_vpc_security_group_ingress_rule" "alb_ipv4" {
  for_each          = local.alb_ingress_ipv4
  security_group_id = aws_security_group.alb.id
  description       = each.value[0] == 443 ? "HTTPS" : "HTTP, redirected to HTTPS"
  ip_protocol       = "tcp"
  from_port         = each.value[0]
  to_port           = each.value[0]
  cidr_ipv4         = each.value[1]
}

resource "aws_vpc_security_group_ingress_rule" "alb_ipv6" {
  for_each          = local.alb_ingress_ipv6
  security_group_id = aws_security_group.alb.id
  description       = each.value[0] == 443 ? "HTTPS" : "HTTP, redirected to HTTPS"
  ip_protocol       = "tcp"
  from_port         = each.value[0]
  to_port           = each.value[0]
  cidr_ipv6         = each.value[1]
}

# The two rules that existed before, so the first apply keeps them instead of replacing them.
moved {
  from = aws_vpc_security_group_ingress_rule.alb_https
  to   = aws_vpc_security_group_ingress_rule.alb_ipv4["443 0.0.0.0/0"]
}

moved {
  from = aws_vpc_security_group_ingress_rule.alb_http
  to   = aws_vpc_security_group_ingress_rule.alb_ipv4["80 0.0.0.0/0"]
}

resource "aws_vpc_security_group_egress_rule" "alb_to_web" {
  security_group_id            = aws_security_group.alb.id
  description                  = "To the web tasks only"
  ip_protocol                  = "tcp"
  from_port                    = var.web_port
  to_port                      = var.web_port
  referenced_security_group_id = aws_security_group.web.id
}

resource "aws_security_group" "web" {
  name        = "${var.name}-web"
  description = "Web tasks: inbound from the load balancer only"
  vpc_id      = aws_vpc.this.id
  tags        = { Name = "${var.name}-web" }
}

resource "aws_vpc_security_group_ingress_rule" "web_from_alb" {
  count                        = var.web_from_alb ? 1 : 0
  security_group_id            = aws_security_group.web.id
  description                  = "From the load balancer"
  ip_protocol                  = "tcp"
  from_port                    = var.web_port
  to_port                      = var.web_port
  referenced_security_group_id = aws_security_group.alb.id
}

# Egress by port (roadmap UB3.9, WA SEC05-BP02). Everything the web, migrate, eval and audit
# tasks call is HTTPS: AWS APIs (S3 through the gateway endpoint, ECR, Secrets Manager, Logs,
# SES, SNS), the Claude API and the help-link check. The database is the one other destination.
# Security groups do not filter DNS to the VPC resolver.
resource "aws_vpc_security_group_egress_rule" "web_https_out" {
  security_group_id = aws_security_group.web.id
  description       = "HTTPS: AWS APIs and the Claude API"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = "0.0.0.0/0"
}

# Once the web task is IPv6-only, the same calls go over IPv6.
resource "aws_vpc_security_group_egress_rule" "web_https_out_ipv6" {
  count             = var.ipv6_web ? 1 : 0
  security_group_id = aws_security_group.web.id
  description       = "HTTPS over IPv6: AWS APIs and the Claude API"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv6         = "::/0"
}

# cloudflared's connections to Cloudflare's edge.
resource "aws_vpc_security_group_egress_rule" "web_tunnel_out" {
  for_each          = local.tunnel_egress
  security_group_id = aws_security_group.web.id
  description       = "Cloudflare Tunnel (${upper(each.value.protocol)})"
  ip_protocol       = each.value.protocol
  from_port         = 7844
  to_port           = 7844
  cidr_ipv4         = each.value.ipv4
  cidr_ipv6         = each.value.ipv6
}

resource "aws_vpc_security_group_egress_rule" "web_to_db" {
  security_group_id            = aws_security_group.web.id
  description                  = "To the database"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db.id
}

resource "aws_security_group" "batch" {
  name        = "${var.name}-batch"
  description = "Batch host: no inbound traffic, SSM only"
  vpc_id      = aws_vpc.this.id
  tags        = { Name = "${var.name}-batch" }
}

# The batch host also fetches media and feeds, some over plain HTTP, and installs packages, so it
# keeps port 80 as well.
resource "aws_vpc_security_group_egress_rule" "batch_web_out" {
  for_each          = { https = 443, http = 80 }
  security_group_id = aws_security_group.batch.id
  description       = each.key == "https" ? "HTTPS: SSM, AWS APIs and media sources" : "HTTP: media sources and feeds"
  ip_protocol       = "tcp"
  from_port         = each.value
  to_port           = each.value
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_egress_rule" "batch_to_db" {
  security_group_id            = aws_security_group.batch.id
  description                  = "To the database"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db.id
}

resource "aws_security_group" "db" {
  name        = "${var.name}-db"
  description = "Database: 5432 from the web and batch security groups only"
  vpc_id      = aws_vpc.this.id
  tags        = { Name = "${var.name}-db" }
}

resource "aws_vpc_security_group_ingress_rule" "db_from_web" {
  security_group_id            = aws_security_group.db.id
  description                  = "From the web tasks"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.web.id
}

resource "aws_vpc_security_group_ingress_rule" "db_from_batch" {
  security_group_id            = aws_security_group.db.id
  description                  = "From the batch host"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.batch.id
}

output "security_group_ids" {
  value = {
    alb   = aws_security_group.alb.id
    web   = aws_security_group.web.id
    batch = aws_security_group.batch.id
    db    = aws_security_group.db.id
  }
}

output "vpc_id" {
  value = aws_vpc.this.id
}

output "public_subnet_ids" {
  value = aws_subnet.public[*].id
}

output "private_subnet_ids" {
  value = aws_subnet.private[*].id
}

output "web_ipv6_subnet_ids" {
  description = "The IPv6-only subnets for the web task; empty without `ipv6_web`."
  value       = aws_subnet.web_ipv6[*].id
}

moved {
  from = aws_vpc_security_group_ingress_rule.web_from_alb
  to   = aws_vpc_security_group_ingress_rule.web_from_alb[0]
}
