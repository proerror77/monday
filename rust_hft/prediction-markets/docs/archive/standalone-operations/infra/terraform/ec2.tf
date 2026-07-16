# EC2 Instance
resource "aws_instance" "ploy" {
  ami                    = data.aws_ami.amazon_linux_2023.id
  instance_type          = var.ec2_instance_type
  key_name               = var.ec2_key_name
  subnet_id              = aws_subnet.public.id
  vpc_security_group_ids = [aws_security_group.ec2.id]
  iam_instance_profile   = aws_iam_instance_profile.ec2.name

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 20
    delete_on_termination = true
    encrypted             = true
  }

  user_data = base64encode(<<-EOF
    #!/bin/bash
    set -e

    # Update system
    dnf update -y

    # Install dependencies
    dnf install -y docker git jq

    # Start Docker
    systemctl enable docker
    systemctl start docker

    # Create ploy user
    useradd -r -m -s /bin/bash ploy

    # Create directories
    mkdir -p /opt/ploy/{bin,config,data,logs,models}
    chown -R ploy:ploy /opt/ploy

    # Install CloudWatch agent
    dnf install -y amazon-cloudwatch-agent

    # Signal completion
    echo "User data script completed" > /tmp/user_data_complete
  EOF
  )

  tags = {
    Name = "${var.project_name}-server"
  }

  lifecycle {
    create_before_destroy = true
  }
}

# Elastic IP for EC2
resource "aws_eip" "ploy" {
  instance = aws_instance.ploy.id
  domain   = "vpc"

  tags = {
    Name = "${var.project_name}-eip"
  }

  depends_on = [aws_internet_gateway.main]
}

# SSH Key Pair (import existing or create)
# Uncomment if you want Terraform to manage the key
# resource "aws_key_pair" "deploy" {
#   key_name   = var.ec2_key_name
#   public_key = file("~/.ssh/ploy-key.pub")
# }
