# 🚀 HFT 系統部署檢查清單

**日期**: 2025-07-25  
**版本**: 4.0  
**執行人**: _____________  
**審核人**: _____________

---

## ✅ 部署前檢查

### 🔧 環境準備
- [ ] **Docker 和 Docker Compose 已安裝**
  ```bash
  docker --version  # >= 20.10
  docker-compose --version  # >= 1.29
  ```
- [ ] **Rust 工具鏈已安裝**
  ```bash
  rustc --version  # >= 1.70
  cargo --version
  ```
- [ ] **Python 環境已配置**
  ```bash
  python3 --version  # >= 3.9
  pip3 list | grep -E "(agno|redis|asyncio)"
  ```
- [ ] **系統資源檢查**
  - [ ] 可用內存 >= 8GB
  - [ ] 可用磁盤空間 >= 50GB
  - [ ] CPU 核心數 >= 4

### 📁 代碼庫準備
- [ ] **最新代碼已拉取**
  ```bash
  git status  # clean working directory
  git log -1  # 確認最新 commit
  ```
- [ ] **依賴已安裝**
  ```bash
  cd rust_hft && cargo build --release
  cd ops_workspace && pip3 install -r requirements.txt
  cd ml_workspace && pip3 install -r requirements.txt
  ```
- [ ] **配置文件已準備**
  - [ ] `rust_hft/config/` 配置正確
  - [ ] `docker-compose.yml` 配置檢查
  - [ ] Redis 和 ClickHouse 配置驗證

---

## 🚀 部署執行步驟

### Phase 1: 基礎設施啟動 (預計 15 分鐘)

#### Step 1.1: 啟動支持服務
```bash
cd /Users/proerror/Documents/monday/rust_hft
docker-compose up -d redis clickhouse prometheus grafana
```
- [ ] **Redis 啟動成功** 
  ```bash
  docker logs rust_hft_redis_1 | tail -5
  redis-cli ping  # 應返回 PONG
  ```
- [ ] **ClickHouse 啟動成功**
  ```bash
  docker logs rust_hft_clickhouse_1 | tail -5
  curl http://localhost:8123  # 應返回 "Ok."
  ```
- [ ] **Prometheus 啟動成功**
  ```bash
  curl http://localhost:9090/-/healthy  # 應返回 200
  ```
- [ ] **Grafana 啟動成功**
  ```bash
  curl http://localhost:3000/api/health  # 應返回健康狀態
  ```

#### Step 1.2: 驗證服務連通性
```bash
# 測試 Redis 連接
python3 -c "
import redis
r = redis.Redis(host='localhost', port=6379)
print('Redis ping:', r.ping())
"
```
- [ ] **Redis 連通性測試通過**
- [ ] **ClickHouse 連通性測試通過**
- [ ] **所有端口監聽正常**
  ```bash
  netstat -tlnp | grep -E "(6379|8123|9090|3000)"
  ```

### Phase 2: 核心系統啟動 (預計 20 分鐘)

#### Step 2.1: 編譯 Rust HFT 核心
```bash
cd rust_hft
cargo build --release --bin hft-core
```
- [ ] **編譯成功，無錯誤**
- [ ] **二進制文件生成**: `target/release/hft-core`
- [ ] **依賴檢查通過**

#### Step 2.2: 啟動 HFT 核心引擎
```bash
# 在背景啟動核心引擎
cargo run --release --bin hft-core > logs/hft-core.log 2>&1 &
HFT_CORE_PID=$!
echo "HFT Core PID: $HFT_CORE_PID"
```
- [ ] **HFT 核心啟動成功**
- [ ] **進程 ID 記錄**: ___________
- [ ] **日誌無 ERROR 級別錯誤**
  ```bash
  grep -i error logs/hft-core.log | wc -l  # 應為 0
  ```

#### Step 2.3: 驗證核心功能
```bash
# 檢查 gRPC 服務
grpcurl -plaintext localhost:50051 list
# 檢查指標端點
curl http://localhost:8080/metrics | head -10
```
- [ ] **gRPC 服務響應正常**
- [ ] **Prometheus 指標暴露正常**
- [ ] **健康檢查端點正常**

### Phase 3: 監控系統啟動 (預計 15 分鐘)

#### Step 3.1: 啟動 Ops Workspace
```bash
cd ../ops_workspace
python3 agents/real_latency_guard.py > logs/ops-agent.log 2>&1 &
OPS_AGENT_PID=$!
echo "Ops Agent PID: $OPS_AGENT_PID"
```
- [ ] **Ops Agent 啟動成功**
- [ ] **進程 ID 記錄**: ___________
- [ ] **Redis 連接建立成功**

#### Step 3.2: 配置 ML Workspace
```bash
cd ../ml_workspace
# 測試 ML 組件
python3 test_agents.py
```
- [ ] **ML Agents 測試通過**
- [ ] **依賴模組正常加載**
- [ ] **配置文件讀取成功**

### Phase 4: 集成測試 (預計 20 分鐘)

#### Step 4.1: 執行煙霧測試
```bash
cd ..
python3 test_cross_workspace_integration.py
```
- [ ] **跨 Workspace 通信測試**: ✅ 通過
- [ ] **ML → Ops 事件流**: ✅ 通過  
- [ ] **Ops → ML 反饋流**: ✅ 通過
- [ ] **緊急協調測試**: ✅ 通過

#### Step 4.2: 性能基線測試
```bash
python3 test_quick_performance.py
```
- [ ] **延遲測試**: _____ ms (目標 < 1ms)
- [ ] **吞吐量測試**: _____ msg/s (目標 > 1000)
- [ ] **並發測試**: _____ % 成功率 (目標 > 95%)
- [ ] **錯誤恢復**: _____ % 恢復率 (目標 > 90%)

---

## 📊 部署後驗證

### 🔍 系統健康檢查
- [ ] **所有進程運行正常**
  ```bash
  ps aux | grep -E "(hft-core|real_latency_guard)"
  ```
- [ ] **內存使用率正常** (< 80%)
  ```bash
  free -h
  ```
- [ ] **CPU 使用率正常** (< 70%)
  ```bash
  top -bn1 | grep "Cpu(s)"
  ```

### 📈 監控配置驗證
- [ ] **Grafana 面板可訪問**: http://localhost:3000
  - 用戶名: admin
  - 密碼: admin (首次登入需修改)
- [ ] **Prometheus 指標收集**: http://localhost:9090
- [ ] **關鍵指標顯示正常**:
  - `hft_exec_latency_ms`
  - `redis_connection_count` 
  - `system_memory_usage`
  - `ops_agent_alerts_total`

### 🔔 告警測試
```bash
# 發送測試告警
python3 -c "
import asyncio
import json
from ops_workspace.connectors.rust_hft_connector import RustHFTConnector

async def test_alert():
    connector = RustHFTConnector()
    await connector.connect()
    await connector.redis_client.publish('ops.alert', json.dumps({
        'type': 'latency',
        'value': 30.0,
        'component': 'test',
        'timestamp': '2025-07-25T15:00:00Z'
    }))
    await connector.disconnect()

asyncio.run(test_alert())
"
```
- [ ] **測試告警發送成功**
- [ ] **Ops Agent 收到並處理告警**
- [ ] **Grafana 面板顯示告警事件**

---

## 🎯 生產就緒檢查

### 🔒 安全檢查
- [ ] **防火牆規則配置**
  ```bash
  # 只允許必要端口
  sudo ufw status | grep -E "(6379|8123|9090|3000|50051)"
  ```
- [ ] **Redis AUTH 啟用** (生產環境必需)
- [ ] **敏感配置文件權限**
  ```bash
  find . -name "*.yml" -o -name "*.toml" | xargs ls -la
  ```

### 📋 操作文檔準備
- [ ] **運維手冊已準備**: `/docs/operations.md`
- [ ] **故障排除指南**: `/docs/troubleshooting.md`  
- [ ] **監控指標說明**: `/docs/metrics.md`
- [ ] **緊急聯絡清單**: `/docs/contacts.md`

### 🔄 備份和恢復
- [ ] **配置文件備份**
  ```bash
  tar -czf config-backup-$(date +%Y%m%d).tar.gz */config/
  ```
- [ ] **數據庫備份策略確認**
- [ ] **系統快照創建** (如使用雲服務)

---

## 📝 部署記錄

### 部署信息
- **開始時間**: ___________
- **完成時間**: ___________
- **總耗時**: ___________
- **部署版本**: v4.0
- **Git Commit**: ___________

### 關鍵組件版本
- **Rust**: ___________
- **Python**: ___________
- **Redis**: ___________
- **ClickHouse**: ___________
- **Docker**: ___________

### 初始性能基線
- **平均延遲**: _____ ms
- **P99 延遲**: _____ ms  
- **吞吐量**: _____ msg/s
- **內存使用**: _____ %
- **CPU 使用**: _____ %

### 問題記錄
**遇到的問題**:
1. ________________________________
2. ________________________________  
3. ________________________________

**解決方案**:
1. ________________________________
2. ________________________________
3. ________________________________

---

## 👥 簽名確認

**部署執行人**: _________________ 日期: _______  
**技術審核**: _________________ 日期: _______  
**業務確認**: _________________ 日期: _______  

---

## 🚨 緊急聯絡

**24x7 技術支持**: [電話/Slack]  
**系統架構師**: [聯絡方式]  
**業務負責人**: [聯絡方式]

---

## 📚 相關文檔

- [系統就緒報告](./SYSTEM_READINESS_REPORT.md)
- [行動計劃](./ACTION_PLAN.md)  
- [架構設計](./CLAUDE.md)
- [詳細路線圖](./detailed_roadmap.json)

---

**🎉 部署完成後，系統即可投入生產使用！**

記住：
- 持續監控關鍵指標
- 每日檢查系統健康狀態
- 定期執行備份
- 保持文檔更新