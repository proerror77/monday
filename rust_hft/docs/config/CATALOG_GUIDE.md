# Catalog 管理指南

透過獨立的 Instrument / Venue catalog，可以讓系統配置更具宣告式與可重用性。本指南說明如何維護與載入 catalog，以及如何結合 Schema v2 系統配置。

## 目錄

- [Catalog 檔案位置](#catalog-檔案位置)
- [Instrument Catalog](#instrument-catalog)
- [Venue Catalog](#venue-catalog)
- [環境變數覆寫](#環境變數覆寫)
- [Runtime 整合流程](#runtime-整合流程)
- [常見檢查項目](#常見檢查項目)

## Catalog 檔案位置

預設情況下，Runtime 會尋找下列檔案：

| 檔案 | 角色 | 預設路徑 |
|------|------|--------------|
| Instrument catalog | 定義商品精度、合約屬性 | `config/instruments.yaml` |
| Venue catalog | 定義交易所端點、WS、能力旗標 | `config/venues.yaml` |

## Instrument Catalog

Instrument catalog 使用 `shared/instrument::InstrumentCatalogConfig` 結構，最小欄位如下：

```yaml
instruments:
  - symbol: BTCUSDT
    venue: BINANCE
    base: BTC
    quote: USDT
    tick_size: 0.1
    lot_size: 0.001
```

Runtime 會在載入 Schema v2 配置時，自動建構 `InstrumentCatalog`：

- `symbol_catalog` 欄位會以 `SYMBOL@VENUE` 形式存在。
- 若配置中的 `symbol_catalog` 包含 catalog 未定義的合約，會輸出警告。

## Venue Catalog

Venue catalog 以 `shared/instrument::VenueMeta` 儲存交易所端點與能力設定：

```yaml
venues:
  - venue_id: BINANCE
    name: Binance Spot
    rest_endpoint: https://api.binance.com
    ws_public_endpoint: wss://stream.binance.com:9443/ws
    ws_private_endpoint: wss://stream.binance.com:9443/stream
    capabilities:
      supports_incremental_book: true
      supports_private_ws: true
      allows_post_only: true
```

若 Instrument catalog 缺席或缺少特定 venue 的連線資訊，Runtime 會回退到 venue catalog 的設定；反之當 `symbol_catalog` 未填寫時，可在環境變數設定 `HFT_AUTOFILL_SYMBOLS=1` 讓 Runtime 由 Instrument catalog 自動補齊該 venue 的商品清單。未啟用自動補齊時會輸出警告。啟用 `HFT_CONFIG_STRICT` 後，這些警告會直接中止載入流程。

## 環境變數覆寫

| 環境變數 | 作用 |
|----------|-------|
| `HFT_INSTRUMENT_CATALOG` | 指定自訂 Instrument catalog 路徑 |
| `HFT_VENUE_CATALOG` | 指定自訂 Venue catalog 路徑 |
| `HFT_AUTOFILL_SYMBOLS` | 設為 `1`/`true` 時，缺少 `symbol_catalog` 會自動以 Instrument catalog 補齊 |
| `HFT_CONFIG_STRICT` | 設為 `1` 或 `true` 時遇到缺失資訊會直接報錯（詳見 [Validation Modes](VALIDATION_MODES.md)） |

設定環境變數後，即可在不同部署環境中使用對應的 catalog 檔案。

## Runtime 整合流程

1. `config_loader` 讀取 YAML，先嘗試舊版格式，再嘗試 Schema v2。
2. 成功解析 Schema v2 時：
   1. 載入 Instrument catalog（若可用）。
   2. 載入 Venue catalog（若可用）。
   3. 針對每個 venue：
      - 驗證 `symbol_catalog` 是否存在於 Instrument catalog。
      - 補寫 REST/WS 端點與能力旗標。
   4. 整理所有 `symbol_catalog` 作為市場訂閱白名單，若策略使用未列出的商品會發出警告。

## 常見檢查項目

- **Instrument 未定義**：`symbol_catalog` 中的商品未在 Instrument catalog 中定義，Runtime 會發出警告。
- **策略漏加商品**：策略引用未列在任何 venue `symbol_catalog` 的商品，同樣會產生警告。
- **端點缺失**：若兩份 catalog 都沒有提供 REST/WS 端點，Runtime 會保留原始配置（可能需要手動補齊）。

維護 catalog 檔案能讓新增交易所、擴張商品清單時的作業更簡單，也減少重複設定的機會。
