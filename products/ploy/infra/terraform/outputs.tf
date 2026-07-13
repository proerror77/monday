output "ec2_public_ip" {
  description = "Public IP of the EC2 instance"
  value       = aws_eip.ploy.public_ip
}

output "ec2_instance_id" {
  description = "ID of the EC2 instance"
  value       = aws_instance.ploy.id
}

output "rds_endpoint" {
  description = "RDS endpoint"
  value       = aws_db_instance.ploy.endpoint
}

output "rds_address" {
  description = "RDS address (without port)"
  value       = aws_db_instance.ploy.address
}

output "database_url" {
  description = "Database connection URL"
  value       = "postgres://${var.db_username}:PASSWORD@${aws_db_instance.ploy.address}:${aws_db_instance.ploy.port}/${var.db_name}"
  sensitive   = true
}

output "vpc_id" {
  description = "VPC ID"
  value       = aws_vpc.main.id
}

output "public_subnet_id" {
  description = "Public subnet ID"
  value       = aws_subnet.public.id
}

output "secrets_manager_db_arn" {
  description = "ARN of the database password secret"
  value       = aws_secretsmanager_secret.db_password.arn
}

output "secrets_manager_wallet_arn" {
  description = "ARN of the wallet key secret"
  value       = aws_secretsmanager_secret.wallet_key.arn
}

output "cloudwatch_log_group" {
  description = "CloudWatch log group name"
  value       = aws_cloudwatch_log_group.ploy.name
}

output "cloudwatch_dashboard_url" {
  description = "CloudWatch dashboard URL"
  value       = "https://${var.aws_region}.console.aws.amazon.com/cloudwatch/home?region=${var.aws_region}#dashboards:name=${aws_cloudwatch_dashboard.ploy.dashboard_name}"
}

output "ssh_command" {
  description = "SSH command to connect to EC2"
  value       = "ssh -i ${var.ec2_key_name}.pem ec2-user@${aws_eip.ploy.public_ip}"
}
