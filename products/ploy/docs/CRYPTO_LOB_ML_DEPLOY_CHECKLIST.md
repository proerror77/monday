# Crypto LOB ML Deployment Checklist

本清单用于 `crypto_lob_ml` 在生产环境上线时的 Go/No-Go 决策。

## 1. 适用范围
- 策略：`crypto_lob_ml`
- 关键变更：
  - runtime 正确读取 `MODEL_TYPE / MODEL_PATH / MODEL_VERSION`
  - `model-first` 决策（默认纯模型，window fallback 默认关闭）
  - 进场强约束：`ask <= 0.30` + `EV >= min_edge` + 5m/15m 最后 180s 才允许进场
  - 模型不可用时启动失败（fail-fast），避免误跑旧逻辑

## 2. 上线前（Pre-Deploy）

### 2.1 模型文件与配置
确认以下环境变量已设置（或明确默认值）：
- `PLOY_CRYPTO_LOB_ML__ENABLED=true`
- `PLOY_CRYPTO_LOB_ML__MODEL_TYPE=onnx`
- `PLOY_CRYPTO_LOB_ML__MODEL_PATH=/opt/ploy/models/crypto/<your_model>.onnx`
- `PLOY_CRYPTO_LOB_ML__MODEL_VERSION=<version>`
- `PLOY_CRYPTO_LOB_ML__EXIT_MODE=ev_exit`
- `PLOY_CRYPTO_LOB_ML__ENTRY_SIDE_POLICY=lagging_only`
- `PLOY_CRYPTO_LOB_ML__MAX_ENTRY_PRICE=0.30`
- `PLOY_CRYPTO_LOB_ML__MIN_DIRECTION_STRENGTH=0.05`（过滤 `p_up_blended` 贴近 0.5 的噪声）
- `PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_5M=180`
- `PLOY_CRYPTO_LOB_ML__ENTRY_LATE_WINDOW_SECS_15M=180`
- `PLOY_CRYPTO_LOB_ML__WINDOW_FALLBACK_WEIGHT=0.00`（默认纯模型，可显式调大）
- `PLOY_CRYPTO_LOB_ML__EV_EXIT_BUFFER=0.005`
- `PLOY_CRYPTO_LOB_ML__EV_EXIT_VOL_SCALE=0.02`

### 2.2 训练数据新鲜度（SQL）
```sql
-- A) sync_records 数据新鲜度（核心输入）
SELECT
  symbol,
  MAX(timestamp) AS last_ts,
  EXTRACT(EPOCH FROM NOW() - MAX(timestamp)) AS lag_secs,
  COUNT(*) FILTER (WHERE timestamp >= NOW() - INTERVAL '5 minutes') AS rows_5m
FROM sync_records
WHERE symbol IN ('BTCUSDT','ETHUSDT','SOLUSDT','XRPUSDT')
GROUP BY symbol
ORDER BY symbol;
```

```sql
-- B) 结算标签覆盖（5m/15m）
WITH by_market AS (
  SELECT
    market_slug,
    CASE
      WHEN market_slug ILIKE '%15m%' THEN '15m'
      WHEN market_slug ILIKE '%5m%' THEN '5m'
      ELSE 'other'
    END AS horizon
  FROM pm_token_settlements
  WHERE resolved = TRUE
    AND settled_price IS NOT NULL
    AND market_slug IS NOT NULL
  GROUP BY market_slug
)
SELECT horizon, COUNT(*) AS resolved_markets
FROM by_market
GROUP BY horizon
ORDER BY horizon;
```

```sql
-- C) 门槛价元数据覆盖（price_to_beat + end_time）
SELECT
  horizon,
  symbol,
  COUNT(*) AS markets,
  COUNT(*) FILTER (WHERE price_to_beat IS NOT NULL) AS with_price_to_beat,
  COUNT(*) FILTER (WHERE end_time IS NOT NULL) AS with_end_time,
  MAX(updated_at) AS last_updated_at
FROM pm_market_metadata
WHERE horizon IN ('5m', '15m')
GROUP BY horizon, symbol
ORDER BY horizon, symbol;
```

### 2.3 模型训练（可选示例）
```bash
# 5m 专用模型（TCN）
python3 scripts/train_crypto_lob_tcn_onnx_from_db.py \
  --source sync_records \
  --horizon 5m \
  --lookback-hours 336 \
  --output ./models/crypto/lob_tcn_5m.onnx \
  --meta ./models/crypto/lob_tcn_5m.meta.json

# 15m 专用模型（TCN）
python3 scripts/train_crypto_lob_tcn_onnx_from_db.py \
  --source sync_records \
  --horizon 15m \
  --lookback-hours 336 \
  --output ./models/crypto/lob_tcn_15m.onnx \
  --meta ./models/crypto/lob_tcn_15m.meta.json
```

## 3. 上线步骤（Deploy）
1. 部署新二进制与模型文件。
2. 应用环境变量并重启服务。
3. 首次启动后检查日志是否出现模型加载成功；若模型无效，进程会直接报错退出（预期行为）。

## 4. 上线后验证（Post-Deploy, 5-30 分钟）

### 4.1 执行概览（SQL）
```sql
SELECT
  COUNT(*) FILTER (WHERE metadata->>'signal_type' = 'crypto_lob_ml_entry') AS entry_cnt,
  COUNT(*) FILTER (WHERE metadata->>'signal_type' = 'crypto_lob_ml_exit')  AS exit_cnt,
  MIN(executed_at) AS first_exec_at,
  MAX(executed_at) AS last_exec_at
FROM agent_order_executions
WHERE agent_id = 'crypto_lob_ml'
  AND executed_at >= NOW() - INTERVAL '30 minutes';
```

### 4.2 模型是否真正生效（SQL）
```sql
SELECT
  COALESCE(metadata->>'model_type', 'missing') AS model_type,
  COUNT(*) AS cnt
FROM agent_order_executions
WHERE agent_id = 'crypto_lob_ml'
  AND metadata->>'signal_type' = 'crypto_lob_ml_entry'
  AND executed_at >= NOW() - INTERVAL '30 minutes'
GROUP BY 1
ORDER BY cnt DESC;
```

### 4.3 model-first 混合权重检查（SQL）
```sql
SELECT
  metadata->>'p_up_blend_w_model'  AS w_model,
  metadata->>'p_up_blend_w_window' AS w_window,
  COUNT(*) AS cnt
FROM agent_order_executions
WHERE agent_id = 'crypto_lob_ml'
  AND metadata->>'signal_type' = 'crypto_lob_ml_entry'
  AND executed_at >= NOW() - INTERVAL '30 minutes'
GROUP BY 1,2
ORDER BY cnt DESC;
```

### 4.4 风控层仍由 Coordinator 执行（SQL）
```sql
SELECT
  decision,
  COUNT(*) AS cnt
FROM risk_gate_decisions
WHERE agent_id = 'crypto_lob_ml'
  AND decided_at >= NOW() - INTERVAL '30 minutes'
GROUP BY decision
ORDER BY cnt DESC;
```

### 4.5 进场硬约束校验（SQL）
```sql
SELECT
  COUNT(*) AS entry_cnt,
  COUNT(*) FILTER (
    WHERE (metadata->>'signal_market_price')::numeric <= 0.30
  ) AS capped_price_cnt,
  COUNT(*) FILTER (
    WHERE (metadata->>'window_remaining_secs')::bigint <= 180
  ) AS late_window_cnt
FROM agent_order_executions
WHERE agent_id = 'crypto_lob_ml'
  AND metadata->>'signal_type' = 'crypto_lob_ml_entry'
  AND executed_at >= NOW() - INTERVAL '30 minutes';
```

## 5. Go / No-Go 标准
Go（通过）
- `sync_records` 最新 lag 在可接受范围（例如 < 10s，按你们实际 SLA）
- `model_type` 统计显示为 `onnx`
- 混合权重符合预期（默认 `1.00 / 0.00`）
- `risk_gate_decisions` 有正常记录，未出现异常大面积 `BLOCKED`

No-Go（阻断）
- 模型加载失败（启动被 fail-fast 阻断）
- `entry` 信号元数据缺失关键字段（`model_type`、blend 权重等）
- 数据源明显滞后（collector 异常）

## 6. 回滚步骤
1. 将 `PLOY_CRYPTO_LOB_ML__ENABLED=false`（最快止损）或切换到上一个稳定 `MODEL_PATH`。
2. 重启服务。
3. 复跑第 4 节 SQL，确认异常行为停止。
4. 在修复后再按本清单重新灰度上线。
