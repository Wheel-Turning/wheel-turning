# Well-Architected guardrails for storage (WA SEC08-BP02, SEC09-BP02, SEC07-BP04): every bucket
# blocks public access, is encrypted and refuses plain HTTP; scratch keeps nothing past a day
# (CLAUDE.md rule 6); access logs expire. Offline:
#   cd infra/terraform/modules/storage && terraform init -backend=false && terraform test

mock_provider "aws" {}

override_data {
  target = data.aws_caller_identity.current
  values = {
    account_id = "000000000000"
  }
}

variables {
  name = "wt-test"
}

run "every_bucket_is_private_encrypted_and_tls_only" {
  command = plan

  assert {
    condition = alltrue(concat(
      [for b in aws_s3_bucket_public_access_block.this :
      b.block_public_acls && b.block_public_policy && b.ignore_public_acls && b.restrict_public_buckets],
      [aws_s3_bucket_public_access_block.alb_logs.block_public_acls && aws_s3_bucket_public_access_block.alb_logs.restrict_public_buckets],
    ))
    error_message = "Every bucket must block all public access."
  }

  assert {
    condition = alltrue(concat(
      [for e in aws_s3_bucket_server_side_encryption_configuration.this :
      one([for r in e.rule : one(r.apply_server_side_encryption_by_default).sse_algorithm]) != ""],
      [one([for r in aws_s3_bucket_server_side_encryption_configuration.alb_logs.rule : one(r.apply_server_side_encryption_by_default).sse_algorithm]) == "AES256"],
    ))
    error_message = "Every bucket must set default encryption."
  }

  assert {
    condition = alltrue([
      for k, d in data.aws_iam_policy_document.tls_only :
      anytrue([for s in d.statement : s.effect == "Deny" && anytrue([for c in s.condition : c.variable == "aws:SecureTransport" && contains(c.values, "false")])])
    ])
    error_message = "Every workload bucket's policy must deny requests without TLS."
  }

  assert {
    condition = anytrue([
      for s in data.aws_iam_policy_document.alb_logs.statement :
      s.effect == "Deny" && anytrue([for c in s.condition : c.variable == "aws:SecureTransport"])
    ])
    error_message = "The access-log bucket must deny requests without TLS."
  }

  assert {
    condition     = alltrue([for o in aws_s3_bucket_ownership_controls.this : one(o.rule).object_ownership == "BucketOwnerEnforced"])
    error_message = "ACLs must be off on every bucket."
  }
}

run "scratch_and_logs_expire" {
  command = plan

  assert {
    condition     = one([for r in aws_s3_bucket_lifecycle_configuration.scratch.rule : one(r.expiration).days]) == 1
    error_message = "Scratch must expire objects after one day: audio must not persist."
  }

  assert {
    condition     = one([for r in aws_s3_bucket_lifecycle_configuration.alb_logs.rule : one(r.expiration).days]) <= 30
    error_message = "Access logs must expire within 30 days."
  }

  assert {
    condition     = !contains(keys(aws_s3_bucket.this), "audio")
    error_message = "No bucket may hold audio (CLAUDE.md rule 6)."
  }
}
