output "budget_name" {
  value = aws_budgets_budget.monthly.name
}

output "anomaly_monitor_arn" {
  value = var.enable_anomaly_detection ? coalesce(var.existing_anomaly_monitor_arn, one(aws_ce_anomaly_monitor.services[*].arn)) : null
}

output "ses_identity_arn" {
  value = aws_sesv2_email_identity.mail.arn
}

output "ses_dkim_records_for_cloudflare" {
  description = "Create each as a DNS-only (grey cloud) CNAME at Cloudflare so SES can sign mail."
  value = [for token in aws_sesv2_email_identity.mail.dkim_signing_attributes[0].tokens : {
    name  = "${token}._domainkey.${var.mail_domain}"
    type  = "CNAME"
    value = "${token}.dkim.amazonses.com"
  }]
}

output "security_alarm_names" {
  value = [
    aws_cloudwatch_metric_alarm.root_activity.alarm_name,
    aws_cloudwatch_metric_alarm.iam_policy_change.alarm_name,
  ]
}

output "guardduty_detector_id" {
  value = aws_guardduty_detector.this.id
}

output "ses_mail_from_records_for_cloudflare" {
  description = "Create both at Cloudflare (DNS-only) so SES sends from the custom MAIL FROM domain and SPF aligns."
  value = [
    {
      name     = aws_sesv2_email_identity_mail_from_attributes.mail.mail_from_domain
      type     = "MX"
      priority = 10
      value    = "feedback-smtp.${var.region}.amazonses.com"
    },
    {
      name  = aws_sesv2_email_identity_mail_from_attributes.mail.mail_from_domain
      type  = "TXT"
      value = "v=spf1 include:amazonses.com ~all"
    },
  ]
}

output "ses_feedback_topic_arn" {
  description = "SNS topic receiving SES bounce and complaint notices."
  value       = aws_sns_topic.ses_feedback.arn
}
