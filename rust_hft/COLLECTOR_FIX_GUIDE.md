# HFT Collector 表結構修復指南

## 問題總結

通過代碼分析，我發現了數據收集器無法插入數據的根本原因：

**問題**：當前的 Binance collector 實現沒有創建 `binance_orderbook` 表，也沒有將訂單簿數據寫入該表。代碼中註釋明確說明"不再創建 binance_orderbook（逐價位行表）"，但 ClickHouse 中的實際表結構顯示需要這個表。

**根本原因**：
1. `setup_tables()` 函數沒有創建 `binance_orderbook` 表
2. `process_message()` 函數沒有將訂單簿更新寫入 `binance_orderbook` 表
3. `flush_data()` 函數沒有處理 `orderbook_data` 緩衝區

## 修復內容

我已經修復了以下文件：
- `/Users/proerror/Documents/monday/rust_hft/apps/collector/src/exchanges/binance.rs`

### 修復詳情

#### 1. 表創建修復 (setup_tables 函數)
```rust
// 添加了 binance_orderbook 表創建
CREATE TABLE IF NOT EXISTS {}.binance_orderbook (
    ts DateTime64(3),                    // 匹配現有表
    symbol LowCardinality(String),       // 匹配現有表
    side Enum8('bid' = 1, 'ask' = 2),   // 匹配現有表
    price Float64,                       // 匹配現有表
    qty Float64,                         // 匹配現有表
    update_id Int64                      // 匹配現有表
) ENGINE = MergeTree()
ORDER BY (symbol, ts, side, price)
```

#### 2. 數據處理修復 (process_message 函數)
```rust
// 為每個 bid/ask 價位創建記錄
for (price_str, qty_str) in &depth_update.bids {
    if let (Ok(price), Ok(qty)) = (price_str.parse::<f64>(), qty_str.parse::<f64>()) {
        if qty > 0.0 {
            let row = BinanceOrderbookRow {
                ts,
                symbol: depth_update.symbol.clone(),
                side: "bid".to_string(),
                price,
                qty,
                update_id: depth_update.final_update_id,
            };
            buffers.orderbook_data.push(serde_json::to_value(row)?);
        }
    }
}
// 相同邏輯處理 asks...
```

#### 3. 數據刷新修復 (flush_data 函數)
```rust
// 添加了 orderbook_data 處理
if !buffers.orderbook_data.is_empty() {
    let n = buffers.orderbook_data.len();
    let _ = sink.insert_json_rows("binance_orderbook", &buffers.orderbook_data).await?;
    tracing::info!("插入 {} 条 Binance 订单簿记录", n);
    buffers.orderbook_data.clear();
}
```

## 部署步驟

### 使用 Docker（推薦）
- 構建映像
```bash
bash apps/collector/docker-build.sh
```
- 本地運行（使用 ClickHouse Cloud）
```bash
export CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
export CLICKHOUSE_DB="hft_db"
export CLICKHOUSE_USER="default"
export CLICKHOUSE_PASSWORD='s9wECb~NGZPOE'

bash apps/collector/docker-run.sh \
  EXCHANGE=binance \
  TOP_LIMIT=50 \
  DEPTH_MODE=both \
  DEPTH_LEVELS=20 \
  LOB_MODE=both \
  STORE_RAW=0
```

推送到 AWS ECR 之後，ECS 以相同映像運行即可，無需 scp 二進制。

### 1. 重新編譯
```bash
cd /Users/proerror/Documents/monday/rust_hft/apps/collector
cargo build --release
ls -lh target/release/hft-collector
```

### 2. 部署到 ECS（當實例可訪問時）
```bash
# 上傳新二進制文件（注意路徑在 apps/collector 下）
scp -i hft-admin-ssh-20250926144355.pem \
    apps/collector/target/release/hft-collector \
    root@<ECS_IP>:/root/hft-collector-fixed

# SSH 到實例
ssh -i hft-admin-ssh-20250926144355.pem root@<ECS_IP>

# 停止服務
systemctl stop hft-collector

# 備份舊版本
cp /root/hft-collector-real /root/hft-collector-backup-$(date +%s)

# 替換二進制文件
mv /root/hft-collector-fixed /root/hft-collector-real
chmod +x /root/hft-collector-real

# 啟動服務
systemctl start hft-collector

# 檢查狀態和日誌
systemctl status hft-collector
journalctl -u hft-collector -f
```

### 3. 驗證修復

#### 檢查服務日誌
```bash
journalctl -u hft-collector -n 50
```
應該看到：
- ✅ "插入 X 条 Binance 订单簿记录"
- ✅ "插入 X 条 Binance 交易记录"

#### 檢查 ClickHouse 數據
```bash
clickhouse-client \
  --host 'kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud' \
  --port 9440 --secure \
  --user default \
  --password 's9wECb~NGZPOE' \
  --database hft_db \
  --query 'SELECT count(*) FROM binance_orderbook'
```

#### 檢查最新數據
```bash
clickhouse-client \
  --host 'kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud' \
  --port 9440 --secure \
  --user default \
  --password 's9wECb~NGZPOE' \
  --database hft_db \
  --query 'SELECT * FROM binance_orderbook ORDER BY ts DESC LIMIT 10'
```

## 期望結果

修復後，應該看到：
1. **服務日誌**：定期顯示訂單簿和交易數據插入
2. **ClickHouse 表**：`binance_orderbook`、`binance_trades`（以及 `binance_l1`，若啟用）表中有新數據
3. **數據結構**：數據符合現有表的 schema 要求

## 故障排除

如果修復後仍有問題：

1. **檢查表結構匹配**：
   ```sql
   DESCRIBE binance_orderbook;
   ```

2. **檢查權限**：
   ```sql
   SHOW GRANTS FOR default;
   ```

3. **檢查網絡連接**：服務日誌中應該顯示 WebSocket 連接成功

4. **重新清空表**（如果需要）：
   ```sql
   TRUNCATE TABLE binance_orderbook;
   TRUNCATE TABLE binance_trades;
   ```

## 注意事項

- 修復版本會同時創建 `binance_orderbook` 和 `binance_trades` 表
- 只有數量 > 0 的價位會被記錄
- 每次 depth update 都會產生多條 orderbook 記錄（每個價位一條）
- 服務會自動重連和重試，具有故障恢復能力

---

## Bitget 重複成交與 L1 缺失修復

問題：
- Bitget 每條成交重複 ~3 次（網路重連期間重放）
- Bitget L1 表無數據

修復內容：
- 在 `apps/collector/src/exchanges/bitget.rs` 中新增去重緩存（按 `symbol#trade_id` 去重）
- 在處理 `books` 訊息時即時計算 BBO，寫入 `bitget_l1`
- 將 Bitget 成交表預設引擎調整為 `ReplacingMergeTree(local_ts)` 並以 `(symbol, trade_id)` 做排序鍵（新表生效）

一次性去重現有數據：
- 腳本：`scripts/clickhouse/bitget_dedup.sql`
- 執行器：`scripts/clickhouse/run_bitget_dedup.sh`（使用 ClickHouse HTTP）

部署補充：
- 將 AsterDEX、Bybit、Hyperliquid 收集器納入 `apps/collector/final-deploy.sh` 和 `apps/collector/build-on-ecs.sh` 的 docker-compose 配置

驗證：
- ClickHouse：`SELECT count(), countDistinct(trade_id) FROM hft_db.bitget_trades` 應接近 100% 唯一
- L1：`SELECT * FROM hft_db.bitget_l1 ORDER BY ts DESC LIMIT 10` 應有連續更新
