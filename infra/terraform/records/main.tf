# A permanent, append-only copy of every exclusion, opt-out and takedown (Spec 03 exclusions,
# Spec 07 takedown), so `wt takedown --replay` can restore them after a database restore, a bad
# migration or a mistaken script. Exclusion and opt-out records carry the teacher's name, aliases,
# content hashes and reason, because discovery matches new talks by name; takedown receipts carry
# IDs, per-store counts and the actor. Neither ever carries a teacher's words.
#
# Every object is written with an Object Lock legal hold, which has no expiry, so records are kept
# permanently; the bucket policy refuses any write without one. The GOVERNANCE default retention
# is a second lock that still holds an object for retention_days if its hold is ever lifted.
#
# It lives in its own stack, apart from envs/v1, so destroying and rebuilding v1 never touches it.
# No workload writes to it yet: the takedown routine (M2, finished in M8) gains s3:PutObject and
# s3:PutObjectLegalHold on this bucket, and nothing ever gains delete or bypass permissions.

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

  # Records are kept permanently, so every write must place a legal hold. A request without the
  # header fails too, because StringNotEquals matches when the key is absent.
  statement {
    sid       = "DenyWriteWithoutLegalHold"
    effect    = "Deny"
    actions   = ["s3:PutObject"]
    resources = ["${aws_s3_bucket.records.arn}/*"]
    principals {
      type        = "*"
      identifiers = ["*"]
    }
    condition {
      test     = "StringNotEquals"
      variable = "s3:object-lock-legal-hold"
      values   = ["ON"]
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
