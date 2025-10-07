#!/bin/bash

# AWS HFT ML系统部署脚本
# 使用 AWS CLI 执行完整的部署流程 (优化版本)

set -e  # 遇到错误立即退出

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 配置参数 - 使用查询到的实际配置
AWS_ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)
AWS_REGION="ap-northeast-1"  # 用户当前配置的东京区域
BUCKET_NAME="hft-ml-bucket-$AWS_ACCOUNT_ID"  # 更清晰的命名
SAGEMAKER_ROLE_NAME="SageMakerHFTExecutionRole"
BATCH_ROLE_NAME="BatchHFTExecutionRole"
BATCH_COMPUTE_ENV_NAME="crypto-backtest-env"
BATCH_QUEUE_NAME="crypto-backtest-queue"

echo -e "${GREEN}开始AWS HFT ML系统部署...${NC}"
echo "=========================================="
echo "AWS 账户 ID: $AWS_ACCOUNT_ID"
echo "AWS 区域: $AWS_REGION"
echo "S3 存储桶: $BUCKET_NAME"
echo "=========================================="

# 步骤1：检查AWS CLI配置
echo -e "\n${YELLOW}步骤1：检查AWS CLI配置${NC}"
aws sts get-caller-identity
echo -e "${GREEN}✅ AWS CLI配置正常${NC}"

# 步骤2：创建IAM角色
echo -e "\n${YELLOW}步骤2：创建IAM角色${NC}"

# 创建信任策略
TRUST_POLICY=$(cat <<EOF
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Service": "sagemaker.amazonaws.com"
      },
      "Action": "sts:AssumeRole"
    }
  ]
}
EOF
)

# 创建SageMaker执行角色
echo "创建SageMaker执行角色..."
if aws iam create-role \
    --role-name $SAGEMAKER_ROLE_NAME \
    --assume-role-policy-document "$TRUST_POLICY" \
    --description "SageMaker execution role for HFT ML training" 2>/dev/null; then
    echo -e "${GREEN}✅ SageMaker角色创建完成${NC}"

    # 附加必要权限
    echo "附加SageMakerFullAccess策略..."
    aws iam attach-role-policy \
        --role-name $SAGEMAKER_ROLE_NAME \
        --policy-arn arn:aws:iam::aws:policy/AmazonSageMakerFullAccess

    echo "附加S3FullAccess策略..."
    aws iam attach-role-policy \
        --role-name $SAGEMAKER_ROLE_NAME \
        --policy-arn arn:aws:iam::aws:policy/AmazonS3FullAccess

    echo "附加CloudWatchFullAccess策略..."
    aws iam attach-role-policy \
        --role-name $SAGEMAKER_ROLE_NAME \
        --policy-arn arn:aws:iam::aws:policy/CloudWatchFullAccess
else
    echo -e "${YELLOW}⚠️  SageMaker角色已存在，使用现有角色${NC}"
fi

# 获取角色ARN
SAGEMAKER_ROLE_ARN=$(aws iam get-role --role-name $SAGEMAKER_ROLE_NAME --query Role.Arn --output text)
echo -e "${GREEN}✅ SageMaker角色ARN: $SAGEMAKER_ROLE_ARN${NC}"

# 步骤3：创建AWS Batch相关角色
echo -e "\n${YELLOW}步骤3：创建AWS Batch相关角色${NC}"

# 创建Batch执行角色信任策略
BATCH_TRUST_POLICY=$(cat <<EOF
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Service": "batch.amazonaws.com"
      },
      "Action": "sts:AssumeRole"
    }
  ]
}
EOF
)

echo "创建Batch执行角色..."
if aws iam create-role \
    --role-name $BATCH_ROLE_NAME \
    --assume-role-policy-document "$BATCH_TRUST_POLICY" \
    --description "AWS Batch execution role for HFT backtesting" 2>/dev/null; then
    echo -e "${GREEN}✅ Batch角色创建完成${NC}"

    # 附加Batch执行权限
    aws iam attach-role-policy \
        --role-name $BATCH_ROLE_NAME \
        --policy-arn arn:aws:iam::aws:policy/service-role/AWSBatchServiceRole
else
    echo -e "${YELLOW}⚠️  Batch角色已存在，使用现有角色${NC}"
fi

BATCH_ROLE_ARN=$(aws iam get-role --role-name $BATCH_ROLE_NAME --query Role.Arn --output text)
echo -e "${GREEN}✅ Batch角色创建完成: $BATCH_ROLE_ARN${NC}"

# 步骤4：创建S3存储桶
echo -e "\n${YELLOW}步骤4：创建S3存储桶${NC}"

# 检查存储桶是否已存在
if aws s3 ls "s3://$BUCKET_NAME" 2>&1 | grep -q 'NoSuchBucket'; then
    aws s3 mb s3://$BUCKET_NAME --region $AWS_REGION
    echo -e "${GREEN}✅ S3存储桶创建完成${NC}"
else
    echo -e "${YELLOW}⚠️  存储桶已存在，跳过创建${NC}"
fi

# 创建所需的目录结构
echo "创建S3目录结构..."
aws s3api put-object --bucket $BUCKET_NAME --key feats/
aws s3api put-object --bucket $BUCKET_NAME --key manifests/
aws s3api put-object --bucket $BUCKET_NAME --key models/
aws s3api put-object --bucket $BUCKET_NAME --key results/
aws s3api put-object --bucket $BUCKET_NAME --key results/backtest_metrics/
echo -e "${GREEN}✅ S3结构创建完成${NC}"

# 步骤5：创建AWS Batch计算环境
echo -e "\n${YELLOW}步骤5：创建AWS Batch计算环境${NC}"

# 获取默认VPC和子网
DEFAULT_VPC=$(aws ec2 describe-vpcs --filters Name=isDefault,Values=true --query Vpcs[0].VpcId --output text)
echo "默认VPC ID: $DEFAULT_VPC"

# 获取默认子网
SUBNET_IDS=$(aws ec2 describe-subnets \
    --filters Name=vpc-id,Values=$DEFAULT_VPC Name=default-for-az,Values=true \
    --query Subnets[*].SubnetId --output text | tr '\t' ',')
echo "子网IDs: $SUBNET_IDS"

# 创建安全组
SG_NAME="batch-sg-$(date +%s)"
SG_ID=$(aws ec2 create-security-group \
    --group-name $SG_NAME \
    --description "Security group for AWS Batch" \
    --vpc-id $DEFAULT_VPC \
    --query GroupId --output text)
echo "安全组ID: $SG_ID"

# 创建计算环境
echo "创建Batch计算环境..."
aws batch create-compute-environment \
    --compute-environment-name $BATCH_COMPUTE_ENV_NAME \
    --type MANAGED \
    --state ENABLED \
    --compute-resources "type=EC2,minvCpus=0,maxvCpus=256,instanceTypes=[\"c5.large\",\"c5.xlarge\"],subnets=[\"$(echo $SUBNET_IDS | sed 's/,/","/g')\"],securityGroupIds=[\"$SG_ID\"],instanceRole=ecsInstanceRole"
echo -e "${GREEN}✅ 计算环境创建完成${NC}"

# 步骤6：创建作业队列
echo -e "\n${YELLOW}步骤6：创建作业队列${NC}"

# 等待计算环境变为可用状态
echo "等待计算环境变为可用状态..."
aws batch wait compute-environment-available --compute-environment $BATCH_COMPUTE_ENV_NAME

echo "创建作业队列..."
aws batch create-job-queue \
    --job-queue-name $BATCH_QUEUE_NAME \
    --priority 1 \
    --state ENABLED \
    --compute-environment-order "[{\"order\": 1, \"computeEnvironment\": \"$BATCH_COMPUTE_ENV_NAME\"}]"
echo -e "${GREEN}✅ 作业队列创建完成${NC}"

# 步骤7：生成环境变量文件
echo -e "\n${YELLOW}步骤7：生成环境变量配置文件${NC}"

cat > .env.aws <<EOF
# AWS SageMaker配置
SAGEMAKER_ROLE_ARN=$SAGEMAKER_ROLE_ARN
S3_BUCKET=$BUCKET_NAME
S3_PREFIX=feats/dt=2025-09-03
INSTANCE_TYPE=ml.g5.2xlarge
INSTANCE_COUNT=1
MODEL_TYPE=tcn
EPOCHS=3
SEQ_LEN=60
INPUT_MODE=FastFile
MODEL_PACKAGE_GROUP=crypto_subhf_v1

# AWS Batch配置
BATCH_COMPUTE_ENV_NAME=$BATCH_COMPUTE_ENV_NAME
BATCH_QUEUE_NAME=$BATCH_QUEUE_NAME
MANIFEST_S3=s3://$BUCKET_NAME/manifests/manifest.csv
MODEL_S3=s3://$BUCKET_NAME/models/model.onnx
OUTPUT_S3=s3://$BUCKET_NAME/results/backtest_metrics

# AWS配置
AWS_REGION=$AWS_REGION
AWS_ACCOUNT_ID=$AWS_ACCOUNT_ID

# 网络配置
SUBNET_IDS=$SUBNET_IDS
SECURITY_GROUP_IDS=$SG_ID
EOF

echo -e "${GREEN}✅ 环境变量文件生成: .env.aws${NC}"

# 步骤8：验证部署
echo -e "\n${YELLOW}步骤8：验证部署${NC}"

echo "验证IAM角色..."
aws iam get-role --role-name $SAGEMAKER_ROLE_NAME --query Role.RoleName --output text
aws iam get-role --role-name $BATCH_ROLE_NAME --query Role.RoleName --output text

echo "验证S3存储桶..."
aws s3 ls s3://$BUCKET_NAME/

echo "验证Batch组件..."
aws batch describe-compute-environments --compute-environments $BATCH_COMPUTE_ENV_NAME --query computeEnvironments[0].status
aws batch describe-job-queues --job-queues $BATCH_QUEUE_NAME --query jobQueues[0].status

echo -e "\n${GREEN}🎉 AWS HFT ML系统基础设施部署完成！${NC}"
echo "=========================================="
echo -e "${YELLOW}下一步操作：${NC}"
echo "1. 请将您的Parquet数据文件上传到 s3://$BUCKET_NAME/feats/dt=2025-09-03/sym=BTCUSDT/ 等路径"
echo "2. 运行以下命令开始训练："
echo "   source .env.aws && python submit_train.py"
echo "3. HPO优化："
echo "   source .env.aws && python submit_hpo.py"
echo "=========================================="

echo -e "\n${GREEN}✅ 部署验证完成${NC}"
