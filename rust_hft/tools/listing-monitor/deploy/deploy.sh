#!/bin/bash
# Listing Monitor ECS Deployment Script
#
# Usage:
#   ./deploy.sh build    - Build Docker image
#   ./deploy.sh push     - Push to ECR
#   ./deploy.sh deploy   - Deploy to ECS
#   ./deploy.sh all      - Build, push, and deploy

set -e

# Configuration
AWS_REGION="${AWS_REGION:-ap-northeast-1}"
AWS_ACCOUNT_ID="${AWS_ACCOUNT_ID:-$(aws sts get-caller-identity --query Account --output text)}"
ECR_REGISTRY="${ECR_REGISTRY:-${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION}.amazonaws.com}"
IMAGE_NAME="listing-monitor"
IMAGE_TAG="${IMAGE_TAG:-latest}"
ECS_CLUSTER="${ECS_CLUSTER:-default}"
ECS_SERVICE="${ECS_SERVICE:-listing-monitor}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

build() {
    log_info "Building Docker image..."
    cd "$PROJECT_DIR"
    docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" .
    log_info "Build complete: ${IMAGE_NAME}:${IMAGE_TAG}"
}

push() {
    log_info "Logging into ECR..."
    aws ecr get-login-password --region "$AWS_REGION" | \
        docker login --username AWS --password-stdin "$ECR_REGISTRY"

    # Create repository if it doesn't exist
    aws ecr describe-repositories --repository-names "$IMAGE_NAME" --region "$AWS_REGION" 2>/dev/null || \
        aws ecr create-repository --repository-name "$IMAGE_NAME" --region "$AWS_REGION"

    log_info "Tagging image..."
    docker tag "${IMAGE_NAME}:${IMAGE_TAG}" "${ECR_REGISTRY}/${IMAGE_NAME}:${IMAGE_TAG}"

    log_info "Pushing to ECR..."
    docker push "${ECR_REGISTRY}/${IMAGE_NAME}:${IMAGE_TAG}"
    log_info "Push complete: ${ECR_REGISTRY}/${IMAGE_NAME}:${IMAGE_TAG}"
}

deploy() {
    log_info "Registering task definition..."

    # Substitute environment variables in task definition
    TASK_DEF=$(cat "$SCRIPT_DIR/ecs-task-def.json" | \
        sed "s/\${AWS_ACCOUNT_ID}/${AWS_ACCOUNT_ID}/g" | \
        sed "s/\${ECR_REGISTRY}/${ECR_REGISTRY}/g" | \
        sed "s/\${IMAGE_TAG}/${IMAGE_TAG}/g")

    echo "$TASK_DEF" > /tmp/task-def-rendered.json

    TASK_DEF_ARN=$(aws ecs register-task-definition \
        --cli-input-json file:///tmp/task-def-rendered.json \
        --region "$AWS_REGION" \
        --query 'taskDefinition.taskDefinitionArn' \
        --output text)

    log_info "Task definition registered: $TASK_DEF_ARN"

    # Check if service exists
    SERVICE_EXISTS=$(aws ecs describe-services \
        --cluster "$ECS_CLUSTER" \
        --services "$ECS_SERVICE" \
        --region "$AWS_REGION" \
        --query 'services[0].status' \
        --output text 2>/dev/null || echo "MISSING")

    if [ "$SERVICE_EXISTS" = "ACTIVE" ]; then
        log_info "Updating existing service..."
        aws ecs update-service \
            --cluster "$ECS_CLUSTER" \
            --service "$ECS_SERVICE" \
            --task-definition "$TASK_DEF_ARN" \
            --region "$AWS_REGION" \
            --force-new-deployment > /dev/null
    else
        log_info "Creating new service..."
        # You'll need to configure subnet and security group IDs
        log_warn "Service doesn't exist. Please create it manually or provide VPC configuration:"
        echo "aws ecs create-service \\"
        echo "    --cluster $ECS_CLUSTER \\"
        echo "    --service-name $ECS_SERVICE \\"
        echo "    --task-definition $TASK_DEF_ARN \\"
        echo "    --desired-count 1 \\"
        echo "    --launch-type FARGATE \\"
        echo "    --network-configuration 'awsvpcConfiguration={subnets=[subnet-xxx],securityGroups=[sg-xxx],assignPublicIp=ENABLED}'"
        return 1
    fi

    log_info "Deployment initiated. Waiting for service to stabilize..."
    aws ecs wait services-stable \
        --cluster "$ECS_CLUSTER" \
        --services "$ECS_SERVICE" \
        --region "$AWS_REGION"

    log_info "Deployment complete!"
}

status() {
    log_info "Checking service status..."
    aws ecs describe-services \
        --cluster "$ECS_CLUSTER" \
        --services "$ECS_SERVICE" \
        --region "$AWS_REGION" \
        --query 'services[0].{Status:status,RunningCount:runningCount,DesiredCount:desiredCount,TaskDefinition:taskDefinition}' \
        --output table
}

logs() {
    STREAM="${1:-alpha}"
    log_info "Fetching logs for $STREAM monitor..."
    aws logs tail "/ecs/listing-monitor" \
        --region "$AWS_REGION" \
        --filter-pattern "$STREAM" \
        --follow
}

case "${1:-help}" in
    build)
        build
        ;;
    push)
        push
        ;;
    deploy)
        deploy
        ;;
    all)
        build
        push
        deploy
        ;;
    status)
        status
        ;;
    logs)
        logs "${2:-alpha}"
        ;;
    help|*)
        echo "Listing Monitor Deployment Script"
        echo ""
        echo "Usage: $0 <command>"
        echo ""
        echo "Commands:"
        echo "  build   - Build Docker image locally"
        echo "  push    - Push image to ECR"
        echo "  deploy  - Deploy to ECS Fargate"
        echo "  all     - Build, push, and deploy"
        echo "  status  - Check service status"
        echo "  logs    - Tail CloudWatch logs (alpha|spot)"
        echo ""
        echo "Environment variables:"
        echo "  AWS_REGION      - AWS region (default: ap-southeast-1)"
        echo "  AWS_ACCOUNT_ID  - AWS account ID (auto-detected)"
        echo "  ECR_REGISTRY    - ECR registry URL"
        echo "  IMAGE_TAG       - Docker image tag (default: latest)"
        echo "  ECS_CLUSTER     - ECS cluster name (default: default)"
        echo "  ECS_SERVICE     - ECS service name (default: listing-monitor)"
        ;;
esac
