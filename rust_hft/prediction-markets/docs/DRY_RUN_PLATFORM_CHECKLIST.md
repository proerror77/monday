# Dry-Run Platform Checklist

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

本文件用于每次上線前的「非下單」檢核，所有項目 pass 後再進入 live dry-run/staging。

## 0. 前置條件

- `PLOY_DRY_RUN__ENABLED=true`
- `PLOY_DEPLOYMENTS_FILE` 指向有效矩陣檔（建議 `data/state/deployments.json`）
- `PLOY_RUN_SQLX_MIGRATIONS=false` 僅限本機實驗，不建議上線前保留
- `.env` / workload env 已備齊且 `ploy-sidecar` 相關憑證不可用於 dry-run
- 預設平台釋出路徑為 `.github/workflows/release-platform.yml`
- 任何仍然指向 `ploy` 單體 binary 的 workflow 都視為 legacy，不是 workspace 預設 deploy 面

## 1. 工具與快照檢查

- `cargo fmt --check`
- `rtk cargo check -p new-ployd`
- `rtk cargo check -p ployctl`
- `rtk cargo check -p ploytui`
- `cargo run -p new-ployd`
- `cargo run -p ployctl -- system status`
- `cargo run -p ployctl -- trading status`
- `cargo run -p ployctl -- deployments list`
- `cargo run -p ploytui`
- `curl -N http://127.0.0.1:8081/api/events/stream`
- `rtk cargo test --test platform_smoke platform_smoke_registers_and_starts_one_deployment -- --nocapture`

## 2. 策略矩陣檢查

- `data/state/deployments.json` 存在且可讀
- 每筆 deployment 至少包含：`deployment_id`、`bundle_id`、`runtime_mode`、`desired_state`
- 若需要 dry-run 送單驗證，目標 deployment 必須是 `runtime_mode=paper`

## 3. 風險控管啟用檢查

- `PLOY_REQUIRE_SQLX_MIGRATIONS=true`
- `PLOY_RUN_SQLX_MIGRATIONS=true`
- `PLOY_RISK__ACCOUNT_RESERVE_PCT`、`PLOY_RISK__CRYPTO_ALLOCATION_PCT`、`PLOY_RISK__SPORTS_ALLOCATION_PCT` 有預期值
- `PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS` 設定符合噪音要求（建議 300）

## 4. 部署腳本固化檢查

- `scripts/install-platform-service.sh` 會在 env 補齊：
  - `PLOY_DEPLOYMENTS_FILE=/opt/ploy/data/state/deployments.json`
  - `PLOY_RUNTIME_ROOT=/opt/ploy/run/platform`
  - `PLOY_SYSTEM_STATUS_FILE=/opt/ploy/run/platform/system-status.json`
  - `PLOY_DEPLOYMENT_STATUS_FILE=/opt/ploy/run/platform/deployments.json`
- 遠端首次部署若 `data/state/deployments.json` 不存在，會使用 repo 內 `data/state/deployments.json.sample` 初始化

## 5. 通過條件

- 以上檢查無 error/stacktrace
- `ployd` / `ployctl` / `ploytui` 可正常啟動且 smoke test 通過
- `/api/deployments` 與 `/api/trading/state` 都能返回有效快照
- `/api/events/stream` 可持續輸出 `system_snapshot` / `deployment_snapshot` / `trading_snapshot`
