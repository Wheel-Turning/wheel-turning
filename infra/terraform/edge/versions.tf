terraform {
  # 1.10 adds S3-native state locking, which the backend uses.
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 5.28"
    }
  }

  backend "s3" {
    bucket       = "wt-tfstate-<AWS_ACCOUNT_ID>"
    key          = "edge/terraform.tfstate"
    region       = "us-east-2"
    encrypt      = true
    use_lockfile = true
  }
}
