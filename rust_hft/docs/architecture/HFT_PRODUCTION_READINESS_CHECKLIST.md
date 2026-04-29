# HFT Production Readiness Checklist

這份清單把系統從「低延遲行情處理」推進到「可上線交易內核」。目標不是每個局部都追絕對最快，而是讓路徑短、隊列可控、抖動可定位、過載可降級，並且永遠不要在狀態未知時交易。

本季可執行範圍和里程碑以
[`HFT_2026_Q2_EXECUTION_PLAN.md`](HFT_2026_Q2_EXECUTION_PLAN.md) 為準；
隊列 owner、容量與滿載行為以
[`HFT_QUEUE_TOPOLOGY_CONTRACT.md`](HFT_QUEUE_TOPOLOGY_CONTRACT.md) 為準；
本文保留為 production readiness checklist。

## 1. Low Latency Path

範圍：`WebSocket -> raw queue -> parser -> order book -> feature -> signal`

P0 必須完成：
- 行情隊列和訂單回報隊列分離。
- hot path 禁止日志、DB、await、動態分配、`serde_json::Value`、大對象 clone。
- 所有跨線程隊列使用 bounded queue。
- 隊列滿時有明確策略：行情 latest-wins，signal 過期丟棄，日志 drop when full，訂單回報不可丟。
- 每條消息記錄 `recv -> queue wait -> parse -> book -> feature -> signal` 的 p50/p95/p99/p999/max。
- 本地 order book 遇到 sequence gap 必須自動 rebuild。

P1 低延遲核心：
- WebSocket receiver 和 engine thread 分離。
- engine thread 固定 CPU core，避免被日志、metrics、IRQ 打斷。
- typed/borrowed parser 替代 `serde_json::Value`。
- 價格和數量用 fixed-point 或受控 Decimal，不用 `f64` 存交易價格。
- `OrderBook<50>` 使用預分配結構，避免 hot path HashMap 和 Vec 增長。
- metrics 采樣，hot path 只寫輕量 trace buffer。

P2 抖動治理：
- 記錄 queue depth、queue wait time、dropped count。
- 拆分 fast strategy 和 slow strategy。
- slow strategy、ML、DTW、research signal 不得阻塞行情主鏈路。
- 加 overload mode：降采樣 metrics、丟 debug log、停 slow strategy、只維護 book、暫停開倉、cancel all。
- 加 allocation audit 和 latency regression test。

## 2. Trading Correctness Path

範圍：`signal -> risk -> order -> ack/fill/cancel -> position -> reconciliation`

P0 必須完成：
- OrderManager 單線程擁有訂單狀態。
- RiskEngine 單線程擁有 position/exposure。
- 策略只能產生 Signal 或 OrderIntent，不能直接改訂單和倉位。
- execution report / order ack / fill event 不和 market data 共用隊列。
- 訂單狀態機覆蓋：`pending_new`、`acknowledged`、`partially_filled`、`filled`、`pending_cancel`、`cancelled`、`cancel_rejected`、`rejected`、`expired`、`unknown`。
- 處理 fill 先於 ack、cancel/fill 競態、重複回報、亂序回報、本地與交易所狀態不一致。

P1 上線安全：
- PositionManager、BalanceManager、ExposureManager 定期和交易所 REST / user stream 對賬。
- RateLimitManager 管理 REST weight、order rate、cancel rate、snapshot rate、WebSocket connection limit。
- ExchangeRuleManager 快速校驗 `tick_size`、`step_size`、`min_qty`、`min_notional`、fee tier、contract multiplier。
- CostModel 要求 `expected_edge > fee + spread + slippage + latency risk + safety_margin`。
- SignalLifecycleManager 管理 `created_ts`、`valid_until`、`source_book_seq`、`confidence`、`cancel_condition`、`max_slippage`、`max_latency`。
- Cancel / Replace 策略必須限制 cancel storm、replace storm 和 self-induced rate limit。

P2 多策略：
- StrategyCoordinator 防止多策略方向沖突。
- SelfTradePrevention 防止自成交。
- RiskBudgetManager 分配 capital、risk、symbol、order-rate budget。
- ShadowTradingEngine 支持 replay、paper、shadow live、small live、full live。

當前本地契約：策略仍輸出穩定的 `OrderIntent`；live/paper/shadow 路徑在
風控或執行前應包成 `OrderIntentEnvelope`，由 `created_ts`、`valid_until`、
`source_book_seq`、`max_latency_us` 等欄位拒絕過期或過時意圖。

## 3. Safety & Observability Path

範圍：`time -> network -> metrics -> alert -> degrade -> recovery`

P0 必須完成：
- 延遲統計使用 monotonic clock，交易事件同時保存 wall clock。
- 記錄 `exchange_ts - local_recv_ts`，監控 clock drift。
- chrony/NTP 基線；多機或跨交易所套利再上 PTP。
- 監控 p99 latency spike、queue depth high、book gap、disconnect、order reject spike、position mismatch、PnL drawdown、rate limit nearing、clock drift、data stale。
- SecretManager 確保 API key 最小權限、無提現權限、secret 不進日志。
- RecoveryManager 預設崩潰/重連/狀態未知流程：停止開新倉、拉 account/open orders/position、重建 order book、對賬、確認無未知訂單後再交易。

P1 運維穩定：
- NetworkPathBenchmark 實測 Tokyo / Singapore / Hong Kong / Frankfurt 等 region 的 WebSocket delay、REST order ack、user stream ack、disconnect、packet loss、p99/p999 RTT。
- ReconnectManager 管理 DNS cache、TLS handshake、WebSocket reconnect、24h 主動輪換、重連後 book rebuild。
- MarketDataSanityChecker 過濾亂序、重複、gap、bid > ask、spread 異常、價格跳變、stale book、交易所局部故障。
- MonitoringAlerting 分級：warning、degrade、stop opening、cancel all、shutdown。
- 數據存儲拆分：RawMsg append-only，MarketEvent binary/parquet，FeatureSnapshot parquet，Signal parquet/ClickHouse，Order/Trade event log。

P2 系統層：
- 生產 Linux 固定 Rust version、target CPU、compiler flags、kernel version、instance type、CPU governor、config version。
- CPU governor performance，關閉 swap。
- CPU isolation、IRQ affinity、NUMA bind。
- receiver core 和 engine core 分離，hot path core 不處理網卡中斷。
- AWS ENA / NIC queue 綁定單獨驗證。

## Current Bitget Audit Evidence

命令：

```bash
cargo run -p hft-data-adapter-bitget --example latency_audit --release -- \
  --symbol BTCUSDT \
  --depth-channel books1 \
  --queue-kind sync-channel \
  --queue-capacity 1024 \
  --max-messages 500 \
  --max-runtime-secs 60
```

結果：receiver 使用 Tokio current-thread runtime，engine consumer 使用獨立 OS thread；`raw_queue_wait` 只統計 frame 入隊後到 engine 取出的等待時間。`--queue-kind sync-channel` 是 std bounded channel 基線；`--queue-kind spsc-spin` 使用預分配 SPSC ring，並避免 receiver 端把 tungstenite text frame 再拷貝成 `String`。

| Metric | p50 | p95 | p99 | p999 / max | Notes |
|--------|-----|-----|-----|------------|-------|
| `ws_receive_gap` | 16,691,708ns | 397,709,708ns | 954,881,500ns | 2,028,413,834ns | 真實消息到達間隔，不是本地處理延遲 |
| `raw_queue_depth` | 1 | 3 | 4 | 5 | 1024 容量下無擁塞 |
| `raw_queue_wait` | 10,125ns | 29,750ns | 385,958ns | 881,416ns | queue/scheduler tail 仍是當前主要尖刺 |
| `envelope_parse` | 3,625ns | 8,125ns | 15,208ns | 22,417ns | parser 本身不是主瓶頸 |
| `event_convert` | 5,584ns | 12,209ns | 23,209ns | 75,750ns | Decimal/event conversion 可接受 |
| `engine_total` | 21,667ns | 47,875ns | 518,709ns | 895,583ns | p99 仍由本機 thread scheduling 主導 |

低延遲 busy-poll 模式：

```bash
cargo run -p hft-data-adapter-bitget --example latency_audit --release -- \
  --symbol BTCUSDT \
  --depth-channel books1 \
  --queue-kind spsc-spin \
  --queue-capacity 1024 \
  --max-messages 500 \
  --max-runtime-secs 60 \
  --idle-timeout-us 50 \
  --receiver-core 1 \
  --engine-core 2 \
  --busy-poll
```

結果：`raw_queue_wait p50=500ns p95=8,959ns p99=1,473,000ns`，`engine_total p50=8,250ns p95=14,916ns p99=1,481,083ns`。busy-poll 已把 common path 壓到微秒級，但 macOS 仍會偶發 deschedule engine thread；要穩定 p99/p999，下一步需要 Linux dedicated core、CPU pinning/isolation、IRQ 隔離與 governor performance。

最新本機 A/B 探針：

| Run | Queue | Busy poll | `raw_queue_wait p99` | `engine_total p99` |
|-----|-------|-----------|----------------------|--------------------|
| `macos-sync2-20260429-125113-6f16c645` | `sync-channel` | false | 16,707,208ns | 16,743,042ns |
| `macos-spsc2-20260429-125258-6f16c645` | `spsc-spin` | false | 1,752,167ns | 1,754,125ns |
| `macos-spsc-busy2-20260429-125341-6f16c645` | `spsc-spin` | true | 25,915,292ns | 25,936,333ns |

解讀：SPSC + producer `unpark` 已降低非 busy-poll 交接尾延遲；busy-poll 在 macOS 無 dedicated core 時仍可能被本地調度污染。這不是 Linux production p99 結論。

Linux staging 驗證入口：

```bash
scripts/linux_latency_preflight.sh
QUEUE_KIND=spsc-spin PROCESS_CORES=1,2 RECEIVER_CORE=1 ENGINE_CORE=2 MAX_MESSAGES=5000 MAX_RUNTIME_SECS=300 \
  scripts/run_bitget_latency_linux.sh
```

每次 Linux run 會產生 `target/latency-audit/<run-id>/metadata.txt`、`stdout.log`、`summary.json`。比較 p99/p999 時必須同時保存 host、kernel、Rust、RUSTFLAGS、receiver/engine core、queue kind、idle timeout 和 busy-poll 配置，避免把不同環境的數據混在一起。

多 run 比較使用：

```bash
scripts/summarize_bitget_latency.py target/latency-audit
```

解讀：Bitget 本地 parser/convert 路徑的 p99 仍在數十微秒級內；更大的問題是 receiver -> engine 調度尾延遲。下一步優先在 Linux staging 上跑 pinned engine thread / CPU isolation / IRQ affinity 驗證，而不是繼續微調 JSON parser。Mac 本機可以完成架構、測試和 replay，但不能作為最終 p99/p999 依據。
