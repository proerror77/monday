# RDS Subnet Group
resource "aws_db_subnet_group" "ploy" {
  name       = "${var.project_name}-db-subnet-group"
  subnet_ids = [aws_subnet.private.id, aws_subnet.private_2.id]

  tags = {
    Name = "${var.project_name}-db-subnet-group"
  }
}

# RDS PostgreSQL Instance
resource "aws_db_instance" "ploy" {
  identifier     = "${var.project_name}-db"
  engine         = "postgres"
  engine_version = "15"
  instance_class = var.db_instance_class

  allocated_storage     = 20
  max_allocated_storage = 100
  storage_type          = "gp3"
  storage_encrypted     = true

  db_name  = var.db_name
  username = var.db_username
  password = var.db_password

  db_subnet_group_name   = aws_db_subnet_group.ploy.name
  vpc_security_group_ids = [aws_security_group.rds.id]

  # Backup configuration
  backup_retention_period = 7
  backup_window           = "03:00-04:00"
  maintenance_window      = "Mon:04:00-Mon:05:00"

  # Performance Insights (free tier for t3.micro)
  performance_insights_enabled = true

  # No public access
  publicly_accessible = false

  # Skip final snapshot for dev (enable in prod)
  skip_final_snapshot       = true
  final_snapshot_identifier = "${var.project_name}-final-snapshot"

  # Auto minor version upgrade
  auto_minor_version_upgrade = true

  # Deletion protection (enable in prod)
  deletion_protection = false

  tags = {
    Name = "${var.project_name}-db"
  }
}

# Secrets Manager for DB password (optional, for automated rotation)
resource "aws_secretsmanager_secret" "db_password" {
  name                    = "${var.project_name}/db-password"
  description             = "Database password for Ploy"
  recovery_window_in_days = 7

  tags = {
    Name = "${var.project_name}-db-password"
  }
}

resource "aws_secretsmanager_secret_version" "db_password" {
  secret_id = aws_secretsmanager_secret.db_password.id
  secret_string = jsonencode({
    username = var.db_username
    password = var.db_password
    host     = aws_db_instance.ploy.address
    port     = aws_db_instance.ploy.port
    dbname   = var.db_name
  })
}

# Secrets Manager for Wallet Key
resource "aws_secretsmanager_secret" "wallet_key" {
  name                    = "${var.project_name}/wallet-key"
  description             = "Wallet private key for Ploy trading"
  recovery_window_in_days = 7

  tags = {
    Name = "${var.project_name}-wallet-key"
  }
}

# Note: The wallet key secret value should be set manually via AWS Console
# or CLI to avoid storing it in Terraform state
