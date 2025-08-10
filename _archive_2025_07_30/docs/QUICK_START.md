# 🚀 HFT 系統快速啟動指南

**最簡單的啟動方式** - 適用於日常使用

---

## ⚡ 一鍵啟動 (推薦)

```bash
cd /Users/proerror/Documents/monday
./start_hft_system.sh
```

**預期結果**: 10-15分鐘後看到系統啟動完成信息

---

## 🎯 正確的啟動順序

如果您想手動控制每個步驟：

### 1. 啟動基礎設施
```bash
cd rust_hft
docker-compose up -d redis clickhouse
```

### 2. 啟動 Rust HFT 核心
```bash
cargo run --release --bin main
```

### 3. 啟動 Ops Workspace (使用 Agno)
```bash
cd ../ops_workspace
python3 agents/real_latency_guard.py
```

### 4. (可選) 配置 ML Workspace  
```bash
cd ../ml_workspace
ag ws schedule workflows/training_workflow.py --cron "0 2 * * *"
```

---

## 🔍 驗證系統運行

```bash
# 檢查所有服務狀態
ps aux | grep -E "(hft-core|ag ws)" | grep -v grep

# 測試系統集成
python3 test_cross_workspace_integration.py

# 檢查性能
python3 test_quick_performance.py
```

---

## 🛑 停止系統

```bash
# 優雅停止
./stop_hft_system.sh

# 強制停止 (如有問題)
./stop_hft_system.sh --force
```

---

## 📊 成功啟動的標誌

當您看到以下輸出時，系統就成功啟動了：

```
🎉 HFT 系統啟動完成！

📊 服務狀態:
  ✅ Redis: 運行中 (端口 6379)
  ✅ ClickHouse: 運行中 (端口 8123)  
  ✅ HFT 核心: 運行中 (PID: XXXX)
  ✅ Ops Workspace: 運行中 (PID: XXXX)

🎯 系統已準備好處理交易！
```

---

## 🔧 常見問題

### 問題: "ag: command not found"
```bash
# 安裝 Agno
pip3 install agno
```

### 問題: "Docker 服務無法啟動"
```bash
# 檢查 Docker 是否運行
docker ps
# 如果沒有，啟動 Docker Desktop
```

### 問題: "Redis 連接失敗"
```bash
# 重啟 Redis
docker-compose down
docker-compose up -d redis
```

### 問題: "Rust 編譯失敗"
```bash
# 清理並重新編譯
cd rust_hft
cargo clean
cargo build --release
```

---

## 📈 監控和管理

### 查看實時日誌
```bash
# HFT 核心日誌
tail -f logs/hft-core.log

# Ops Workspace 日誌  
tail -f logs/ops-workspace.log

# 系統整體狀態
watch -n 2 'ps aux | grep -E "(hft-core|ag ws)" | grep -v grep'
```

### 性能監控
```bash
# 快速性能檢查
python3 test_quick_performance.py

# 系統資源使用
htop  # 或 top
```

---

## 🎯 下一步

系統啟動成功後，您可以：

1. **配置交易策略**: 修改 `rust_hft/config/` 中的參數
2. **設置監控告警**: 訪問 http://localhost:3000 (Grafana)
3. **查看系統指標**: 訪問 http://localhost:9090 (Prometheus)
4. **運行模型訓練**: 在 `ml_workspace` 中配置訓練任務

---

**🚀 現在開始使用您的 HFT 系統進行高頻交易吧！**