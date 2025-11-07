Quotes-only 模式（只連行情、不下單）

目的
- 在生產相同的組件組合下，驗證市場數據接入、橋接、引擎主循環的整體健康，而避免任何下單行為。

開啟方式
- YAML（推薦）：在 system.yaml 置頂加入 `quotes_only: true`（v2 schema）
  - 範例：`config/dev/binance_quotes_only.yaml` 已包含 `quotes_only: true`
- CLI 旗標：
  - `cargo run -p hft-live --features "binance,metrics" -- --config config/dev/binance_quotes_only.yaml --quotes-only --exit-after-ms 30000`
- 環境變量（等效）：
  - `HFT_QUOTES_ONLY=1 cargo run -p hft-live --features "binance,metrics" -- --config config/dev/binance_quotes_only.yaml --exit-after-ms 30000`

行為變更
- SystemBuilder.auto_register_adapters() 在 `quotes_only: true` 或 `HFT_QUOTES_ONLY=1` 時僅註冊行情，不註冊任何執行客戶端。
- SystemRuntime.start() 跳過建立執行隊列與啟動 ExecutionWorker。
- hft-live CLI 會在 `--quotes-only` 時自動設定 `HFT_QUOTES_ONLY=1`。

最小範例配置
- 檔案：`config/dev/binance_quotes_only.yaml`
- 內容：只配置 Binance 公開行情與 `symbol_catalog`，不配置策略（`strategies: []`）。

驗收要點
- 日誌中應可見：
  - 「STATUS | ... exec_evts=0 | new=0 ack=0 fill=0 ...」
  - AdapterBridge 攝取統計「陳舊 0」（若發現陳舊，請調整 adapter 時間戳單位或提高 `engine.stale_us`）。
- 指標（需 `metrics` feature）：
  - Prometheus 指標中不應有任何下單相關計數增長（orders_*）。

常見問題
- 陳舊事件（stale）
  - 原因：交易所時間戳以毫秒（ms）提供，系統採用微秒（μs）。
  - 解法：在相應 adapter 內將 `ms` 轉換為 `μs`（×1000）。目前 Binance/Bybit/Bitget/Asterdex/Hyperliquid 已對齊。
