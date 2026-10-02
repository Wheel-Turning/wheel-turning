terraform {
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  # Applied first with local state, before any other resource exists. Once the bootstrap stack
  # has created the state bucket, this block gains an S3 backend and the state migrates there.
}
