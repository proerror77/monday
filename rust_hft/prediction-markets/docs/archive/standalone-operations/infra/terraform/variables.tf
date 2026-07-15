variable "aws_region" {
  description = "AWS region to deploy resources"
  type        = string
  default     = "us-east-1"
}

variable "environment" {
  description = "Environment name (prod, dev, staging)"
  type        = string
  default     = "prod"
}

variable "project_name" {
  description = "Project name for resource naming"
  type        = string
  default     = "ploy"
}

# VPC
variable "vpc_cidr" {
  description = "CIDR block for VPC"
  type        = string
  default     = "10.0.0.0/16"
}

# EC2
variable "ec2_instance_type" {
  description = "EC2 instance type"
  type        = string
  default     = "t3.small"
}

variable "ec2_key_name" {
  description = "Name of the SSH key pair"
  type        = string
  default     = "ploy-key"
}

variable "ssh_allowed_cidr" {
  description = "CIDR blocks allowed for SSH access"
  type        = list(string)
  default     = ["0.0.0.0/0"]  # Restrict in production!
}

# RDS
variable "db_instance_class" {
  description = "RDS instance class"
  type        = string
  default     = "db.t3.micro"
}

variable "db_name" {
  description = "Database name"
  type        = string
  default     = "ploy"
}

variable "db_username" {
  description = "Database master username"
  type        = string
  default     = "ploy"
  sensitive   = true
}

variable "db_password" {
  description = "Database master password"
  type        = string
  sensitive   = true
}

# Trading config
variable "trading_symbol" {
  description = "Binance symbol to trade"
  type        = string
  default     = "BTCUSDT"
}

variable "trading_market" {
  description = "Polymarket market slug"
  type        = string
  default     = "will-btc-go-up-15m"
}

variable "trade_size" {
  description = "Trade size in USD"
  type        = number
  default     = 1.0
}

variable "max_position" {
  description = "Maximum position in USD"
  type        = number
  default     = 50.0
}
