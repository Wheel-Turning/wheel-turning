# An append-only record of takedown and exclusion events (Spec 07 takedown, Spec 03 exclusions,
# Spec 08 rights ledger): one object per event, written once and kept for the retention period.
# S3 Object Lock makes each object version undeletable and unchangeable until it expires, so the
# record survives a database restore, a mistaken script or a compromised workload role.
#
# It lives in its own stack, apart from envs/v1, so destroying and rebuilding v1 never touches it.
# No workload writes to it yet: the takedown routine (M2, finished in M8) gains s3:PutObject on
# this bucket, and nothing ever gains delete or bypass permissions.

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project = "wheel-turning"
      env     = "v1"
    }
  }
}

data "aws_caller_identity" "current" {}

resource "aws_s3_bucket" "records" {
  bucket              = "wt-records-${data.aws_caller_identity.current.account_id}"
  object_lock_enabled = true # can only be set when the bucket is created

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_ownership_controls" "records" {
  bucket = aws_s3_bucket.records.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "records" {
  bucket                  = aws_s3_bucket.records.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

# Object Lock requires versioning, and it stays on for good.
resource "aws_s3_bucket_versioning" "records" {
  bucket = aws_s3_bucket.records.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "records" {
  bucket = aws_s3_bucket.records.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_object_lock_configuration" "records" {
  bucket = aws_s3_bucket.records.id

  rule {
    default_retention {
      mode = var.retention_mode
      days = var.retention_days
    }
  }

  depends_on = [aws_s3_bucket_versioning.records]
}

data "aws_iam_policy_document" "records" {
  statement {
    sid       = "DenyInsecureTransport"
    effect    = "Deny"
    actions   = ["s3:*"]
    resources = [aws_s3_bucket.records.arn, "${aws_s3_bucket.records.arn}/*"]
    principals {
      type        = "*"
      identifiers = ["*"]
    }
    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }

}

resource "aws_s3_bucket_policy" "records" {
  bucket = aws_s3_bucket.records.id
  policy = data.aws_iam_policy_document.records.json

  depends_on = [
    aws_s3_bucket_public_access_block.records,
    aws_s3_bucket_object_lock_configuration.records,
  ]
}

output "records_bucket" {
  value = aws_s3_bucket.records.id
}
