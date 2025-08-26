# HFT System SLO 運維手冊

**版本**: 1.0  
**適用階段**: Phase 1 LOB-Live 硬化  
**更新日期**: 2025-08-25  

---

## 1. SLO 定義與閾值

### 1.1 核心執行 SLO

| 指標 | 目標值 | 告警閾值 | 熔斷閾值 | 測量窗口 |
|------|--------|----------|----------|----------|
| **下單成功率** | ≥99.5% | <99.5% | <99% | 1分鐘 |
| **撤單成功率** | ≥99% | <99% | <95% | 1分鐘 |
| **拒單率** | ≤1% | >1% | >5% | 1分鐘 |
| **端到端延遲 p99** | ≤100μs | >100μs | >200μs | 1分鐘 |
| **下單確認延遲 p99** | ≤50μs | >50μs | >100μs | 1分鐘 |

### 1.2 風控 SLO

| 指標 | 目標值 | 告警閾值 | 熔斷閾值 | 測量窗口 |
|------|--------|----------|----------|----------|
| **風控檢查延遲 p95** | ≤20μs | >50μs | >100μs | 1分鐘 |
| **日內最大虧損** | ≤$2000 | >$2000 | >$3000 | 實時 |
| **最大回撤比例** | ≤2% | >2% | >3% | 實時 |
| **連續虧損次數** | ≤2 | ≥3 | ≥4 | 實時 |

### 1.3 系統健康 SLO

| 指標 | 目標值 | 告警閾值 | 危險閾值 | 測量窗口 |
|------|--------|----------|----------|----------|
| **隊列利用率** | ≤60% | >80% | >95% | 1分鐘 |
| **事件丟棄率** | ≤0.01% | >0.1% | >1% | 1分鐘 |
| **數據陳舊度 p95** | ≤3ms | >5ms | >10ms | 1分鐘 |
| **WebSocket 重連頻率** | ≤1次/小時 | >3次/小時 | >10次/小時 | 10分鐘 |

---

## 2. 告警處理流程

### 2.1 🔴 Critical 級別告警

#### CircuitBreakerTriggered - 熔斷器激活

**影響**: 🚨 所有交易暫停，系統進入保護模式

**立即行動** (5分鐘內):
1. **確認熔斷原因**:
   ```bash
   # 檢查熔斷觸發原因
   curl -s http://localhost:9090/api/v1/query?query=hft_circuit_breaker_active | jq
   
   # 檢查最近的告警
   curl -s http://localhost:9093/api/v1/alerts?active=true | jq '.data[] | select(.labels.component=="risk_manager")'
   ```

2. **評估系統狀態**:
   ```bash
   # 檢查 PnL
   curl -s http://localhost:9090/api/v1/query?query=hft_daily_pnl_usd
   
   # 檢查連續虧損
   curl -s http://localhost:9090/api/v1/query?query=hft_consecutive_losses
   
   # 檢查回撤
   curl -s http://localhost:9090/api/v1/query?query=hft_drawdown_percentage
   ```

3. **決策樹**:
   - **虧損觸發**: 檢查策略邏輯，評估市場條件
   - **延遲觸發**: 檢查網絡連接，系統資源使用
   - **頻繁觸發**: 檢查風控配置是否過於嚴格

4. **手動恢復** (僅在確認安全後):
   ```bash
   # 重置熔斷器 (需要管理員權限)
   curl -X POST http://localhost:8080/api/v1/risk/circuit-breaker/reset
   ```

#### OrderSuccessRateLow - 下單成功率低

**影響**: 策略無法正常執行，可能錯失交易機會

**診斷步驟** (2分鐘內):
1. **檢查交易所狀態**:
   ```bash
   # 檢查 Bitget API 狀態
   curl -s https://api.bitget.com/api/spot/v1/public/time
   
   # 檢查私有 WebSocket 連接
   grep "WebSocket" /var/log/hft/execution.log | tail -10
   ```

2. **檢查訂單拒絕原因**:
   ```bash
   # 檢查最近的拒單日志
   grep "OrderReject" /var/log/hft/execution.log | tail -20
   
   # 檢查風控拒絕統計
   curl -s http://localhost:9090/api/v1/query?query=rate(hft_orders_rejected_total[5m])
   ```

3. **常見解決方案**:
   - **資金不足**: 檢查賬戶余額
   - **API 限流**: 降低下單頻率
   - **風控攔截**: 調整風控參數
   - **網絡問題**: 重啟連接或切換網絡

#### EndToEndLatencyHigh - 端到端延遲過高

**影響**: 交易執行滯後，可能影響策略效果

**診斷步驟**:
1. **延遲分解分析**:
   ```bash
   # 檢查各階段延遲
   curl -s 'http://localhost:9090/api/v1/query?query=histogram_quantile(0.99,rate(hft_latency_ingestion_microseconds_bucket[1m]))'
   curl -s 'http://localhost:9090/api/v1/query?query=histogram_quantile(0.99,rate(hft_latency_risk_microseconds_bucket[1m]))'
   curl -s 'http://localhost:9090/api/v1/query?query=histogram_quantile(0.99,rate(hft_latency_execution_microseconds_bucket[1m]))'
   ```

2. **系統資源檢查**:
   ```bash
   # CPU 使用率
   top -p $(pgrep hft-live)
   
   # 內存使用
   cat /proc/$(pgrep hft-live)/status | grep -E "VmRSS|VmSize"
   
   # 網絡延遲
   ping -c 10 api.bitget.com
   ```

3. **優化措施**:
   - **高 CPU**: 重啟進程，檢查是否有死循環
   - **高內存**: 檢查內存洩漏，重啟服務
   - **網絡延遲**: 切換網絡路由或 CDN
   - **風控延遲**: 簡化風控規則或異步處理

### 2.2 ⚠️ Warning 級別告警

#### QueueUtilizationHigh - 隊列利用率高

**影響**: 系統接近過載，可能開始丟棄事件

**處理措施**:
1. **監控趨勢**:
   ```bash
   # 檢查隊列利用率趨勢
   curl -s 'http://localhost:9090/api/v1/query_range?query=hft_queue_utilization_ratio&start='$(date -d '10 minutes ago' +%s)'&end='$(date +%s)'&step=30s'
   ```

2. **暫時措施**:
   - 降低數據攝取頻率
   - 暫停非關鍵策略
   - 增加隊列大小 (重啟後生效)

3. **長期解決**:
   - 優化數據處理邏輯
   - 考慮增加處理線程
   - 升級硬件配置

#### DataStalenessHigh - 數據陳舊度高

**影響**: 決策基於過期數據，可能影響策略準確性

**處理措施**:
1. **檢查數據源**:
   ```bash
   # 檢查 WebSocket 連接狀態
   grep "heartbeat\|ping\|pong" /var/log/hft/data.log | tail -10
   
   # 檢查網絡延遲
   ping -c 5 ws.bitget.com
   ```

2. **重啟數據連接**:
   ```bash
   # 重啟市場數據服務
   systemctl restart hft-data-service
   ```

---

## 3. 監控看板使用指南

### 3.1 核心 SLO 儀表板

訪問地址: `http://grafana.internal/d/hft-slo`

**關鍵面板**:
- **🎯 核心 SLO 總覽**: 一眼看到所有關鍵指標狀態
- **📊 執行性能 SLO**: 下單成功率與拒單率趨勢
- **⚡ 延遲分解**: 各階段延遲分解，定位性能瓶頸
- **🛡️ 風控熔斷監控**: 熔斷器狀態與風控指標
- **💰 PnL & 風控指標**: 實時盈虧與風控狀態

### 3.2 告警查看

Alertmanager 地址: `http://alertmanager.internal`

**告警分類**:
- **🔴 Critical**: 需要立即處理，可能影響系統穩定性
- **⚠️ Warning**: 需要關注，但不會立即影響交易
- **ℹ️ Info**: 信息性告警，如系統恢復通知

---

## 4. 常見問題排查

### 4.1 性能問題

**症狀**: 延遲突然增加，吞吐量下降

**排查步驟**:
1. **系統資源**: `htop`, `iotop`, `nethogs`
2. **進程狀態**: `strace -p <pid>`, `lsof -p <pid>`
3. **網絡狀態**: `ss -tuln`, `netstat -i`
4. **日志檢查**: `journalctl -u hft-live -f`

### 4.2 連接問題

**症狀**: WebSocket 頻繁重連，數據中斷

**排查步驟**:
1. **DNS 解析**: `nslookup ws.bitget.com`
2. **網絡連通性**: `telnet ws.bitget.com 443`
3. **證書驗證**: `openssl s_client -connect ws.bitget.com:443`
4. **防火牆規則**: `iptables -L`, `ufw status`

### 4.3 風控異常

**症狀**: 大量訂單被風控拒絕

**排查步驟**:
1. **風控配置**: 檢查 `risk_config.yaml`
2. **賬戶狀態**: 檢查持倉與余額
3. **市場條件**: 檢查是否有異常波動
4. **風控日志**: `grep "risk" /var/log/hft/engine.log`

---

## 5. 應急處理預案

### 5.1 系統完全停止交易

**觸發條件**:
- 熔斷器激活且無法自動恢復
- 延遲超過 1ms 持續 5 分鐘
- 數據中斷超過 30 秒

**處理步驟**:
1. **立即**: 通知交易團隊，停止策略
2. **2分鐘內**: 評估問題嚴重性，決定是否人工干預
3. **5分鐘內**: 實施緊急修復或切換到備用系統
4. **事後**: 進行根本原因分析，更新應急預案

### 5.2 部分功能降級

**觸發條件**:
- 單一交易所連接中斷
- 策略延遲超過閾值但仍可接受
- 部分風控功能異常

**處理步驟**:
1. **隔離問題**: 禁用有問題的模塊
2. **降級運行**: 使用簡化策略繼續交易
3. **並行修復**: 在不影響交易的情況下修復問題
4. **逐步恢復**: 問題解決後逐步恢復完整功能

---

## 6. 聯繫方式

### 6.1 告警通知渠道

- **Slack**: `#hft-alerts`
- **PagerDuty**: Critical 告警自動呼叫
- **Email**: `hft-team@company.com`
- **短信**: 熔斷器觸發時發送

### 6.2 升級路徑

1. **L1 - 運維工程師**: 基礎問題排查，系統重啟
2. **L2 - 系統工程師**: 深度問題分析，配置調優
3. **L3 - 架構師**: 設計問題，重大系統變更
4. **L4 - 業務專家**: 交易邏輯，風控策略調整

---

## 7. 定期維護檢查清單

### 7.1 每日檢查 (自動化)

- [ ] SLO 達成率統計
- [ ] 告警數量與趨勢分析  
- [ ] 系統資源使用率
- [ ] 交易量與 PnL 日報

### 7.2 每週檢查 (手工)

- [ ] 風控參數合理性評估
- [ ] 性能趨勢分析
- [ ] 告警規則優化
- [ ] 系統配置備份

### 7.3 每月檢查 (團隊)

- [ ] SLO 目標達成情況回顧
- [ ] 重大事件復盤
- [ ] 運維流程優化
- [ ] 應急預案更新

---

**文檔維護**: 該文檔應隨系統演進持續更新，每次重大變更後需要更新相應的運維流程。