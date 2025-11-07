# hft-collector AWS ECS 部署指南

## 概述
本指南将帮助你在 AWS 东京区域 (ap-northeast-1) 部署 hft-collector，用于24小时收集 Bitget 数据到 ClickHouse Cloud。

## 前提条件
1. AWS CLI 已安装并配置
2. 你的 AWS 密钥文件：`/Users/proerror/Downloads/aws_tango_1.pem`
3. Docker 已安装
4. ClickHouse Cloud 实例已就绪：`ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443`

## 部署步骤

### 第一步：设置 AWS CLI 和区域

```bash
# 设置默认区域为东京
aws configure set default.region ap-northeast-1

# 验证配置
aws sts get-caller-identity
```

### 第二步：创建 ECR 仓库

```bash
# 进入 collector 目录
cd /Users/proerror/Documents/monday/rust_hft/apps/collector

# 部署 ECR CloudFormation 栈
aws cloudformation create-stack \
  --stack-name hft-collector-ecr \
  --template-body file://ecr-template.yaml \
  --parameters ParameterKey=AppName,ParameterValue=hft-collector

# 等待栈创建完成
aws cloudformation wait stack-create-complete --stack-name hft-collector-ecr

# 获取 ECR Repository URI
ECR_URI=$(aws cloudformation describe-stacks \
  --stack-name hft-collector-ecr \
  --query 'Stacks[0].Outputs[?OutputKey==`ECRRepositoryURI`].OutputValue' \
  --output text)

echo "ECR Repository URI: $ECR_URI"
```

### 第三步：构建并推送 Docker 镜像

```bash
# 登录到 ECR
aws ecr get-login-password --region ap-northeast-1 | docker login --username AWS --password-stdin $ECR_URI

# 构建镜像 (在 rust_hft 根目录)
cd /Users/proerror/Documents/monday/rust_hft
docker build -f apps/collector/Dockerfile -t hft-collector:latest .

# 标记镜像
docker tag hft-collector:latest $ECR_URI:latest

# 推送镜像
docker push $ECR_URI:latest
```

### 第四步：部署 ECS 基础设施

```bash
# 回到 collector 目录
cd /Users/proerror/Documents/monday/rust_hft/apps/collector

# 部署 ECS 基础设施栈
aws cloudformation create-stack \
  --stack-name hft-collector-ecs \
  --template-body file://ecs-infrastructure.yaml \
  --parameters \
    ParameterKey=AppName,ParameterValue=hft-collector \
    ParameterKey=ECRRepositoryURI,ParameterValue=$ECR_URI:latest \
    ParameterKey=ClickHouseURL,ParameterValue=https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443 \
    ParameterKey=ClickHouseUser,ParameterValue=default \
    ParameterKey=ClickHousePassword,ParameterValue=sIiFK.4ygf.9R \
  --capabilities CAPABILITY_IAM

# 等待栈创建完成 (大约 5-10 分钟)
aws cloudformation wait stack-create-complete --stack-name hft-collector-ecs
```

### 第五步：验证部署

```bash
# 获取 ECS 集群和服务信息
CLUSTER_NAME=$(aws cloudformation describe-stacks \
  --stack-name hft-collector-ecs \
  --query 'Stacks[0].Outputs[?OutputKey==`ClusterName`].OutputValue' \
  --output text)

SERVICE_NAME=$(aws cloudformation describe-stacks \
  --stack-name hft-collector-ecs \
  --query 'Stacks[0].Outputs[?OutputKey==`ServiceName`].OutputValue' \
  --output text)

# 检查服务状态
aws ecs describe-services \
  --cluster $CLUSTER_NAME \
  --services $SERVICE_NAME

# 查看任务状态
aws ecs list-tasks --cluster $CLUSTER_NAME --service-name $SERVICE_NAME

# 获取任务详情
TASK_ARN=$(aws ecs list-tasks --cluster $CLUSTER_NAME --service-name $SERVICE_NAME --query 'taskArns[0]' --output text)
aws ecs describe-tasks --cluster $CLUSTER_NAME --tasks $TASK_ARN
```

### 第六步：查看日志

```bash
# 获取 CloudWatch 日志组名
LOG_GROUP=$(aws cloudformation describe-stacks \
  --stack-name hft-collector-ecs \
  --query 'Stacks[0].Outputs[?OutputKey==`LogGroupName`].OutputValue' \
  --output text)

# 查看最新日志
aws logs describe-log-streams --log-group-name $LOG_GROUP

# 获取日志流名称并查看日志
LOG_STREAM=$(aws logs describe-log-streams \
  --log-group-name $LOG_GROUP \
  --order-by LastEventTime \
  --descending \
  --max-items 1 \
  --query 'logStreams[0].logStreamName' \
  --output text)

aws logs get-log-events \
  --log-group-name $LOG_GROUP \
  --log-stream-name $LOG_STREAM \
  --start-from-head
```

## 监控和维护

### CloudWatch 指标
- CPU 使用率：`AWS/ECS` 命名空间
- 内存使用率：`AWS/ECS` 命名空间
- 任务计数：`AWS/ECS` 命名空间

### 重启服务
```bash
aws ecs update-service \
  --cluster $CLUSTER_NAME \
  --service $SERVICE_NAME \
  --force-new-deployment
```

### 更新镜像
```bash
# 重新构建和推送镜像
docker build -f apps/collector/Dockerfile -t hft-collector:latest .
docker tag hft-collector:latest $ECR_URI:latest
docker push $ECR_URI:latest

# 强制重新部署
aws ecs update-service \
  --cluster $CLUSTER_NAME \
  --service $SERVICE_NAME \
  --force-new-deployment
```

### 扩缩容
```bash
# 调整实例数量
aws ecs update-service \
  --cluster $CLUSTER_NAME \
  --service $SERVICE_NAME \
  --desired-count 2
```

## 成本优化

### 使用 Spot 容量（可选）
修改任务定义以使用 Spot 实例：

```bash
# 创建新的容量提供程序
aws ecs create-capacity-provider \
  --name fargate-spot \
  --fargate-capacity-provider-config {}

# 更新集群以使用 Spot
aws ecs put-cluster-capacity-providers \
  --cluster $CLUSTER_NAME \
  --capacity-providers FARGATE FARGATE_SPOT \
  --default-capacity-provider-strategy capacityProvider=FARGATE_SPOT,weight=1
```

## 故障排除

### 常见问题

1. **任务启动失败**
   ```bash
   # 检查任务定义
   aws ecs describe-task-definition --task-definition hft-collector-task
   
   # 检查停止原因
   aws ecs describe-tasks --cluster $CLUSTER_NAME --tasks $TASK_ARN
   ```

2. **网络连接问题**
   ```bash
   # 检查安全组规则
   aws ec2 describe-security-groups --group-names hft-collector-ecs-security-group
   
   # 检查子网路由
   aws ec2 describe-route-tables
   ```

3. **镜像拉取失败**
   ```bash
   # 验证 ECR 权限
   aws ecr describe-repositories --repository-names hft-collector
   
   # 重新登录 ECR
   aws ecr get-login-password --region ap-northeast-1 | docker login --username AWS --password-stdin $ECR_URI
   ```

## 清理资源

```bash
# 删除 ECS 栈
aws cloudformation delete-stack --stack-name hft-collector-ecs

# 删除 ECR 栈（会删除镜像）
aws cloudformation delete-stack --stack-name hft-collector-ecr

# 等待删除完成
aws cloudformation wait stack-delete-complete --stack-name hft-collector-ecs
aws cloudformation wait stack-delete-complete --stack-name hft-collector-ecr
```

## 预计成本（月）
- **Fargate (512 CPU, 1024 MB, 24/7)**: ~$15-20
- **CloudWatch Logs (7天保留)**: ~$2-5  
- **ECR 存储**: ~$1-2
- **数据传出**: ~$5-10
- **总计**: ~$23-37/月

你的 hft-collector 现在应该在 AWS ECS 上 24/7 运行，持续收集 Bitget 数据到你的 ClickHouse Cloud 实例！