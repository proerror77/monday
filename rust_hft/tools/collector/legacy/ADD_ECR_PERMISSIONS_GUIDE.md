# 如何增加 ECR 权限给你的 AWS 用户

当前你的 AWS 用户 `proerror` 在账号 `279630655471` 中缺少 ECR 权限。以下是几种解决方法：

## 方法一：通过 AWS 控制台（推荐）

### 步骤1：登录 AWS 控制台
1. 访问 [AWS 控制台](https://console.aws.amazon.com/)
2. 使用管理员账户或有 IAM 权限的账户登录

### 步骤2：进入 IAM 服务
1. 在服务搜索框输入 "IAM"
2. 点击 "IAM" 服务

### 步骤3：找到用户并添加权限
1. 在左侧菜单点击 "Users"（用户）
2. 找到并点击用户 `proerror`
3. 点击 "Permissions"（权限）标签
4. 点击 "Add permissions"（添加权限）

### 步骤4：附加策略
有两种选择：

**选择 A：使用 AWS 管理策略（推荐）**
1. 选择 "Attach existing policies directly"（直接附加现有策略）
2. 搜索并勾选以下策略：
   - `AmazonEC2ContainerRegistryFullAccess` （ECR 完全访问）
   - `AmazonECS_FullAccess` （ECS 完全访问）
   - `CloudFormationFullAccess` （CloudFormation 完全访问）
   - `AmazonEC2FullAccess` （VPC 网络权限）
   - `CloudWatchLogsFullAccess` （日志权限）
3. 点击 "Next: Review"
4. 点击 "Add permissions"

**选择 B：创建自定义策略**
1. 选择 "Create policy"（创建策略）
2. 点击 "JSON" 标签
3. 复制粘贴 `ecs-full-permissions-policy.json` 的内容
4. 点击 "Next: Tags"
5. 点击 "Next: Review"
6. 策略名称输入：`HFTCollectorDeploymentPolicy`
7. 点击 "Create policy"
8. 返回用户权限页面，附加刚创建的策略

## 方法二：通过 AWS CLI（需要管理员权限）

如果你有另一个具有管理员权限的 AWS profile：

### 步骤1：切换到管理员 profile
```bash
aws configure --profile admin
# 或者
export AWS_PROFILE=admin
```

### 步骤2：创建并附加策略
```bash
# 创建 ECR 策略
aws iam create-policy \
    --policy-name HFTCollectorECRPolicy \
    --policy-document file://ecr-permissions-policy.json

# 创建完整策略
aws iam create-policy \
    --policy-name HFTCollectorDeploymentPolicy \
    --policy-document file://ecs-full-permissions-policy.json

# 附加策略到用户
aws iam attach-user-policy \
    --user-name proerror \
    --policy-arn arn:aws:iam::279630655471:policy/HFTCollectorDeploymentPolicy
```

### 步骤3：验证权限
```bash
# 切换回 proerror profile
export AWS_PROFILE=default

# 测试 ECR 权限
aws ecr describe-repositories --region ap-northeast-1
```

## 方法三：使用 Root 用户

如果你是账号的 root 用户：

1. 使用 root 账号登录 AWS 控制台
2. 按照方法一的步骤进行操作

## 方法四：联系 AWS 管理员

如果你不是管理员，需要联系你的 AWS 管理员：

### 请求的权限内容：
```
请为用户 proerror (arn:aws:iam::279630655471:user/proerror) 添加以下 AWS 管理策略：

1. AmazonEC2ContainerRegistryFullAccess
2. AmazonECS_FullAccess  
3. CloudFormationFullAccess
4. AmazonEC2FullAccess
5. CloudWatchLogsFullAccess

或者创建自定义策略，内容见附加的 ecs-full-permissions-policy.json 文件。

这些权限用于部署 hft-collector 容器应用到 ECS。
```

## 验证权限是否生效

权限添加后，运行以下命令验证：

```bash
# 测试 ECR 权限
aws ecr describe-repositories --region ap-northeast-1

# 测试 ECS 权限  
aws ecs list-clusters --region ap-northeast-1

# 测试 CloudFormation 权限
aws cloudformation list-stacks --region ap-northeast-1
```

## 最小权限替代方案

如果不能获得完全权限，你可以请求以下最小权限集合：

### ECR 最小权限：
- `ecr:CreateRepository`
- `ecr:DescribeRepositories` 
- `ecr:GetAuthorizationToken`
- `ecr:BatchCheckLayerAvailability`
- `ecr:InitiateLayerUpload`
- `ecr:UploadLayerPart`
- `ecr:CompleteLayerUpload`
- `ecr:PutImage`

### ECS 最小权限：
- `ecs:CreateCluster`
- `ecs:CreateService`
- `ecs:RegisterTaskDefinition`
- `ecs:RunTask`
- `ecs:DescribeClusters`
- `ecs:DescribeServices`
- `ecs:DescribeTasks`

添加权限后，请重新运行部署脚本：
```bash
cd /Users/proerror/Documents/monday/rust_hft/apps/collector
./deploy.sh
```