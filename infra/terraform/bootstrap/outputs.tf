output "state_bucket" {
  description = "Bucket for every stack's remote state. Each stack uses its own key."
  value       = aws_s3_bucket.state.id
}

output "state_bucket_arn" {
  value = aws_s3_bucket.state.arn
}
