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
