# 配置驗證模式指引

Runtime 在載入 Schema v2 配置時會進行多層檢查，並提供「寬鬆模式」與「嚴格模式」兩種行為。本文檔整理各種異常情境下的系統反應與處理建議。

## 驗證模式一覽

| 模式 | 環境變數 | 行為 |
|------|----------|------|
| 寬鬆模式 (預設) | 無或 `HFT_CONFIG_STRICT=0` | 解析異常會輸出 `warn!`，仍嘗試載入並啟動 |
| 嚴格模式 | `HFT_CONFIG_STRICT=1` / `true` | 解析異常會輸出 `warn!` 並以錯誤回傳，中止載入 |

## 檢查項目

目前 loader 會在下列情境下觸發檢查：

1. **未知的 venue**：`symbol_catalog` 參考的 `venue_type` 未在 instrument/venue catalog 中定義。
2. **未知的 InstrumentId**：`symbol_catalog` 或策略引用的 `SYMBOL@VENUE` 未存在於 Instrument catalog。
3. **策略商品未掛載於任何 venue**：策略中的 `symbols` 與所有 venue `symbol_catalog` 不吻合。
4. **缺失的 symbol_catalog**：若 YAML 未提供 `symbol_catalog`，預設會輸出警告；設定 `HFT_AUTOFILL_SYMBOLS=1` 可允許自動從 Instrument catalog 補齊。

> 嚴格模式會對上述情境直接回傳錯誤並中止載入；寬鬆模式則僅輸出警告並繼續啟動。

## 推薦使用時機

- **本地開發 / 快速試驗**：維持預設寬鬆模式，讓開發者可以先啟動系統再逐步修正。
- **CI/CD 或生產部署**：在啟動或部署前設定 `HFT_CONFIG_STRICT=1`，確保所有警告都被視為錯誤，避免遺漏配置造成運行期錯誤。

## 組合建議

| 場景 | 建議設定 |
|------|----------|
| 本地開發（測試模擬場景） | 寬鬆模式 + `RUST_LOG=warn` 觀察警告 |
| 預備環境 (staging) | 嚴格模式，確保即時回報缺漏 |
| 生產部署 | 嚴格模式 + CI 腳本預先驗證 |

## 擴充建議

若未來增加更多 Schema 元件（如 Router、Infra）應同步將檢查結果整合進此模式，以維持一致的驗證體驗。
