# ECS自动部署说明

## 快速开始

### 1. 创建ECS实例
```bash
./create-ecs-instance.sh
```

这个脚本会：
- ✅ 创建一个**2核2G抢占式实例**（约0.05元/小时）
- ✅ 使用**按流量付费**带宽（峰值100Mbps）
- ✅ 自动配置安全组（允许SSH访问）
- ✅ 创建并保存SSH密钥对
- ✅ 等待实例就绪
- ✅ 保存实例信息到 `.ecs-info`

### 2. 部署Collector
```bash
./deploy-collector-auto.sh
```

这个脚本会：
- ✅ 读取 `.ecs-info` 中的ECS信息
- ✅ 编译最新的collector
- ✅ 上传到ECS实例
- ✅ 配置systemd服务
- ✅ 启动collector服务

## 配置说明

### 实例规格
- **规格**: ecs.t6-c1m2.large (2核2G)
- **计费**: 抢占式实例，价格上限0.05元/小时
- **带宽**: 按流量付费，峰值100Mbps
- **磁盘**: 40GB高效云盘

### 预估成本
- **实例费用**: ~0.05元/小时 = ~36元/月
- **流量费用**: 按实际使用，约0.8元/GB
- **总计**: 约40-50元/月（假设每天使用10GB流量）

## 管理命令

### 查看实例状态
```bash
source .ecs-info
aliyun ecs DescribeInstances --InstanceIds [$INSTANCE_ID] --region $REGION
```

### SSH登录
```bash
source .ecs-info
ssh -i $KEY_FILE root@$PUBLIC_IP
```

### 查看服务日志
```bash
source .ecs-info
ssh -i $KEY_FILE root@$PUBLIC_IP 'journalctl -u hft-collector -f'
```

### 停止/启动实例
```bash
source .ecs-info

# 停止（节省费用）
aliyun ecs StopInstance --InstanceId $INSTANCE_ID --region $REGION

# 启动
aliyun ecs StartInstance --InstanceId $INSTANCE_ID --region $REGION
```

### 删除实例
```bash
source .ecs-info
aliyun ecs DeleteInstance --InstanceId $INSTANCE_ID --Force true --region $REGION
rm .ecs-info
```

## 注意事项

1. **抢占式实例**：可能在资源紧张时被回收，建议配置告警
2. **SSH密钥**：请妥善保管 `hft-collector-key-new.pem` 文件
3. **安全组**：默认只开放22端口，如需其他端口请手动添加
4. **成本控制**：不使用时记得停止实例

## 故障排查

### 实例创建失败
- 检查账户余额是否充足
- 检查区域可用区是否有资源
- 尝试更换其他可用区

### SSH连接失败
- 检查公网IP是否正确
- 检查安全组规则是否允许SSH
- 检查密钥文件权限（应为600）

### 服务启动失败
- 查看日志：`journalctl -u hft-collector -n 100`
- 检查ClickHouse连接配置
- 检查二进制文件是否正确上传
