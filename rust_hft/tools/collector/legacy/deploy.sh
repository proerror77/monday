#!/bin/bash

# hft-collector AWS ECS 自动部署脚本
# 使用方法: ./deploy.sh

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== hft-collector AWS ECS 部署脚本 ===${NC}"

# 检查前置条件
echo -e "${YELLOW}检查前置条件...${NC}"

if ! command -v aws &> /dev/null; then
    echo -e "${RED}错误: AWS CLI 未安装${NC}"
    exit 1
fi

if ! command -v docker &> /dev/null; then
    echo -e "${RED}错误: Docker 未安装${NC}"
    exit 1
fi

# 设置变量
REGION="ap-northeast-1"
APP_NAME="hft-collector"
ECR_STACK="${APP_NAME}-ecr"
ECS_STACK="${APP_NAME}-ecs"
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"

echo -e "${YELLOW}配置 AWS 区域为 ${REGION}...${NC}"
aws configure set default.region $REGION

echo -e "${YELLOW}验证 AWS 身份...${NC}"
aws sts get-caller-identity

# 步骤1: 创建 ECR 仓库
echo -e "${GREEN}步骤1: 创建 ECR 仓库...${NC}"

if aws cloudformation describe-stacks --stack-name $ECR_STACK &>/dev/null; then
    echo -e "${YELLOW}ECR 栈已存在，跳过创建${NC}"
else
    aws cloudformation create-stack \
      --stack-name $ECR_STACK \
      --template-body file://ecr-template.yaml \
      --parameters ParameterKey=AppName,ParameterValue=$APP_NAME
    
    echo -e "${YELLOW}等待 ECR 栈创建完成...${NC}"
    aws cloudformation wait stack-create-complete --stack-name $ECR_STACK
fi

# 获取 ECR URI
ECR_URI=$(aws cloudformation describe-stacks \
  --stack-name $ECR_STACK \
  --query 'Stacks[0].Outputs[?OutputKey==`ECRRepositoryURI`].OutputValue' \
  --output text)

echo -e "${GREEN}ECR Repository URI: ${ECR_URI}${NC}"

# 步骤2: 构建并推送镜像
echo -e "${GREEN}步骤2: 构建并推送 Docker 镜像...${NC}"

# 登录 ECR
echo -e "${YELLOW}登录到 ECR...${NC}"
aws ecr get-login-password --region $REGION | docker login --username AWS --password-stdin $ECR_URI

# 构建镜像 (需要在 rust_hft 根目录)
echo -e "${YELLOW}构建 Docker 镜像...${NC}"
cd /Users/proerror/Documents/monday/rust_hft
docker build -f apps/collector/Dockerfile -t $APP_NAME:latest .

# 标记和推送镜像
echo -e "${YELLOW}推送镜像到 ECR...${NC}"
docker tag $APP_NAME:latest $ECR_URI:latest
docker push $ECR_URI:latest

# 回到 collector 目录
cd /Users/proerror/Documents/monday/rust_hft/apps/collector

# 步骤3: 部署 ECS 基础设施
echo -e "${GREEN}步骤3: 部署 ECS 基础设施...${NC}"

if aws cloudformation describe-stacks --stack-name $ECS_STACK &>/dev/null; then
    echo -e "${YELLOW}更新现有 ECS 栈...${NC}"
    aws cloudformation update-stack \
      --stack-name $ECS_STACK \
      --template-body file://ecs-infrastructure.yaml \
      --parameters \
        ParameterKey=AppName,ParameterValue=$APP_NAME \
        ParameterKey=ECRRepositoryURI,ParameterValue=$ECR_URI:latest \
        ParameterKey=ClickHouseURL,ParameterValue=$CLICKHOUSE_URL \
        ParameterKey=ClickHouseUser,ParameterValue=$CLICKHOUSE_USER \
        ParameterKey=ClickHousePassword,ParameterValue=$CLICKHOUSE_PASSWORD \
      --capabilities CAPABILITY_IAM
    
    echo -e "${YELLOW}等待 ECS 栈更新完成...${NC}"
    aws cloudformation wait stack-update-complete --stack-name $ECS_STACK
else
    aws cloudformation create-stack \
      --stack-name $ECS_STACK \
      --template-body file://ecs-infrastructure.yaml \
      --parameters \
        ParameterKey=AppName,ParameterValue=$APP_NAME \
        ParameterKey=ECRRepositoryURI,ParameterValue=$ECR_URI:latest \
        ParameterKey=ClickHouseURL,ParameterValue=$CLICKHOUSE_URL \
        ParameterKey=ClickHouseUser,ParameterValue=$CLICKHOUSE_USER \
        ParameterKey=ClickHousePassword,ParameterValue=$CLICKHOUSE_PASSWORD \
      --capabilities CAPABILITY_IAM
    
    echo -e "${YELLOW}等待 ECS 栈创建完成 (约 5-10 分钟)...${NC}"
    aws cloudformation wait stack-create-complete --stack-name $ECS_STACK
fi

# 获取资源信息
CLUSTER_NAME=$(aws cloudformation describe-stacks \
  --stack-name $ECS_STACK \
  --query 'Stacks[0].Outputs[?OutputKey==`ClusterName`].OutputValue' \
  --output text)

SERVICE_NAME=$(aws cloudformation describe-stacks \
  --stack-name $ECS_STACK \
  --query 'Stacks[0].Outputs[?OutputKey==`ServiceName`].OutputValue' \
  --output text)

LOG_GROUP=$(aws cloudformation describe-stacks \
  --stack-name $ECS_STACK \
  --query 'Stacks[0].Outputs[?OutputKey==`LogGroupName`].OutputValue' \
  --output text)

# 步骤4: 验证部署
echo -e "${GREEN}步骤4: 验证部署...${NC}"

echo -e "${YELLOW}检查服务状态...${NC}"
aws ecs describe-services \
  --cluster $CLUSTER_NAME \
  --services $SERVICE_NAME \
  --query 'services[0].{Status:status,Running:runningCount,Desired:desiredCount}' \
  --output table

echo -e "${YELLOW}检查任务状态...${NC}"
TASK_ARN=$(aws ecs list-tasks --cluster $CLUSTER_NAME --service-name $SERVICE_NAME --query 'taskArns[0]' --output text)

if [[ "$TASK_ARN" != "None" && "$TASK_ARN" != "" ]]; then
    aws ecs describe-tasks --cluster $CLUSTER_NAME --tasks $TASK_ARN \
      --query 'tasks[0].{LastStatus:lastStatus,HealthStatus:healthStatus,CreatedAt:createdAt}' \
      --output table
else
    echo -e "${YELLOW}暂无运行中的任务${NC}"
fi

echo -e "${GREEN}=== 部署完成 ===${NC}"
echo -e "${GREEN}集群名称: ${CLUSTER_NAME}${NC}"
echo -e "${GREEN}服务名称: ${SERVICE_NAME}${NC}"
echo -e "${GREEN}日志组: ${LOG_GROUP}${NC}"

echo -e "${YELLOW}查看日志命令:${NC}"
echo "aws logs tail $LOG_GROUP --follow"

echo -e "${YELLOW}重启服务命令:${NC}"
echo "aws ecs update-service --cluster $CLUSTER_NAME --service $SERVICE_NAME --force-new-deployment"

echo -e "${GREEN}hft-collector 已成功部署到 AWS ECS！${NC}"
echo -e "${GREEN}它将 24/7 收集 Bitget 数据到你的 ClickHouse Cloud 实例。${NC}"