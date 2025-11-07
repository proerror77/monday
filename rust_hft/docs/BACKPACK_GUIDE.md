# Backpack 交易所整合指南

本指南簡述如何啟用 Backpack Exchange 的行情與下單功能。

## 功能一覽

- **行情**：透過 REST API 輪詢 `/api/v1/depth` 與 `/api/v1/trades`，轉換為統一的 `MarketEvent::Snapshot` / `MarketEvent::Trade`。
- **下單**：實作 `POST /api/v1/order` 以及 `DELETE /api/v1/order`，使用 ED25519 簽章 (`X-API-KEY` / `X-SIGNATURE` / `X-TIMESTAMP` / `X-WINDOW`)。
- **未成交訂單**：`GET /api/v1/orders` 解析為 `OpenOrder` 結構。
- **Paper 模式**：無需 API Key，即可模擬 ACK / Fill / Completed 回報。

## 需求條件

1. 申請 Backpack API key。`X-API-KEY` 為 public key (Base64)，`secret_key` 為對應的 private key (Base64)。
2. 啟用 feature：`cargo build -p hft-runtime --features backpack`。
3. 若需要正式下單，請於配置檔填入正確的 `api_key` 與 `secret_key`。

## 範例配置 (`config/backpack_paper.yaml`)

```yaml
venues:
  - name: "BACKPACK"
    venue_type: "Backpack"
    data_config:
      adapter_type: "backpack"
      ws_url: "wss://ws.backpack.exchange"
      subscribe_depth: true       # 訂閱 depth.<symbol>
      subscribe_trades: true      # 訂閱 trade.<symbol>
      reconnect_interval_ms: 5000 # WebSocket 重連間隔 (毫秒)
      default_symbols:
        - SOL_USDC
    execution_config:
      adapter_type: "backpack"
      mode: "Paper"
      rest_base_url: "https://api.backpack.exchange"
      ws_public_url: "wss://ws.backpack.exchange"
      timeout_ms: 5000            # REST 請求逾時 (毫秒)
      window_ms: 5000             # X-WINDOW 標頭 (預設 5000)
      api_key: ""
      secret_key: ""
```

> 若要實盤下單，將 `mode` 改為 `Live` 並填入 API credentials。

## 注意事項

- REST 行情屬於輪詢模式，預設 500ms 週期。若需要更低延遲，建議日後補上 WebSocket 市場流。
- `data_config` 與 `execution_config` 支援依 YAML 覆寫，未設置時會回退至 `venue` 節點的 `rest/ws` 端點或預設值。
- 簽章字串遵循官方規範：`instruction=<Instr>&key=value...&timestamp=...&window=...`，並以 ED25519 簽名後 Base64 編碼。
- Live 模式可在 `execution_config` 內直接填入 `api_key` 與 `secret_key`，或透過 `accounts` 區段集中管理，系統會自動帶入憑證。
- Backpack 目前未提供原生改單 API，因此 `modify_order` 會返回 `HftError::Execution`。
- `list_open_orders` 僅在 Live 模式且具 API credential 時生效。

## 驗證

```bash
# 編譯帶 backpack feature 的 runtime
cargo check -p hft-runtime --features backpack

# 僅檢查 Backpack adapters
cargo check -p hft-data-adapter-backpack
cargo check -p hft-execution-adapter-backpack
```

如需更多 API 細節，請參考官方文件：https://docs.backpack.exchange/
