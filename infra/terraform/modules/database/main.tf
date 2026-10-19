# Spec 01 database: RDS Postgres 16 with a parameter group and a subnet group. Encrypted, 7-day
# backups, rds.force_ssl=1, no public access, storage that grows on its own up to a ceiling. The vector extension is created by migration.
# RDS owns the master password in Secrets Manager, so it never passes through Terraform.

variable "name" {
  description = "Name prefix for every resource."
  type        = string
}

variable "private_subnet_ids" {
  type = list(string)
}

variable "security_group_id" {
  type = string
}

variable "instance_class" {
  type    = string
  default = "db.t4g.micro"
}

variable "allocated_storage_gb" {
  type    = number
  default = 20
}

variable "max_allocated_storage_gb" {
  description = "Ceiling for storage autoscaling: RDS grows the volume online when free space runs low, up to this. Roadmap UB3.9 (WA SUS04-BP04)."
  type        = number
  default     = 100
}

variable "backup_retention_days" {
  type    = number
  default = 7
}

variable "deletion_protection" {
  description = "Blocks deleting the instance. Off by default so terraform destroy works, as the M1 acceptance criteria require."
  type        = bool
  default     = false
}

variable "skip_final_snapshot" {
  description = "When false, destroying the instance first takes a final snapshot."
  type        = bool
  default     = false
}

resource "aws_db_subnet_group" "this" {
  name       = var.name
  subnet_ids = var.private_subnet_ids
}

# shared_buffers stays at RDS's default, a quarter of the instance's memory. The rest is left to
# the OS page cache, which holds the parts of the search indexes that do not fit in the buffers.
# Raising it on a 2 GB instance starves that cache: on a copy held to 2 GB with 960 MB of
# buffers, a pass of 242 searches ran for over 30 minutes without finishing, against 2.5 minutes
# with the default.
resource "aws_db_parameter_group" "this" {
  name   = "${var.name}-pg16"
  family = "postgres16"

  parameter {
    name  = "rds.force_ssl"
    value = "1"
    # A static parameter: it takes effect at boot, and the instance was created with it.
    apply_method = "pending-reboot"
  }
}

resource "aws_db_instance" "this" {
  identifier     = var.name
  engine         = "postgres"
  engine_version = "16"
  instance_class = var.instance_class

  allocated_storage     = var.allocated_storage_gb
  max_allocated_storage = var.max_allocated_storage_gb
  storage_type          = "gp3"
  storage_encrypted     = true

  db_name                     = "wt"
  username                    = "wt_admin"
  manage_master_user_password = true

  db_subnet_group_name   = aws_db_subnet_group.this.name
  vpc_security_group_ids = [var.security_group_id]
  parameter_group_name   = aws_db_parameter_group.this.name
  publicly_accessible    = false
  multi_az               = false

  backup_retention_period    = var.backup_retention_days
  auto_minor_version_upgrade = true
  copy_tags_to_snapshot      = true
  deletion_protection        = var.deletion_protection
  skip_final_snapshot        = var.skip_final_snapshot
  final_snapshot_identifier  = var.skip_final_snapshot ? null : "${var.name}-final"

  # Roadmap UB3.9 (WA PERF03-BP03): CloudWatch Database Insights in Standard mode, which replaced
  # the Performance Insights console on July 31, 2026. Seven days of DB load and per-query
  # metrics are free; turning it on needs no restart.
  performance_insights_enabled          = true
  performance_insights_retention_period = 7
  database_insights_mode                = "standard"

  # A size change applies at once instead of in the next maintenance window; it restarts the
  # instance, a few minutes without the database.
  apply_immediately = true
}

output "address" {
  value = aws_db_instance.this.address
}

output "port" {
  value = aws_db_instance.this.port
}

output "db_name" {
  value = aws_db_instance.this.db_name
}

output "identifier" {
  value = aws_db_instance.this.identifier
}

output "master_username" {
  value = aws_db_instance.this.username
}

output "master_secret_arn" {
  description = "Secrets Manager secret RDS manages for the master user (JSON with username and password)."
  value       = aws_db_instance.this.master_user_secret[0].secret_arn
}
