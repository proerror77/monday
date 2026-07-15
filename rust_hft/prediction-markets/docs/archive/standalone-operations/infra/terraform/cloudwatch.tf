# CloudWatch Log Group
resource "aws_cloudwatch_log_group" "ploy" {
  name              = "/${var.project_name}/application"
  retention_in_days = 30

  tags = {
    Name = "${var.project_name}-logs"
  }
}

# CloudWatch Alarm: EC2 CPU High
resource "aws_cloudwatch_metric_alarm" "ec2_cpu_high" {
  alarm_name          = "${var.project_name}-ec2-cpu-high"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "CPUUtilization"
  namespace           = "AWS/EC2"
  period              = 300
  statistic           = "Average"
  threshold           = 80
  alarm_description   = "EC2 CPU utilization is above 80%"

  dimensions = {
    InstanceId = aws_instance.ploy.id
  }

  alarm_actions = []  # Add SNS topic ARN for notifications

  tags = {
    Name = "${var.project_name}-ec2-cpu-alarm"
  }
}

# CloudWatch Alarm: EC2 Status Check Failed
resource "aws_cloudwatch_metric_alarm" "ec2_status_check" {
  alarm_name          = "${var.project_name}-ec2-status-check"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "StatusCheckFailed"
  namespace           = "AWS/EC2"
  period              = 60
  statistic           = "Maximum"
  threshold           = 0
  alarm_description   = "EC2 instance status check failed"

  dimensions = {
    InstanceId = aws_instance.ploy.id
  }

  alarm_actions = []  # Add SNS topic ARN for notifications

  tags = {
    Name = "${var.project_name}-ec2-status-alarm"
  }
}

# CloudWatch Alarm: RDS CPU High
resource "aws_cloudwatch_metric_alarm" "rds_cpu_high" {
  alarm_name          = "${var.project_name}-rds-cpu-high"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "CPUUtilization"
  namespace           = "AWS/RDS"
  period              = 300
  statistic           = "Average"
  threshold           = 80
  alarm_description   = "RDS CPU utilization is above 80%"

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.ploy.identifier
  }

  alarm_actions = []  # Add SNS topic ARN for notifications

  tags = {
    Name = "${var.project_name}-rds-cpu-alarm"
  }
}

# CloudWatch Alarm: RDS Free Storage Low
resource "aws_cloudwatch_metric_alarm" "rds_storage_low" {
  alarm_name          = "${var.project_name}-rds-storage-low"
  comparison_operator = "LessThanThreshold"
  evaluation_periods  = 1
  metric_name         = "FreeStorageSpace"
  namespace           = "AWS/RDS"
  period              = 300
  statistic           = "Average"
  threshold           = 5368709120  # 5 GB in bytes
  alarm_description   = "RDS free storage is below 5GB"

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.ploy.identifier
  }

  alarm_actions = []  # Add SNS topic ARN for notifications

  tags = {
    Name = "${var.project_name}-rds-storage-alarm"
  }
}

# CloudWatch Dashboard
resource "aws_cloudwatch_dashboard" "ploy" {
  dashboard_name = "${var.project_name}-dashboard"

  dashboard_body = jsonencode({
    widgets = [
      {
        type   = "metric"
        x      = 0
        y      = 0
        width  = 12
        height = 6
        properties = {
          title  = "EC2 CPU Utilization"
          region = var.aws_region
          metrics = [
            ["AWS/EC2", "CPUUtilization", "InstanceId", aws_instance.ploy.id]
          ]
          period = 300
          stat   = "Average"
        }
      },
      {
        type   = "metric"
        x      = 12
        y      = 0
        width  = 12
        height = 6
        properties = {
          title  = "RDS CPU Utilization"
          region = var.aws_region
          metrics = [
            ["AWS/RDS", "CPUUtilization", "DBInstanceIdentifier", aws_db_instance.ploy.identifier]
          ]
          period = 300
          stat   = "Average"
        }
      },
      {
        type   = "metric"
        x      = 0
        y      = 6
        width  = 12
        height = 6
        properties = {
          title  = "EC2 Network"
          region = var.aws_region
          metrics = [
            ["AWS/EC2", "NetworkIn", "InstanceId", aws_instance.ploy.id],
            ["AWS/EC2", "NetworkOut", "InstanceId", aws_instance.ploy.id]
          ]
          period = 300
          stat   = "Sum"
        }
      },
      {
        type   = "metric"
        x      = 12
        y      = 6
        width  = 12
        height = 6
        properties = {
          title  = "RDS Connections"
          region = var.aws_region
          metrics = [
            ["AWS/RDS", "DatabaseConnections", "DBInstanceIdentifier", aws_db_instance.ploy.identifier]
          ]
          period = 300
          stat   = "Average"
        }
      },
      {
        type   = "log"
        x      = 0
        y      = 12
        width  = 24
        height = 6
        properties = {
          title  = "Application Logs"
          region = var.aws_region
          query  = "SOURCE '/${var.project_name}/application' | fields @timestamp, @message | sort @timestamp desc | limit 100"
        }
      }
    ]
  })
}
