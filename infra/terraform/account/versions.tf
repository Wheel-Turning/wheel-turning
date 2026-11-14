terraform {
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  # Applied first with local state, before any other resource existed, then migrated into the
  # bucket the bootstrap stack created.
  backend "s3" {
    bucket       = "wt-tfstate-<AWS_ACCOUNT_ID>"
    key          = "account/terraform.tfstate"
    region       = "us-east-2"
    encrypt      = true
    use_lockfile = true
  }
}
