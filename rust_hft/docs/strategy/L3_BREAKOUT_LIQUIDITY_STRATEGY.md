# L3 Breakout Liquidity Strategy

## 結論
- 以 L3 訂單簿重建時間加權深度 (TW-Depth) 畫出流動性地圖，支撐 / 壓力即為局部峰值。
- 破位訊號結合價格穿越、穿越後主動成交量與 CVD/OFI 動量確認；K 線僅用於彙總面板與風控視圖。
- 支撐 / 壓力使用滾動時間窗 (DeltaT) 記憶掛單，破位確認則使用短窗 (delta t) 捕捉動能半衰期。
- 事件推進式回測 (L3 撮合重播) 能保留撮合細節；K 線回測只做基礎校準或無 L3 時的降階方案。

## 訊號層

### 1. 流動性地圖與 TW-Depth
- 在每筆 snapshot / L2 更新到來時，將完整深度快照存入 `LiquidityMap` (`apps/backtest/src/engine.rs`)，為每檔價格累積「掛單量 × 停留時間」。
- 時間窗長度取 `strategy.liquidity_window_secs`，並在過窗時以時間占比遞減權重，維持滾動記憶。
- 對每個價位計算平均深度與瞬時深度，再以 `smoothing_alpha` 做指數平滑，形成 TW-Depth。
- 支撐 (bid) / 壓力 (ask) 帶來源：挑選與 mid 價同側的局部最大深度點，再保留 `support_count` / `resistance_count` 個最強層。

### 2. 破位判定
1. **價格穿越**：當 mid 價向下跌破支撐 `L` 並超過 `price_delta_ticks × tick_size` 時觸發初步警報。
2. **穿越量 vs 深度**：在短窗 `breakout_window_secs` 內計算向下主動成交量 `TT_vol_down`，若 `TT_vol_down >= volume_factor × TW-Depth(L)` 則代表穿越量充足。
3. **順勢確認**：
   - CVD 以 `FlowTracker` 累積買賣差，計算短窗內變化量 `cvd_delta`；要求 `cvd_delta <= -cvd_threshold`。
   - OFI 透過最優掛單量變化轉為流量，短窗總量需 `<= -ofi_threshold`。
4. **對倉操作**：上方條件成立即進空；做多版本鏡像檢查壓力帶。

### 3. 執行與出場
- 進場價以市價滑點調整：空單用 best bid 減 `max_slippage_ticks × tick_size`，多單鏡像 (`apps/backtest/src/engine.rs` `ExecutionManager::enter_short` / `enter_long`)。
- 倉位與穿越量成正比：`calc_short_qty` / `calc_long_qty` 以 `TT_vol / depth` 比率調整，並受 `base_qty` 與 `max_position` 限制。
- 出場條件：
  - mid 收復參考價且 CVD/OFI 翻向 (`ExecutionManager::evaluate_exit`)。
  - 觸發止損、止盈、持倉時間上限或風控停機 (`RiskConfig`)。

### 4. 低毒性做市版本
- 當毒性指標 (如短窗 CVD/OFI 變化) 處於低風險區間時，可在支撐 / 壓力帶內掛被動單。
- 任何破位前兆 (價格穿越、穿越量、CVD/OFI) 成立即撤出被動單並轉為主動追單。

## 窗口與頻率建議

| 市場             | DeltaT (S/R) | delta t (破位) | 價格穿越 (ticks) | 量閾 theta (倍) |
|------------------|--------------|----------------|------------------|----------------|
| 美股大盤股       | 600–1200 s   | 3–15 s         | 1–2              | 0.6–1.2        |
| 股指 / 國債期貨  | 300–900 s    | 1–10 s         | 1–2              | 0.7–1.5        |
| 加密主流幣       | 900–2700 s   | 5–30 s         | 2–4              | 0.5–1.0        |

- 高波動 / 新聞期：縮短 delta t 約 50%，提高 theta 一檔。
- 流動性薄弱：放寬 delta t，降低 theta 與價格穿越距離。

## 數據要求與事件回測
- **資料優先級**：完整 L3 + 撮合事件 → L2 (降階補位) → K 線 (僅監控)。
- **事件重放流程**：
  1. 透過 `apps/backtest` 的 `BacktestEngine` 讀取 NDJSON 事件 (`config/backtest/default.yaml` 指向 `data/backtest/sample.ndjson`)。
  2. `OrderBook` 重建 Level 2/3 狀態，覆寫 `LiquidityMap` 與 `FlowTracker`。
  3. 觸發訊號、執行決策並紀錄交易 (`backtest_trades.csv`, `backtest_summary.csv`, `backtest_metrics.json`)。
- **多參數掃描**：準備多份策略參數覆蓋檔 (`--params`)，以 shell / Makefile 批次呼叫 `cargo run -p hft-backtest -- --config ... --params ...`。未來如需自動化可直接在 Rust 內部實作格點掃描器。

## 樣本量與切分
- 單市場至少 3–6 個月連續 L3 資料作初訓，另保留 1–3 個月樣本外。
- 採時間封鎖 + embargo 的滾動回測，避免資訊洩漏。
- 時段分組：開盤前 30–60 分、常態時段、收盤前 30 分獨立校準。
- 不同資產不可共用 `theta`, `cvd_threshold`, `ofi_threshold` 等動量參數。
- 每週滾動再訓；制度或費率調整時重置。

## K 線限定時的降階方案
- 訊號來源仍建議維持 LOB (若無則使用成交量不平衡 + VWAP 偏離替代)。
- K 線用途：
  - 1m / 5m 作為實時監控與風控儀表。
  - 15m 作大級別支撐 / 壓力背景，但精度低於 TW-Depth。

## 校準與驗證指標
- 參數格點：`theta` 0.5–1.5、`price_delta_ticks` 1–3、`breakout_window_secs` 1–30、`liquidity_window_secs` 依市場建議掃描。
- 評估指標：破位後 10–50 ticks 命中率、每跳盈虧、ASR、穿越量分層梯度、成交率、滑點分佈。
- 風控觀察：尾部日損、連續失敗次數、衝擊成本估計 (納入標的選擇)。

## 實作架構 (Rust)
- **Rust**：
  - `lob-core`：L3 深度重建與快照。
  - `LiquidityMap`：時間加權深度計算 (`apps/backtest/src/engine.rs`)。
  - `FlowTracker`：TT 成交量、CVD、OFI 追蹤。
  - `ExecutionManager`：進出場、滑點控制、佇列位置 / TTL、風控檢查。
  - `risk-control` 套件：庫存、滑點、連虧等限制。

## 最小可運行設定
- `liquidity_window_secs = 900` (15 分)、`breakout_window_secs = 5`、`price_delta_ticks = 2`、`volume_factor = 1.0`。
- `cvd_threshold` 與 `ofi_threshold` 取近 60 日 70 百分位的絕對值，例：80 與 50。
- 先只啟用做空破支撐，回測 60 個交易日觀察穿越量分層是否嚴格遞增，檢驗訊號單調性。
- 推薦輸出 `backtest_trades.csv` 快速檢查穿越時的 TW-Depth 與對應倉位，確認風控與倉位調節是否如預期。

## 後續擴充
- 引入事件標籤 (新聞、撮合方向) 進行毒性分類，動態調整 theta 與滑點容忍度。
- 加入被動單掛單模組與排隊時間估計，與主動追價策略共用風控。
- 擴充 `FlowTracker` 以記錄成交對手與外盤/內盤標籤，支援 CVD 分層分析。
