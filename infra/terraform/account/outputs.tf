output "budget_name" {
  value = aws_budgets_budget.monthly.name
}

output "anomaly_monitor_arn" {
  value = var.enable_anomaly_detection ? coalesce(var.existing_anomaly_monitor_arn, one(aws_ce_anomaly_monitor.services[*].arn)) : null
}
