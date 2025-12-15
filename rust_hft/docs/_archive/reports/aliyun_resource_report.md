# 阿里云资源使用情况报告

**生成时间**: 2025-10-03
**报告类型**: 全账户资源清单与成本分析

---

## 📊 执行摘要

### 资源概览
- **总ECS实例数**: 2台 (全部已停止)
- **主要区域**: 日本东京 (ap-northeast-1)
- **总VPC网络**: 6个
- **总安全组**: 12个
- **当前月费用**: ~$9.60 USD (仅磁盘存储)

### 关键发现
✅ 所有实例已停止，成本控制良好
⚠️  存在5个未使用的VPC和11个历史安全组
💡 建议进行资源清理以简化管理

---

## 1. ECS 实例详细清单

### 1.1 日本东京区域 (ap-northeast-1)

#### 实例 1: hft-collector-20250103
```
实例ID:      i-6wec6is4knvg5h9s3iya
实例名称:    hft-collector-20250103
状态:        已停止 ⏸️
规格:        ecs.e-c1m1.large
配置:        2核CPU / 2GB内存
操作系统:    Ubuntu 22.04 64位
计费方式:    按量付费 (PostPaid)

网络配置:
  VPC ID:    vpc-6wesy84ixw2esl6lb3ov5 (for trading)
  私有IP:    172.16.0.205
  公网IP:    无
  带宽:      100 Mbps

存储配置:
  磁盘ID:    d-6wed7x33nsqv6y2emhz5
  磁盘类型:  ESSD云盘
  容量:      40 GB
  计费方式:  按量付费

创建时间:   2025-10-03T11:06Z
到期时间:   2099-12-31T15:59Z (按量付费)
```

#### 实例 2: hft-collector-stable
```
实例ID:      i-6weih85eb3xwrvaewwgd
实例名称:    hft-collector-stable
状态:        已停止 ⏸️
规格:        ecs.c7.large
配置:        2核CPU / 4GB内存
操作系统:    Ubuntu 24.04 64位
计费方式:    按量付费 (PostPaid)

网络配置:
  VPC ID:    vpc-6wesy84ixw2esl6lb3ov5 (for trading)
  私有IP:    172.16.0.204
  公网IP:    无
  带宽:      10 Mbps

存储配置:
  磁盘ID:    d-6wecnufv9egxfufzondw
  磁盘类型:  ESSD云盘
  容量:      40 GB
  计费方式:  按量付费

创建时间:   2025-10-03T08:13Z
到期时间:   2099-12-31T15:59Z (按量付费)
```

---

## 2. 网络资源清单

### 2.1 VPC 网络 (6个)

#### 生产环境 VPC
| 区域 | VPC ID | 名称 | CIDR | 状态 | 创建时间 |
|------|--------|------|------|------|----------|
| **ap-northeast-1** | vpc-6wesy84ixw2esl6lb3ov5 | **for trading** | 172.16.0.0/12 | Available | 2024-06-01 |
| ap-northeast-1 | vpc-6weu5imjuiwcnwz8bklnh | - | 172.16.0.0/12 | Available | 2024-06-01 |

#### 历史/未使用 VPC
| 区域 | VPC ID | CIDR | 创建时间 | 建议 |
|------|--------|------|----------|------|
| cn-hangzhou | vpc-bp1tves2x4yl0007oer72 | 172.16.0.0/16 | 2017-10-25 | 🗑️ 可删除 |
| cn-shanghai | vpc-uf6c7pfebe6c98ijn2jtf | 172.19.0.0/16 | 2017-01-11 | 🗑️ 可删除 |
| cn-hongkong | vpc-j6cq1yyy74nb58tdh16zv | 10.0.0.0/8 | 2021-07-14 | 🗑️ 可删除 |
| cn-hongkong | vpc-j6ccl9738kizqwirho2cw | 172.31.0.0/16 | 2017-11-08 | 🗑️ 可删除 |
| us-west-1 | vpc-rj9abt8k0oeu50ekro8hl | 172.16.0.0/12 | 2023-03-01 | 🗑️ 可删除 |

### 2.2 安全组 (12个)

#### 生产环境安全组
| 区域 | 安全组ID | 名称 | VPC | 用途 |
|------|----------|------|-----|------|
| **ap-northeast-1** | **sg-6we9gm9bevx5u9s5rmpq** | **hft-collector-sg** | vpc-6wesy84ixw2esl6lb3ov5 | 🔒 生产环境 |
| ap-northeast-1 | sg-6we2pvdjmzmbm2o8fjzs | System SG | vpc-6weu5imjuiwcnwz8bklnh | 🗑️ 历史 |

#### 历史安全组 (建议清理)
- **cn-hangzhou**: 2个
- **cn-shanghai**: 4个
- **cn-hongkong**: 3个
- **us-west-1**: 1个

---

## 3. 成本分析

### 3.1 当前费用 (停机状态)

```
┌──────────────────────────────────────────┐
│ 当前月度费用 (已停止状态)                │
├──────────────────────────────────────────┤
│ ECS 实例费用:           $0.00            │
│ 磁盘存储费用:           ~$9.60           │
│                                          │
│ 总计:                   ~$9.60 USD/月    │
└──────────────────────────────────────────┘
```

**费用明细**:
- ESSD云盘 80GB × $0.12/GB/月 = **$9.60/月**
- 实例停机状态不产生计算费用

### 3.2 运行状态费用估算

如果两台实例24x7全天候运行:

```
┌──────────────────────────────────────────────────┐
│ 实例规格              运行费用    磁盘费用  合计  │
├──────────────────────────────────────────────────┤
│ ecs.e-c1m1.large     $21.60      $4.80    $26.40│
│ ecs.c7.large         $57.60      $4.80    $62.40│
│                                                  │
│ 总计 (两台全开):                         $88.80 │
└──────────────────────────────────────────────────┘

月度费用: ~$88.80 USD
年度费用: ~$1,065.60 USD
```

### 3.3 实例规格定价参考

#### ecs.e-c1m1.large (经济型)
- **配置**: 2核2GB
- **按量付费**: ~$0.03/小时
- **适用场景**:
  - 轻量级应用
  - 开发测试环境
  - 数据采集服务

#### ecs.c7.large (计算优化型)
- **配置**: 2核4GB
- **按量付费**: ~$0.08/小时
- **适用场景**:
  - 计算密集型应用
  - 高频交易系统 (HFT)
  - 实时数据处理

---

## 4. 优化建议

### 4.1 立即可执行 (成本优化)

#### ✅ 资源清理
```bash
优先级: 高 🔴
预计节省: 管理成本降低
执行时间: 30分钟

行动项:
□ 删除5个未使用的VPC
  - cn-hangzhou: vpc-bp1tves2x4yl0007oer72
  - cn-shanghai: vpc-uf6c7pfebe6c98ijn2jtf
  - cn-hongkong: vpc-j6cq1yyy74nb58tdh16zv, vpc-j6ccl9738kizqwirho2cw
  - us-west-1: vpc-rj9abt8k0oeu50ekro8hl

□ 删除11个历史安全组
  - 保留: ap-northeast-1 的生产环境安全组

□ 如果实例长期不用 (>30天):
  ✓ 创建镜像 (snapshot) 备份
  ✓ 释放实例
  ✓ 节省 $9.60/月磁盘费用
```

#### 💰 成本控制
```bash
优先级: 高 🔴
执行时间: 15分钟

行动项:
□ 设置预算告警
  - 建议阈值: $100/月
  - 告警比例: 80%, 100%

□ 启用资源监控
  - 跟踪实例运行时间
  - 监控流量使用

□ 考虑长期方案
  - 预留实例 (节省30-50%)
  - 节省计划
  - 仅在必要时使用
```

### 4.2 性能优化 (HFT系统)

#### 🚀 实例升级建议

基于您的 HFT 系统需求，当前配置分析:

**当前配置评估**:
```
ecs.c7.large (2核4GB)
├─ 延迟性能: 良好
├─ 适用场景: 轻-中量级 HFT
└─ 限制因素: CPU和内存可能成为瓶颈
```

**升级选项**:

1. **ecs.c7a.xlarge** (推荐)
   ```
   配置: 4核8GB
   处理器: AMD EPYC Milan (3.5GHz)
   费用: ~$0.16/小时 (~$115/月)
   优势:
   - 更低延迟 (AMD Milan)
   - 双倍计算资源
   - 适合中高频交易
   ```

2. **ecs.g7.xlarge** (内存密集)
   ```
   配置: 4核16GB
   处理器: Intel Ice Lake
   费用: ~$0.24/小时 (~$172/月)
   优势:
   - 更大内存缓冲
   - 适合大量数据缓存
   - 适合复杂策略
   ```

3. **ecs.c7.2xlarge** (高性能)
   ```
   配置: 8核16GB
   处理器: Intel Ice Lake (3.5GHz+)
   费用: ~$0.32/小时 (~$230/月)
   优势:
   - 最佳单核性能
   - 适合超低延迟需求
   - 生产环境推荐
   ```

#### 💾 存储优化

**当前**: ESSD云盘 (标准性能)
**建议升级**: ESSD PL3 (极致性能)

```
ESSD PL3 特性:
├─ IOPS: 高达 1,000,000
├─ 吞吐: 4,000 MB/s
├─ 延迟: 微秒级
└─ 费用: +30% (~$0.16/GB/月)

适用场景:
✓ 实时订单簿更新
✓ 高频数据写入
✓ 低延迟要求 (p99 < 1ms)
```

### 4.3 安全加固

#### 🔒 安全组配置检查

检查 `hft-collector-sg` 安全组规则:

```bash
# 查看当前规则
aliyun ecs DescribeSecurityGroupAttribute \
  --RegionId ap-northeast-1 \
  --SecurityGroupId sg-6we9gm9bevx5u9s5rmpq

建议配置:
├─ 入站规则
│  ├─ SSH (22): 仅允许办公IP或VPN
│  ├─ 应用端口: 按需开放
│  └─ ICMP: 限制或禁用
│
└─ 出站规则
   ├─ 交易所API: 仅必要的域名/IP
   ├─ NTP服务: 时间同步
   └─ 日志上传: ClickHouse/Monitoring
```

#### 🛡️ 额外安全措施

```
□ 启用 WAF (Web应用防火墙)
  - 防护API端点
  - DDoS攻击防护
  - 费用: ~$15-50/月

□ 配置 VPC 流日志
  - 记录所有网络流量
  - 异常检测
  - 费用: 按存储量计费

□ 启用操作审计
  - 追踪所有API调用
  - 安全合规
  - 费用: 免费

□ 配置密钥管理 (KMS)
  - 加密敏感配置
  - API密钥轮换
  - 费用: ~$1/密钥/月
```

### 4.4 监控与告警

#### 📊 CloudMonitor 配置

```yaml
关键指标监控:
  CPU使用率:
    - 告警阈值: >80% (持续5分钟)
    - 通知方式: 短信 + 邮件

  内存使用率:
    - 告警阈值: >85%
    - 通知方式: 邮件

  磁盘使用率:
    - 告警阈值: >80%
    - 通知方式: 邮件

  网络流量:
    - 告警阈值: 异常峰值
    - 监控带宽利用率

  HFT 特定指标:
    - 订单延迟: p99 < 25μs
    - 数据完整性: >99.9%
    - WebSocket 重连: <3次/小时
```

---

## 5. 行动计划

### 5.1 高优先级 (本周执行)

```
□ 1. 确认实例使用需求
   ├─ 评估是否需要保留已停止的实例
   ├─ 如果不需要: 创建镜像后释放
   └─ 如果需要: 计划重启时间

□ 2. 资源清理
   ├─ 删除5个未使用的VPC
   ├─ 删除11个历史安全组
   └─ 释放未使用的弹性IP (如有)

□ 3. 成本控制
   ├─ 设置预算告警 ($100/月)
   ├─ 启用资源使用报告
   └─ 配置成本优化建议
```

### 5.2 中优先级 (本月执行)

```
□ 4. 性能评估
   ├─ 测试当前配置的HFT性能
   ├─ 确定是否需要升级实例
   └─ 评估ESSD PL3的必要性

□ 5. 安全加固
   ├─ 审计安全组规则
   ├─ 配置VPC流日志
   ├─ 启用操作审计
   └─ 评估WAF需求

□ 6. 监控优化
   ├─ 配置CloudMonitor告警
   ├─ 设置HFT特定指标
   └─ 集成到Grafana (如使用)
```

### 5.3 低优先级 (季度规划)

```
□ 7. 长期成本优化
   ├─ 评估预留实例方案
   ├─ 考虑节省计划
   └─ 优化资源配置

□ 8. 架构优化
   ├─ 评估多区域部署
   ├─ 考虑容器化 (K8s)
   └─ 自动伸缩策略

□ 9. 灾难恢复
   ├─ 制定备份策略
   ├─ 设计故障转移方案
   └─ 定期演练恢复流程
```

---

## 6. 快速命令参考

### 6.1 资源管理

```bash
# 启动实例
aliyun ecs StartInstance --InstanceId i-6wec6is4knvg5h9s3iya

# 停止实例
aliyun ecs StopInstance --InstanceId i-6wec6is4knvg5h9s3iya

# 创建镜像备份
aliyun ecs CreateImage \
  --RegionId ap-northeast-1 \
  --InstanceId i-6wec6is4knvg5h9s3iya \
  --ImageName "hft-collector-backup-$(date +%Y%m%d)"

# 释放实例 (谨慎操作!)
aliyun ecs DeleteInstance \
  --InstanceId i-6wec6is4knvg5h9s3iya \
  --Force true
```

### 6.2 VPC 清理

```bash
# 删除VPC (确保无资源依赖)
aliyun vpc DeleteVpc \
  --RegionId cn-hangzhou \
  --VpcId vpc-bp1tves2x4yl0007oer72

# 删除安全组
aliyun ecs DeleteSecurityGroup \
  --RegionId cn-hangzhou \
  --SecurityGroupId sg-xxxxxxxxx
```

### 6.3 成本监控

```bash
# 查看费用账单
aliyun bssopenapi QueryBill \
  --BillingCycle 2025-01 \
  --ProductCode ecs

# 查看资源使用趋势
aliyun cms DescribeMetricList \
  --Namespace acs_ecs_dashboard \
  --MetricName CPUUtilization \
  --StartTime "2025-01-01 00:00:00" \
  --EndTime "2025-01-31 23:59:59"
```

---

## 7. 联系信息

### 7.1 阿里云支持

- **工单系统**: https://workorder.console.aliyun.com/
- **技术支持**: 95187
- **紧急故障**: 7x24小时热线

### 7.2 有用的文档链接

- [ECS定价](https://www.alibabacloud.com/product/ecs/pricing)
- [成本优化最佳实践](https://www.alibabacloud.com/help/doc-detail/52145.htm)
- [安全最佳实践](https://www.alibabacloud.com/help/doc-detail/49867.htm)
- [监控最佳实践](https://www.alibabacloud.com/help/doc-detail/163515.htm)

---

## 附录: 技术细节

### A. 实例规格对比

| 规格 | vCPU | 内存 | 网络 | 存储 | 适用场景 | 小时费用 |
|------|------|------|------|------|----------|----------|
| ecs.e-c1m1.large | 2 | 2GB | 10Gbps | ESSD | 轻量应用 | ~$0.03 |
| ecs.c7.large | 2 | 4GB | 15Gbps | ESSD | 计算优化 | ~$0.08 |
| ecs.c7a.xlarge | 4 | 8GB | 20Gbps | ESSD | 高性能计算 | ~$0.16 |
| ecs.g7.xlarge | 4 | 16GB | 20Gbps | ESSD | 内存优化 | ~$0.24 |
| ecs.c7.2xlarge | 8 | 16GB | 30Gbps | ESSD | 超低延迟 | ~$0.32 |

### B. 网络性能指标

```
当前配置 (ecs.c7.large):
├─ 网络带宽: 最高10 Mbps (公网), 15 Gbps (内网)
├─ 网络延迟: 东京到币安 ~5-10ms
├─ 连接数限制: 最高25万连接/s
└─ 建议: 适合中频交易 (100-1000 TPS)

升级后 (ecs.c7.2xlarge):
├─ 网络带宽: 最高100 Mbps (公网), 30 Gbps (内网)
├─ 网络延迟: 优化到 ~3-5ms
├─ 连接数限制: 最高100万连接/s
└─ 适用: 高频交易 (10000+ TPS)
```

### C. 存储性能对比

| 类型 | IOPS | 吞吐量 | 延迟 | 费用/GB/月 | 适用场景 |
|------|------|--------|------|------------|----------|
| ESSD PL0 | 10,000 | 180 MB/s | 1-3ms | $0.08 | 开发测试 |
| **ESSD PL1** | **50,000** | **350 MB/s** | **0.5-2ms** | **$0.12** | **当前使用** |
| ESSD PL2 | 100,000 | 750 MB/s | <1ms | $0.18 | 高I/O应用 |
| ESSD PL3 | 1,000,000 | 4,000 MB/s | <0.2ms | $0.38 | 极致性能 |

---

**报告结束**

*本报告基于 2025-10-03 时的账户状态生成。实际费用以阿里云账单为准。*
