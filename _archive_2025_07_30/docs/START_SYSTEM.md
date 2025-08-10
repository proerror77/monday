# 🚀 HFT 系統啟動指南

**執行時間**: 約 10-15 分鐘  
**前提條件**: 所有測試已通過 ✅

---

## 📋 啟動前檢查

```bash
# 1. 確認當前位置
cd /Users/proerror/Documents/monday
pwd

# 2. 檢查 Docker 服務
docker --version
docker-compose --version

# 3. 檢查系統資源
free -h  # 確保有足夠內存
df -h    # 確保有足夠磁盤空間
```

---

## 🔥 系統啟動步驟 (按順序執行)

### Step 1: 啟動基礎設施 (2-3 分鐘)

```bash
# 進入 rust_hft 目錄
cd rust_hft

# 啟動 Redis 和 ClickHouse
docker-compose up -d redis clickhouse

# 等待服務啟動並驗證
sleep 10
redis-cli ping  # 應返回 PONG
curl http://localhost:8123  # 應返回 "Ok."

echo "✅ 基礎設施啟動完成"
```

### Step 2: 啟動 Rust HFT 核心 (3-5 分鐘)

```bash
# 確保在 rust_hft 目錄
cd /Users/proerror/Documents/monday/rust_hft

# 編譯並啟動核心引擎
echo "🔨 編譯 Rust HFT 核心..."
cargo build --release

# 啟動核心引擎 (在背景執行)
echo "🚀 啟動 HFT 核心引擎..."
cargo run --release --bin main > ../logs/hft-core.log 2>&1 &
HFT_PID=$!

echo "HFT 核心引擎 PID: $HFT_PID"
echo $HFT_PID > ../logs/hft-core.pid

# 等待啟動
sleep 5
echo "✅ Rust HFT 核心啟動完成"
```

### Step 3: 啟動 Ops Workspace 監控 (1-2 分鐘)

```bash
# 進入 ops_workspace 目錄
cd /Users/proerror/Documents/monday/ops_workspace

# 啟動 Ops Workspace 監控代理
echo "🛡️ 啟動 Ops Workspace 監控代理..."
python3 agents/real_latency_guard.py > ../logs/ops-workspace.log 2>&1 &
OPS_PID=$!

echo "Ops Workspace PID: $OPS_PID" 
echo $OPS_PID > ../logs/ops-workspace.pid

echo "✅ Ops Workspace 監控啟動完成"
```

### Step 4: 驗證系統運行 (2-3 分鐘)

```bash
# 回到根目錄
cd /Users/proerror/Documents/monday

# 創建日誌目錄
mkdir -p logs

# 執行系統健康檢查
echo "🔍 執行系統健康檢查..."
python3 test_cross_workspace_integration.py

echo "✅ 系統啟動驗證完成"
```

---

## 📊 啟動驗證檢查

執行以下命令確認系統正常運行：

```bash
# 檢查所有進程
ps aux | grep -E "(hft-core|real_latency_guard|redis|clickhouse)" | grep -v grep

# 檢查端口監聽
netstat -tlnp | grep -E "(6379|8123|50051)"

# 檢查 Redis 連接
redis-cli ping

# 檢查日誌（應該沒有 ERROR）
tail -20 logs/hft-core.log
tail -20 logs/ops-agent.log
```

---

## 🎯 預期輸出結果

### 成功啟動的標誌：

1. **Redis**: 
   ```
   redis-cli ping
   PONG
   ```

2. **ClickHouse**:
   ```
   curl http://localhost:8123
   Ok.
   ```

3. **HFT 核心**: 日誌顯示 `"HFT Core initialized successfully"`

4. **Ops 監控**: 日誌顯示 `"✅ 延遲監控已啟動"`

5. **集成測試**: 顯示 `"🎉 所有跨 Workspace 集成測試通過！"`

---

## 🚨 常見問題排除

### 問題 1: Redis 連接失敗
```bash
# 解決方案
docker-compose down
docker-compose up -d redis
sleep 5
redis-cli ping
```

### 問題 2: Rust 編譯失敗
```bash
# 解決方案
cd rust_hft
cargo clean
cargo build --release
```

### 問題 3: Python 模組找不到
```bash
# 解決方案
cd ops_workspace
pip3 install -r requirements.txt

cd ../ml_workspace  
pip3 install -r requirements.txt
```

### 問題 4: 端口被佔用
```bash
# 檢查端口佔用
lsof -i :6379  # Redis
lsof -i :8123  # ClickHouse
lsof -i :50051 # gRPC

# 終止佔用進程
kill -9 <PID>
```

---

## 📈 啟動後的監控

### 實時監控命令：

```bash
# 監控系統狀態
watch -n 2 "
echo '=== 進程狀態 ==='
ps aux | grep -E '(hft-core|real_latency_guard)' | grep -v grep
echo
echo '=== 內存使用 ==='
free -h
echo  
echo '=== Redis 狀態 ==='
redis-cli ping
"
```

### 關鍵指標檢查：

```bash
# 檢查延遲性能
python3 -c "
import asyncio
import time
from ops_workspace.connectors.rust_hft_connector import RustHFTConnector

async def check_latency():
    connector = RustHFTConnector()
    await connector.connect()
    
    latencies = []
    for i in range(10):
        start = time.perf_counter()
        await connector.redis_client.ping() 
        end = time.perf_counter()
        latencies.append((end - start) * 1000)
    
    avg = sum(latencies) / len(latencies)
    print(f'平均延遲: {avg:.2f}ms')
    print(f'最小延遲: {min(latencies):.2f}ms')
    print(f'最大延遲: {max(latencies):.2f}ms')
    
    await connector.disconnect()

asyncio.run(check_latency())
"
```

---

## 🛑 系統停止

當需要停止系統時：

```bash
# 停止所有服務
cd /Users/proerror/Documents/monday

# 停止 Python 進程
if [ -f logs/ops-agent.pid ]; then
    kill $(cat logs/ops-agent.pid)
    rm logs/ops-agent.pid
fi

# 停止 Rust 進程  
if [ -f logs/hft-core.pid ]; then
    kill $(cat logs/hft-core.pid)
    rm logs/hft-core.pid
fi

# 停止 Docker 服務
cd rust_hft
docker-compose down

echo "✅ 系統已完全停止"
```

---

## 📝 一鍵啟動腳本

我已經為您準備了一鍵啟動腳本：

```bash
# 使用一鍵啟動腳本
chmod +x start_hft_system.sh
./start_hft_system.sh
```

---

## ✅ 啟動完成確認

當您看到以下輸出時，系統就已經成功啟動：

```
🎉 HFT 系統啟動完成！

✅ Redis: 運行中 (端口 6379)
✅ ClickHouse: 運行中 (端口 8123)  
✅ HFT 核心: 運行中 (PID: XXXX)
✅ Ops 監控: 運行中 (PID: XXXX)
✅ 集成測試: 通過

📊 系統性能:
  - 平均延遲: X.XX ms
  - 系統狀態: 健康
  - 內存使用: XX%

🎯 系統已準備好處理交易！
```

---

**🚀 現在您可以開始使用 HFT 系統進行高頻交易了！**