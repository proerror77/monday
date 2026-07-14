# crypto_lob_ml 架构层 vs 策略层边界

## 1. 架构层（System / Runtime）
- `PlatformBootstrapConfig::from_app_config` 负责把环境变量注入策略配置（如 `MODEL_TYPE/MODEL_PATH/MODEL_VERSION`）。
- 模型加载失败时属于启动阻断（fail-fast），不会降级到旧启发式或 collect-only。
- 风控和下单准入由 Coordinator 统一负责（本次未改变这层职责）。

## 2. 策略层（Signal / Decision）
- `crypto_lob_ml` 用序列 LOB 特征输出 `p_up_model`（5m=60 秒窗口，15m=180 秒窗口，11 维特征）。
- `p_up_window` 仅作为可选 fallback；默认关闭（`PLOY_CRYPTO_LOB_ML__WINDOW_FALLBACK_WEIGHT=0.00`）。
- 当前默认混合：`p_up = 1.00 * p_up_model + 0.00 * p_up_window`（可用 env 显式开启窗口权重）。
- 退出默认走 `ev_exit`：当市场价格高于模型公平价值时退出（纯模型驱动）。
- 进场默认走 `lagging_only`：只考虑更便宜的一侧做 EV 判断。
- 进场硬条件：`ask <= 0.30` 且 `EV >= min_edge`。
- 5m / 15m 市场都只允许在最后 `180s` 内进场（分别由 `ENTRY_LATE_WINDOW_SECS_5M/15M` 控制）。
- `ev_exit_buffer` 提供退出阈值，避免过度抖动。
- `ev_exit_vol_scale` 让退出阈值随窗口不确定性动态放大。

## 3. 训练层（Offline ML）
- 训练入口：`scripts/train_crypto_lob_tcn_onnx_from_db.py --source sync_records --horizon 5m|15m`。
- 标签来自 `pm_token_settlements`（resolved 结果）。
- 特征来自 `sync_records`，阈值与时间维度来自 `pm_market_metadata`（`price_to_beat`, `end_time`, `horizon`, `symbol`）。
- 每条样本包含 Binance LOB + 动量 + `spot_price` + `remaining_secs` + `price_to_beat` + `distance_to_beat`，并按 1s 序列对齐。
- 训练产物通过 `MODEL_TYPE/MODEL_PATH/MODEL_VERSION` 接入 runtime。

## 4. 上线清单
- 参考：`docs/CRYPTO_LOB_ML_DEPLOY_CHECKLIST.md`
