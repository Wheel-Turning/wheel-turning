# The v1 environment. Spec 01 composes one module per row of its Terraform modules table here.
# M0 only fixes the provider and tagging so CI can run fmt and validate; M1 adds the modules.

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      project = "dharma-gates"
      env     = "v1"
    }
  }
}
