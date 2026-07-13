# OpenClaw ↔ ploy（遠端工具）快速開始

這個目錄提供「可直接複製」到 OpenClaw 的 skill 模板，讓 OpenClaw agent 透過 SSH 呼叫交易機器上的 `ploy rpc`（JSON-RPC）來查詢/下單。

## 前置：交易機器（跑 ploy 的那台）

1) 編譯：

```bash
cargo build --release
```

2) 用 SSH forced-command allowlist 鎖死可執行指令（推薦）：

參考 `docs/OPENCLAW_INTEGRATION.md`。

3) 如果要允許 RPC 下單/撤單（寫入）：

在交易機器的服務環境加：

```bash
export PLOY_RPC_WRITE_ENABLED=true
```

（預設為 false；agent 只能讀取與掃描。）

## OpenClaw 端（遠端 gateway）

1) 把 `skill-ploy-rpc/` 複製到你的 OpenClaw workspace skills 目錄（依 OpenClaw 的安裝方式而定）。

2) 設定環境變數（OpenClaw skill 會用到）：

- `PLOY_TRADING_HOST`：例如 `ploy@1.2.3.4`
- `PLOY_TRADING_SSH_OPTS`：例如 `-i ~/.ssh/ploy -o StrictHostKeyChecking=yes`

3) 多事件來源（RSS/Atom、新聞、X 透過 RSS bridge）

把 `skill-ploy-rpc/config/feeds.example.json` 複製成 `skill-ploy-rpc/config/feeds.json`，填入你要監控的 feed URLs。

在 OpenClaw 裡可以用：

```bash
./bin/ingest_feeds ./config/feeds.json
```

它會輸出 JSON（新文章/新貼文），並用本地 state 檔去重複。

3) 你就可以在 OpenClaw 裡把這些當成 tools 來用（bash skill 形式）。
