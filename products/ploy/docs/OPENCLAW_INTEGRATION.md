# OpenClaw（GitHub: openclaw/openclaw）整合方式

你貼的 `https://github.com/openclaw/openclaw` 是一個 **Node.js Gateway + agent runtime**（不是 Rust crate）。
crates.io 上的 `openclaw`（`openclaw = "0.1.0"`）目前只是 stub，和 GitHub 的 OpenClaw 專案不是同一個可直接嵌入的 framework。

在本 repo，建議用下列方式把 OpenClaw 變成「永遠主動」的 orchestrator，而由 `ployd` control plane 實際管理 deployment、trading state 與 paper intent ingress：

## A) Workspace 預設：OpenClaw 直接呼叫 `ployctl` / control-plane API

1. 先把 `ployd` / `ployctl` 編譯好並放進 PATH：

```bash
cargo build --release -p ployd -p ployctl
export PATH="$(pwd)/target/release:$PATH"
```

2. 啟動平台 daemon，並先確認 control-plane 快照可讀：

```bash
ployd
ployctl system status
ployctl deployments list
ployctl trading status
```

3. 遠端 gateway 控制交易機器（推薦：SSH forced command allowlist）

在交易機器上（跑 `ployd` 的那台）：

- 建一個專用使用者（例如 `ploy`），並把 repo 放在固定路徑
- 把你的 SSH public key 加到 `~ploy/.ssh/authorized_keys`，用 forced command 綁死可執行的指令（只允許 `ployctl` 的 status / deployment / trading 指令，或有限制的 HTTP proxy）：

The old repo-local `ssh_ployctl.sh` wrapper has been removed with the retired
root-runtime archive. Keep the allowlist outside this repo, and make it execute
only a fixed `ployctl` binary plus explicitly allowed subcommands.

然後在遠端（OpenClaw gateway 所在機器）就可以安全地只呼叫 allowlist：

```bash
ssh ploy@TRADING_HOST "ployctl system status"
ssh ploy@TRADING_HOST "ployctl deployments list"
ssh ploy@TRADING_HOST "ployctl deployments inspect example.paper"
ssh ploy@TRADING_HOST "ployctl trading inspect example.paper"
```

這樣 OpenClaw 只要有 SSH 連線能力，就能「遠端永遠主動」地控這台交易機器，但不會變成任意 RCE。

4. 在 OpenClaw 裡建立一個自訂 skill，內容用 bash 或 HTTP 直接調 control-plane：

- `ployctl system status`
- `ployctl deployments list`
- `ployctl deployments apply /opt/ploy/config/deployments/example.paper.json`
- `ployctl deployments pause example.paper`
- `ployctl deployments resume example.paper`
- `ployctl trading inspect example.paper`

目前 workspace 預設路徑是 deployment resources + trading snapshots，不再建議讓 OpenClaw 啟動 retired root runtime。

（可直接用本 repo 提供的 OpenClaw skill 模板：`examples/openclaw/skill-ploy-rpc/`）

### Control-plane API（給 agent 用的最小 operator surface）

- `GET /api/system/status`
- `GET /api/trading/state`
- `GET /api/deployments`
- `GET /api/deployments/:id`
- `PUT /api/deployments/:id`
- `POST /api/deployments/:id/control`
- `POST /api/deployments/:id/intents`（paper-only）

## B) Deployment Resource API

目前 control plane 預設的 deployment resource API：

- `GET /api/deployments`
- `GET /api/deployments/:id`
- `PUT /api/deployments/:id`（body: `deployment_id`、`bundle_id`、`runtime_mode`、`desired_state`）
- `POST /api/deployments/:id/control`（body: `{ "desired_state": "running|paused|stopped" }`）
- `POST /api/deployments/:id/intents`（body: paper trading intent）
- deployment registry 會落地到 `data/state/deployments.json`（可用 `PLOY_DEPLOYMENTS_FILE` 覆寫）。

### Default Control-Plane API

目前 branch 的 OpenClaw / agent operator surface 應只走新的 `ployd`
control plane（需 admin token）：

- `GET /api/system/status`
  - 平台整體狀態、uptime、error count、degraded/recovering 狀態
- `GET /api/deployments`
  - deployment 清單與 `desired_state` / `observed_state` / `deployment_state`
- `GET /api/deployments/:id`
  - 單 deployment 詳情
- `POST /api/deployments/:id/control`
  - pause / resume / stop 與 `enabled|draining|disabled|archived` lifecycle 切換
- `GET /api/trading/state`
  - canonical trading ledger snapshot
- `POST /api/deployments/:id/intents`
  - paper/live intent ingress
- `POST /api/deployments/:id/orders/:order_id/cancel`
  - live order cancel
- `POST /api/deployments/:id/orders/:order_id/replace`
  - live order amend / replace
- `GET /api/events/stream`
  - system / deployment / trading SSE snapshot stream

Do not wire OpenClaw to retired `ploy rpc`, `/api/sidecar/*`, or archived
root-runtime scripts. Those compatibility surfaces have been removed from the
active repo and should not be used for new agent tooling.

若是全局熔斷/降風險，請優先使用 deployment control flow，而不是跨
agent 手動 SELL。

#### OpenClaw skill（bash）建議寫法

在 OpenClaw 的自訂 skill 裡（bash），把 `TRADING_HOST` 固定成你的交易機器，然後每個工具只呼叫被 allowlist 固定住的 `ployctl` 子命令：

```bash
TRADING_HOST="ploy@YOUR_IP_OR_HOSTNAME"

ssh "$TRADING_HOST" "ployctl system status"
ssh "$TRADING_HOST" "ployctl trading status"
```

## OpenClaw-only Runtime Lockdown

若要在交易機器強制禁用內建 agent runtime（改由 OpenClaw 全接管），可設定：

```toml
[agent_framework]
mode = "openclaw"
hard_disable_internal_agents = true
```

## 內建模式（推薦）

若你要盡量使用 repo 內建 runtime（非 OpenClaw 接管），請固定：

```toml
[agent_framework]
mode = "internal"
hard_disable_internal_agents = false
```

或用環境變數：

```bash
export PLOY_AGENT_FRAMEWORK_MODE=internal
export PLOY_AGENT_FRAMEWORK_HARD_DISABLE_INTERNAL_AGENTS=false
```

## B) 深度：讓 OpenClaw 以 MCP Tool 方式控制交易（下一步）

OpenClaw 支援 MCP Servers；下一步可以做：

- 在本 repo 新增 `ploy mcp`（stdio JSON-RPC）提供工具：
  - `event_edge_targets`
  - `event_edge_scan`
  - `event_edge_buy_yes`
- 然後在 OpenClaw gateway 的 MCP config 註冊這個 server，讓 OpenClaw 的 agent 可以「工具調用」而不是純 bash。

如果你要走 B) 路線，告訴我你希望 OpenClaw 用哪個 provider（Claude CLI / OpenAI / 其他），我會把 MCP server binary + 範例 config 補齊。
