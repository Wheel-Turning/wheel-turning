# Well-Architected guardrails for the database (WA SEC08-BP02, SEC09-BP02, REL09-BP03,
# SUS04-BP04): private, encrypted, TLS-only, backed up for at least a week, and able to grow its
# storage. Offline:
#   cd infra/terraform/modules/database && terraform init -backend=false && terraform test

mock_provider "aws" {}

variables {
  name               = "wt-test"
  private_subnet_ids = ["subnet-a", "subnet-b"]
  security_group_id  = "sg-db"
}

run "private_encrypted_tls_and_backed_up" {
  command = plan

  assert {
    condition     = aws_db_instance.this.publicly_accessible == false
    error_message = "The database must not be publicly accessible."
  }

  assert {
    condition     = aws_db_instance.this.storage_encrypted == true
    error_message = "The database volume must be encrypted."
  }

  assert {
    condition     = aws_db_instance.this.backup_retention_period >= 7
    error_message = "Automated backups must be kept at least 7 days (Spec 01; RPO in Spec 01's targets)."
  }

  assert {
    condition     = aws_db_instance.this.manage_master_user_password == true
    error_message = "RDS must own the master password, so it never passes through Terraform."
  }

  assert {
    condition     = anytrue([for p in aws_db_parameter_group.this.parameter : p.name == "rds.force_ssl" && p.value == "1"])
    error_message = "The parameter group must force TLS."
  }

  assert {
    condition     = aws_db_instance.this.skip_final_snapshot == false
    error_message = "Destroying the database must take a final snapshot."
  }
}

run "storage_can_grow" {
  command = plan

  assert {
    condition     = aws_db_instance.this.max_allocated_storage > aws_db_instance.this.allocated_storage
    error_message = "Storage autoscaling needs a ceiling above the allocated size."
  }
}
