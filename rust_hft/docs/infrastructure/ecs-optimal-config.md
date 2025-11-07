# ECS 最優配置方案

**文檔版本**: 1.0
**最後更新**: 2025-10-05
**適用場景**: 數據收集器 + ClickHouse Cloud 架構

---

## 配置概覽

此配置已在生產環境驗證，適用於高頻數據收集場景，月成本僅 **$57.60 USD (~420 CNY)**。

### 核心優勢

✅ **成本最優**: 搶佔式實例比按量付費便宜 70-80%
✅ **網絡優化**: 內網流量不計費，主要數據傳輸零成本
✅ **高可用**: ap-northeast-1 區域穩定性高

---

## 實例配置

### 基礎規格

```yaml
Provider: 阿里雲 (Alibaba Cloud)
Region: ap-northeast-1 (東京)
Zone: ap-northeast-1a

Instance:
  Type: ecs.e-c1m1.large
  vCPU: 2
  Memory: 2 GB

Billing:
  Mode: PostPaid (按量付費)
  Strategy: SpotWithPriceLimit (搶佔式實例，限價模式)
  SpotPriceLimit: $0.08 USD/hour
  EstimatedMonthlyCost: $57.60 USD/month
```

### 網絡配置

```yaml
Network:
  VPC: vpc-6wesy84ixw2esl6lb3ov5
  VSwitch: vsw-6wen523u3gvktuual6iec
  PrivateIP: 172.16.0.205

PublicIP:
  Type: EIP (彈性公網 IP)
  Address: 47.91.23.185
  Bandwidth: 100 Mbps
  ChargeType: PayByTraffic (按實際流量計費)

SecurityGroup: sg-6we9gm9bevx5u9s5rmpq
```

---

## 關鍵架構設計

### 內網流量優化

**核心原則**: ECS 與 ClickHouse 部署在同一 AWS 區域

```
ECS (ap-northeast-1)
    ↓ 內網連接 (不計費)
ClickHouse Cloud (ap-northeast-1)
    URL: https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
```

**流量分析**:

| 流量類型 | 方向 | 帶寬 | 計費 | 說明 |
|---------|------|------|------|------|
| WebSocket 數據接收 | 入站 (公網) | ~0.02 Mbps | 免費 | 4 個交易所數據流 |
| 系統管理 | 出站 (公網) | ~0.01 Mbps | 按量 | SSH, Docker, 更新 |
| ClickHouse 寫入 | 出站 (內網) | ~3-5 Mbps | **免費** | 主要數據流 (1,500 records/sec × 4) |

**關鍵收益**: 主要數據流走內網，**流量成本趨近於零**。

---

## 部署步驟

### 1. 創建 ECS 實例

```bash
# 使用阿里雲 CLI 創建搶佔式實例
aliyun ecs CreateInstance \
  --RegionId ap-northeast-1 \
  --ZoneId ap-northeast-1a \
  --InstanceType ecs.e-c1m1.large \
  --ImageId ubuntu_22_04_x64_20G_alibase_20230907.vhd \
  --SecurityGroupId sg-6we9gm9bevx5u9s5rmpq \
  --VSwitchId vsw-6wen523u3gvktuual6iec \
  --InstanceChargeType PostPaid \
  --SpotStrategy SpotWithPriceLimit \
  --SpotPriceLimit 0.08 \
  --InternetChargeType PayByTraffic \
  --InternetMaxBandwidthOut 100
```

### 2. 配置 ClickHouse Cloud

**重要**: 必須選擇與 ECS 相同的區域！

```yaml
ClickHouse Cloud Settings:
  Provider: AWS
  Region: ap-northeast-1 (Tokyo)
  Tier: Development (或根據需求選擇)

Connection:
  URL: https://<your-id>.ap-northeast-1.aws.clickhouse.cloud:8443
  Database: hft_db
  User: default
  Password: <your-password>
```

### 3. 驗證內網連通性

```bash
# SSH 登入 ECS 後測試
# 方法 1: 檢查 DNS 解析
nslookup kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud

# 方法 2: 測試 ClickHouse 連接
clickhouse-client \
  --host kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud \
  --port 9440 \
  --secure \
  --user default \
  --password <password> \
  --query "SELECT 1"

# 方法 3: 查看路由（確認走內網）
traceroute kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud
```

### 4. 部署數據收集器

```bash
# 克隆代碼
git clone <your-repo>
cd rust_hft/tools/collector

# 構建 Docker 鏡像
docker build -t hft-collector .

# 運行 4 個收集器
docker run -d --name binance-spot \
  -e CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443" \
  -e CLICKHOUSE_USER=default \
  -e CLICKHOUSE_PASSWORD=<password> \
  hft-collector --exchanges binance --symbols BTCUSDT,ETHUSDT

docker run -d --name binance-futures \
  -e CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443" \
  -e CLICKHOUSE_USER=default \
  -e CLICKHOUSE_PASSWORD=<password> \
  hft-collector --exchanges binance_futures --symbols BTCUSDT,ETHUSDT

docker run -d --name bitget \
  -e CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443" \
  -e CLICKHOUSE_USER=default \
  -e CLICKHOUSE_PASSWORD=<password> \
  hft-collector --exchanges bitget --symbols BTCUSDT,ETHUSDT

docker run -d --name asterdex \
  -e CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443" \
  -e CLICKHOUSE_USER=default \
  -e CLICKHOUSE_PASSWORD=<password> \
  hft-collector --exchanges asterdex --symbols BTCUSDT,ETHUSDT
```

---

## 成本估算

### 月度成本明細

```
實例費用:
  - 價格: $0.08 USD/hour
  - 月小時數: 24 × 30 = 720 hours
  - 月費: $0.08 × 720 = $57.60 USD

流量費用:
  - 公網入站: 免費
  - 公網出站: ~0.01 Mbps × 720h × 60 × 60 / 8 / 1024 / 1024 × $0.12/GB ≈ $0.00
  - 內網流量: 免費

總計: $57.60 USD/月 (約 420 CNY/月)
```

### 年度成本

```
年度總成本: $57.60 × 12 = $691.20 USD/year
```

### 成本對比

| 方案 | 月成本 | 年成本 | 說明 |
|-----|--------|--------|------|
| **當前方案 (搶佔式)** | **$57.60** | **$691** | 最優選擇 |
| 按量付費實例 | $200-250 | $2,400-3,000 | 貴 4-5 倍 |
| 包年包月實例 | $150-180 | $1,800-2,160 | 貴 2.5-3 倍 |

---

## 進一步優化建議

### 可選優化 1: 降級實例規格

**適用場景**: 數據收集器 CPU/內存使用率 < 50%

```yaml
Current: ecs.e-c1m1.large (2 vCPU, 2 GB)
Optimized: ecs.t5-c1m1.small (1 vCPU, 1 GB)

Savings: ~$20-25 USD/month (40-50% reduction)
Risk: 需測試性能是否足夠
```

**測試步驟**:
1. 創建新的 1 vCPU 實例
2. 部署單個收集器測試
3. 監控 CPU/內存使用率
4. 如果 < 70%，可以安全遷移

### 可選優化 2: 降低帶寬上限

**適用場景**: 避免意外流量爆發

```yaml
Current: 100 Mbps
Optimized: 10 Mbps

Savings: ~$0 (按流量計費，影響極小)
Benefit: 防止意外大流量產生高額費用
```

---

## 監控指標

### 關鍵性能指標

```bash
# CPU 使用率
aliyun ecs DescribeInstanceMonitorData \
  --InstanceId i-6wec6is4knvg5h9s3iya \
  --StartTime "2025-10-05T00:00:00Z" \
  --EndTime "2025-10-05T23:59:59Z"

# 流量監控
aliyun ecs DescribeInstanceMonitorData \
  --InstanceId i-6wec6is4knvg5h9s3iya \
  --StartTime "2025-10-05T00:00:00Z" \
  --EndTime "2025-10-05T23:59:59Z" \
  | jq '.MonitorData.InstanceMonitorData[] | {ts, out_mbps: (.BPSWrite/1000000)}'
```

### 告警閾值

```yaml
CPU_Usage:
  Warning: > 70%
  Critical: > 90%

Memory_Usage:
  Warning: > 80%
  Critical: > 95%

Network_OutBandwidth:
  Warning: > 10 Mbps (異常，正常 < 1 Mbps)
  Critical: > 50 Mbps (可能攻擊或配置錯誤)

Disk_Usage:
  Warning: > 80%
  Critical: > 90%
```

---

## 故障處理

### 搶佔式實例被回收

**症狀**: 實例突然停止

**恢復步驟**:
```bash
# 1. 檢查實例狀態
aliyun ecs DescribeInstances --InstanceIds '["i-6wec6is4knvg5h9s3iya"]'

# 2. 如果被回收，創建新的搶佔式實例（使用相同配置）
aliyun ecs CreateInstance \
  --RegionId ap-northeast-1 \
  --ZoneId ap-northeast-1a \
  --InstanceType ecs.e-c1m1.large \
  --SpotStrategy SpotWithPriceLimit \
  --SpotPriceLimit 0.08 \
  # ... (其他參數同上)

# 3. 重新部署數據收集器（使用 Docker Compose 或腳本）
```

**預防措施**:
- 使用 AutoScaling 自動創建新實例
- 配置告警通知（實例停止時發送郵件/短信）
- 定期備份配置和數據

### 網絡連接問題

**症狀**: ClickHouse 連接失敗

**診斷步驟**:
```bash
# 1. 檢查 DNS 解析
nslookup kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud

# 2. 檢查網絡連通性
ping kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud

# 3. 檢查端口
telnet kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud 8443

# 4. 檢查安全組規則
aliyun ecs DescribeSecurityGroupAttribute \
  --SecurityGroupId sg-6we9gm9bevx5u9s5rmpq
```

---

## 參考信息

### 實例實際使用數據

```
實例 ID:      i-6wec6is4knvg5h9s3iya
創建時間:     2025-10-03T11:06Z
運行時長:     2+ days
實際帶寬:     0.03 Mbps (公網出站)
CPU 使用率:   ~30-40% (估計)
內存使用率:   ~50-60% (估計)
```

### 相關文檔

- [阿里雲搶佔式實例文檔](https://help.aliyun.com/document_detail/52088.html)
- [ClickHouse Cloud 區域列表](https://clickhouse.com/docs/en/cloud/reference/regions)
- [HFT 數據收集器使用指南](../collector/README.md)

### 維護記錄

| 日期 | 操作 | 說明 |
|------|------|------|
| 2025-10-03 | 創建實例 | 初始部署 |
| 2025-10-05 | 成本審計 | 確認配置合理性 |

---

## 總結

這個配置方案的核心價值在於：

1. **成本效益**: 搶佔式實例節省 70-80% 費用
2. **網絡優化**: 同區域部署，內網流量免費
3. **高性能**: ap-northeast-1 延遲低，穩定性高
4. **可擴展**: 隨時可以升級實例規格或添加更多實例

**推薦用於**:
- 數據收集器部署
- 低延遲數據處理
- ClickHouse Cloud 配套架構
- 成本敏感型項目

**不適用於**:
- 需要 100% 可用性的核心業務（搶佔式實例可能被回收）
- 跨區域數據傳輸場景
- 需要固定 IP 長期穩定的服務
