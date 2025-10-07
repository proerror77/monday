# Collector CLI/ENV Reference

## CLI (ms subcommand)

- `--exchange <name>`: binance | bybit | bitget | asterdex | hyperliquid
- `--top-limit <N>`: Top‑N depth for snapshot_books (default 20)
- `--shard-index <i>` / `--shard-count <N>`: Sharded collection by symbol (default 0/1)
- `--lob-mode <snapshot|row|both>`: Write Top‑N only, L2 rows only, or both (default snapshot)
- `--store-raw`: Store raw WS event summaries into `raw_ws` (default off)
- `--metrics-addr <host:port>`: Expose Prometheus metrics at `http://addr/` (default none)

Common env overrides (affect adapters if present):

- `COLLECTOR_DEPTH_MODE`: `limited` or `incremental` (diff); default varies by adapter
- `COLLECTOR_DEPTH_LEVELS`: 5/10/20/50… (adapter-dependent)
- `COLLECTOR_DEPTH_FREQ`: `100ms`/`150ms`/… (adapter-dependent)

## Per‑exchange ENV

### Binance
- `BINANCE_USE_LIMITED`: `true` → subscribe `symbol@depth{levels}@{freq}`; else `symbol@depth`
- `BINANCE_DEPTH_LEVELS`: default 20 (when limited)
- `BINANCE_DEPTH_FREQ`: default `100ms`
- `BINANCE_MINITICKER_ARR`: `true` → subscribe `!miniTicker@arr` (全市场 miniTicker 数组)，并取消按 symbol 逐条 miniTicker 订阅
  - 兼容别名：`COLLECTOR_MINITICKER_ARR`

Futures（USDT-M）
- `BINANCE_FUT_MINITICKER_ARR`: `true` → 订阅 `!miniTicker@arr`（futures 入口），并取消逐条 miniTicker
  - 兼容别名：`COLLECTOR_FUT_MINITICKER_ARR`

Generic fallbacks: `COLLECTOR_DEPTH_MODE/LEVELS/FREQ` (take precedence)

### Bybit (v5)
- `BYBIT_CATEGORY`: `spot` | `linear` (default `spot`)
- `BYBIT_DEPTH_LEVELS`: default `50`

Generic fallback for levels: `COLLECTOR_DEPTH_LEVELS`

### Bitget
- `BITGET_INST_TYPE`: `USDT-FUTURES` | `SPOT` | `COIN-FUTURES` | `USDC-FUTURES` (default `USDT-FUTURES`)
- `BITGET_DEPTH_CHANNEL`: `books` (incremental) | `books1` | `books5` | `books15` (default `books5`)

Generic fallbacks: `COLLECTOR_INST_TYPE` (same as inst_type), `COLLECTOR_BITGET_CHANNEL` (same as depth_channel)

Notes:
- books: initial `snapshot` then `update`; local merge; sequence检查；尝试 CRC 校验
- books1/5/15: 每条都是快照；按原始字符串计算 CRC32 并校验（保留尾零）

### Aster (Perp)
- `ASTER_USE_DIFF`: `true` → `symbol@depth@{freq}`; else `symbol@depth{levels}@{freq}`
- `ASTER_DEPTH_LEVELS`: default 20
- `ASTER_DEPTH_FREQ`: default `100ms`
- `ASTER_INIT_REST_SNAPSHOT`: `true` → 启动时 REST 快照初始化（默认关闭）

Generic fallbacks: `COLLECTOR_DEPTH_MODE/LEVELS/FREQ`

### Hyperliquid
- 默认订阅 `l2Book,trades`。
- 通过 `HYPERLIQUID_WS_TOPICS` 配置额外主题（逗号分隔），例如：`HYPERLIQUID_WS_TOPICS=l2Book,trades,candles,liquidations`

## Prometheus Metrics
- Endpoint: `--metrics-addr <addr>`
- `collector_events_total{exchange,type}`: snapshot/update/trade 事件计数
- `collector_errors_total{exchange}`: 事件错误计数
- 新增：
  - `collector_reconnects_total{exchange}`: 重连次数
  - `collector_error_kinds_total{exchange,kind}`: 各类错误计数（connect_failed/ws_error/process_error/flush_error/raw_flush_error/heartbeat_error）
  - `collector_last_error_kind_seconds{exchange,kind}`: 最近一次该类错误发生的 Unix 秒时间戳
  - `collector_batch_insert_seconds{exchange,table}`: 批量落库耗时（秒），`table` 为 `unified` 或 `raw_ws`

## Funding (USDT Perp)

- `--collect-funding`: enable funding collection
- WS (preferred):
  - Binance/Aster: subscribes `<symbol>@markPrice@1s`
- REST fallback (optional, default off):
  - `COLLECTOR_FUNDING_REST_FALLBACK=true`
  - `BYBIT_FUNDING_URL_TMPL` and/or `BITGET_FUNDING_URL_TMPL` with `{symbol}` placeholder
  - `COLLECTOR_FUNDING_POLL_SECS` (default 120)
