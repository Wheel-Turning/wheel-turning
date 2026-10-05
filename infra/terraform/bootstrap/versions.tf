terraform {
  # 1.10 is the first release with S3-native state locking (use_lockfile), so no DynamoDB table.
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }

  # The first apply ran with local state; `terraform init -migrate-state` then moved this stack's
  # own state into the bucket it created. To rebuild from nothing, comment this block out, apply,
  # and migrate again.
  backend "s3" {
    bucket       = "wt-tfstate-904883674947"
    key          = "bootstrap/terraform.tfstate"
    region       = "us-east-2"
    encrypt      = true
    use_lockfile = true
  }
}
