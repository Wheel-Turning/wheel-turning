# Spec 11, PB3.6: a WAFv2 web ACL on the load balancer with the AWS Common and Known Bad Inputs
# managed rule groups and a rate rule of 300 requests per 5 minutes per client address.
#
# Every rule counts until `waf_block` is set, which is meant to happen after 7 days of counting
# with no false positives in the sampled requests. The Common group's body-size rule
# (SizeRestrictions_BODY, over 8 KB) stays in count even then while `waf_count_body_size` is true:
# the rights and accessibility forms take up to 4,000 characters of free text, which form-encoded
# can pass 8 KB (any non-ASCII character takes 6 to 9 bytes).
#
# WAF inspects requests only, so responses, including the ask page's server-sent events stream,
# pass through unchanged. DNS is DNS-only at Cloudflare, so the address the rate rule sees is the
# client's own.

variable "waf_block" {
  description = "Switch the WAF rules from count to block. Spec 11 PB3.6: set after 7 days in count."
  type        = bool
  default     = false
}

variable "waf_count_body_size" {
  description = "Keep the Common group's SizeRestrictions_BODY rule in count when blocking is on."
  type        = bool
  default     = true
}

variable "waf_rate_limit" {
  description = "Requests per client address per 5 minutes before the rate rule matches."
  type        = number
  default     = 300
}

locals {
  waf_rules_count = !var.waf_block
}

resource "aws_wafv2_web_acl" "this" {
  name        = var.name
  description = "Edge protection for the ${var.name} load balancer, Spec 11 PB3.6"
  scope       = "REGIONAL"

  default_action {
    allow {}
  }

  rule {
    name     = "aws-common"
    priority = 10

    override_action {
      dynamic "count" {
        for_each = local.waf_rules_count ? [1] : []
        content {}
      }
      dynamic "none" {
        for_each = local.waf_rules_count ? [] : [1]
        content {}
      }
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesCommonRuleSet"

        dynamic "rule_action_override" {
          for_each = var.waf_count_body_size ? ["SizeRestrictions_BODY"] : []
          content {
            name = rule_action_override.value
            action_to_use {
              count {}
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${var.name}-aws-common"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "aws-known-bad-inputs"
    priority = 20

    override_action {
      dynamic "count" {
        for_each = local.waf_rules_count ? [1] : []
        content {}
      }
      dynamic "none" {
        for_each = local.waf_rules_count ? [] : [1]
        content {}
      }
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesKnownBadInputsRuleSet"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${var.name}-aws-known-bad-inputs"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "rate-per-ip"
    priority = 30

    action {
      dynamic "count" {
        for_each = local.waf_rules_count ? [1] : []
        content {}
      }
      dynamic "block" {
        for_each = local.waf_rules_count ? [] : [1]
        content {}
      }
    }

    statement {
      rate_based_statement {
        limit                 = var.waf_rate_limit
        evaluation_window_sec = 300
        aggregate_key_type    = "IP"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${var.name}-rate-per-ip"
      sampled_requests_enabled   = true
    }
  }

  visibility_config {
    cloudwatch_metrics_enabled = true
    metric_name                = var.name
    sampled_requests_enabled   = true
  }
}

resource "aws_wafv2_web_acl_association" "alb" {
  resource_arn = aws_lb.this.arn
  web_acl_arn  = aws_wafv2_web_acl.this.arn
}

output "web_acl_name" {
  value = aws_wafv2_web_acl.this.name
}
