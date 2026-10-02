terraform {
  # 1.10 adds S3-native state locking, which the M1 backend uses.
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  # M1 adds the S3 backend once the bootstrap stack has created the state bucket.
}
