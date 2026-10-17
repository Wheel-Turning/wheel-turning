output "dns_records_for_cloudflare" {
  description = "Create each as a DNS-only (grey cloud) record at Cloudflare."
  value = concat(module.service.certificate_validation_records, [for name in concat([var.domain_name], var.alternate_domain_names) : {
    name  = name
    type  = "CNAME"
    value = module.service.alb_dns_name
  }])
}

output "url" {
  value = "https://${var.domain_name}"
}

output "ecr_repository_url" {
  value = module.service.ecr_repository_url
}

output "cluster_name" {
  value = module.service.cluster_name
}

output "service_name" {
  value = module.service.service_name
}

output "task_definition_families" {
  value = module.service.task_definition_families
}

output "public_subnet_ids" {
  value = module.network.public_subnet_ids
}

output "security_group_ids" {
  value = module.network.security_group_ids
}

output "bucket_names" {
  value = module.storage.bucket_names
}

output "secret_arns" {
  value = module.secrets.arns
}

output "image_tag" {
  description = "The image tag the web service runs now. deploy.yml reads it before a rollout."
  value       = var.image_tag
}

output "batch_host_instance_id" {
  value = module.batch_host.instance_id
}

output "alert_topic_arn" {
  description = "WT_ALERT_TOPIC_ARN, for `wt alerts test` and the batch host."
  value       = module.observability.alarm_topic_arn
}

output "web_acl_name" {
  value = module.service.web_acl_name
}
