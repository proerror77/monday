# Schema v2 配置說明

```yaml
schema_version: v2
engine:
  queue_capacity: 16384
  stale_us: 500000
  top_n: 10
  ack_timeout_ms: 3000
  reconcile_interval_ms: 5000

venues:
  - name: binance
    venue_type: BINANCE
    symbol_catalog:
      - BTCUSDT@BINANCE
    capabilities:
      supports_incremental_book: true
      supports_private_ws: true
      allows_post_only: true
    simulate_execution: false

strategies:
  - name: trend_btc
    strategy_type: Trend
    symbols: [BTCUSDT]
    params:
      kind: trend
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14
    risk_limits:
      max_notional: 100000.0
      max_position: 2.0
      daily_loss_limit: 5000.0
      cooldown_ms: 500

risk:
  risk_type: Default
  global_position_limit: 1000000.0
  global_notional_limit: 10000000.0
  max_daily_trades: 1000
  max_orders_per_second: 50
  staleness_threshold_us: 5000
```

### 說明
- `schema_version`：未設定或非 `v2` 時會回到舊版解析路徑。
- `venue_type`：可填寫交易所字串（如 `BINANCE`、`BITGET`），也接受舊版數值 ID。
- `symbol_catalog`：對應 `shared/instrument` 的 `InstrumentId`，會透過 `config/instruments.yaml` 驗證；若省略且 Instrument catalog 含有對應 venue 的商品，設定 `HFT_AUTOFILL_SYMBOLS=1` 可讓 Runtime 自動補上該 venue 的商品清單。
- `params.kind`：目前支援 `trend`, `imbalance`, `dl`, `lob_flow_grid`，未填則為 `none`。
- `risk`：目前保留原有欄位，`enhanced` 與 `strategy_overrides` 預計後續強型別化。
- `instrument catalog`：預設從 `config/instruments.yaml` 載入，可由環境變數 `HFT_INSTRUMENT_CATALOG` 指定自訂路徑。
- `venue catalog`：建議同步維護 `config/venues.yaml`，提供 endpoint 與能力旗標，可透過環境變數 `HFT_VENUE_CATALOG` 指定自訂路徑，後續 loader 會自動合併。
- `auto_cancel_exchange_only`：僅保留舊配置解析相容性；設為 `true` 會在啟動驗證時失敗。交換端孤兒單必須由可比對 OMS 的 runtime 控制面處理。

### Binance USD-M

USD-M 必須在 Schema v2 中声明为 Binance venue，并显式选择 execution market；直接填写 `BINANCE_FUTURES` 会在 loader 阶段拒绝，避免降级为 Mock：

```yaml
venues:
  - name: binance-usdm
    venue_type: BINANCE
    execution_mode: Testnet   # 当前 Live 仍关闭
    symbol_catalog: [BTCUSDT@BINANCE]
    execution_config:
      market: usdm
      account_mode: BOTH
      position_mode: BOTH
      account_type: usdm
```

Runtime 会把该配置的市场商品规范化为 `BINANCE_FUTURES`/`Perp`。Testnet 默认使用 Binance 官方 `https://demo-fapi.binance.com` REST 与 `wss://demo-fstream.binance.com/private/ws/<listenKey>` 私有流；Live 默认使用 `https://fapi.binance.com` 与 `wss://fstream.binance.com/private/ws/<listenKey>`。Schema v2 venue catalog 的 Spot endpoint 只作为可识别的 catalog 默认值，会按 USD-M 市场替换；显式自定义 endpoint 会保留，Testnet/Live 不能交叉使用 Binance 生产与测试 endpoint。账户模式只支持单向/BOTH；Hedge mode 会在注册前拒绝。Paper 运行必须通过 `simulate_execution: true` 使用现有的 canonical simulated execution matcher，直接构造 USD-M adapter 的 Paper client 不提供模拟成交。

同一 symbol 的 USD-M 与其他市场不能共用 symbol-only Portfolio ledger；配置和运行时都会 fail-closed。当前验证范围是本地 Rust tests/Clippy，Live 保持关闭，未包含真实 Binance/Testnet acceptance。

更多範例可參考 `shared/config/examples/system_v2.yaml`。

> 📁 **Instrument Catalog**：範例位於 `config/instruments.yaml`，同時提供 venue 端點、功能旗標與商品精度資訊。
>
> 📁 **Venue Catalog**：範例位於 `config/venues.yaml`，描述 REST/WS 端點與能力設定，可搭配 instrument catalog 使用。
