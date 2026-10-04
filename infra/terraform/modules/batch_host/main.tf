# Spec 01 batch_host: an EC2 launch template and instance with an instance profile.
# g5.xlarge by default with an NVIDIA-ready image, a 200 GB gp3 scratch volume, access through
# SSM only. `enabled` creates or destroys the instance; it is the one costly resource.
# A CPU instance type gets plain Amazon Linux 2023, which is how the M1 toggle test runs after
# AWS declined the GPU quota (Spec 01, GPU access).

variable "name" {
  type = string
}

variable "enabled" {
  description = "batch_host_enabled: true creates the instance, false destroys it."
  type        = bool
  default     = false
}

variable "instance_type" {
  type    = string
  default = "g5.xlarge"
}

variable "scratch_volume_gb" {
  type    = number
  default = 200
}

variable "subnet_id" {
  type = string
}

variable "security_group_id" {
  type = string
}

variable "instance_profile_name" {
  type = string
}

variable "gpu_ami_ssm_parameter" {
  description = "Public SSM parameter for the latest NVIDIA-driver Deep Learning base AMI, used for GPU instance types."
  type        = string
  default     = "/aws/service/deeplearning/ami/x86_64/base-oss-nvidia-driver-gpu-amazon-linux-2023/latest/ami-id"
}

variable "cpu_ami_ssm_parameter" {
  description = "Public SSM parameter for the latest Amazon Linux 2023 AMI, used for CPU instance types (Spec 01, GPU access: the toggle test runs on a small CPU type)."
  type        = string
  default     = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
}

locals {
  # G and P families carry NVIDIA GPUs and get the driver image; anything else gets plain AL2023.
  gpu_instance = can(regex("^[gp][0-9]", var.instance_type))
}

data "aws_ssm_parameter" "ami" {
  count = var.enabled ? 1 : 0
  name  = local.gpu_instance ? var.gpu_ami_ssm_parameter : var.cpu_ami_ssm_parameter
}

resource "aws_launch_template" "this" {
  count         = var.enabled ? 1 : 0
  name          = "${var.name}-batch"
  image_id      = nonsensitive(data.aws_ssm_parameter.ami[0].value)
  instance_type = var.instance_type

  iam_instance_profile {
    name = var.instance_profile_name
  }

  vpc_security_group_ids = [var.security_group_id]

  metadata_options {
    http_tokens                 = "required" # IMDSv2 only
    http_put_response_hop_limit = 1
    http_endpoint               = "enabled"
  }

  block_device_mappings {
    device_name = "/dev/sdf"
    ebs {
      volume_size           = var.scratch_volume_gb
      volume_type           = "gp3"
      encrypted             = true
      delete_on_termination = true
    }
  }

  tag_specifications {
    resource_type = "instance"
    tags          = { Name = "${var.name}-batch", project = "dharma-gates", env = "v1" }
  }

  tag_specifications {
    resource_type = "volume"
    tags          = { Name = "${var.name}-batch", project = "dharma-gates", env = "v1" }
  }
}

resource "aws_instance" "this" {
  count     = var.enabled ? 1 : 0
  subnet_id = var.subnet_id

  # A public IP is needed for outbound traffic without a NAT gateway. The security group allows
  # no inbound traffic, and access is through Session Manager.
  associate_public_ip_address = true

  launch_template {
    id      = aws_launch_template.this[0].id
    version = aws_launch_template.this[0].latest_version
  }
}

output "instance_id" {
  value = one(aws_instance.this[*].id)
}
