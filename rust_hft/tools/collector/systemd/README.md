# Systemd 部署指南

此目录给出无 Docker 情况下在 Linux (systemd) 节点运行 `hft-collector` 的参考方案。

## 1. 安装步骤

1. **安装二进制**
   ```bash
   # 在构建主机
   cargo build -p hft-collector --release
   scp target/release/hft-collector user@ecs:/usr/local/bin/
   sudo chmod +x /usr/local/bin/hft-collector
   ```

2. **创建运行用户（推荐）**
   ```bash
   sudo useradd --system --home /var/lib/hft-collector --shell /usr/sbin/nologin hftcollector
   sudo mkdir -p /var/lib/hft-collector/logs
   sudo chown -R hftcollector:hftcollector /var/lib/hft-collector
   ```

3. **复制 systemd 模板**
   ```bash
   sudo mkdir -p /etc/systemd/system
   sudo cp apps/collector/systemd/hft-collector@.service /etc/systemd/system/
   ```

4. **准备环境变量文件**
   - 每个实例一个 env 文件，存放在 `/etc/hft-collector/<instance>.env`。
   - 可以以 `binance-spot-0.env.example` 为模版，复制到服务器并修改：
     ```bash
     sudo mkdir -p /etc/hft-collector
     sudo cp apps/collector/systemd/binance-spot-0.env.example /etc/hft-collector/binance-spot-0.env
     sudo vi /etc/hft-collector/binance-spot-0.env
     ```
   - 若有 ClickHouse 用户名/密码，放到 `/etc/hft-collector/credentials.env`：
     ```ini
     CLICKHOUSE_USER=xxx
     CLICKHOUSE_PASSWORD=yyy
     ```

5. **授权日志输出（可选）**
   - 默认写入 `journalctl`。如需落地文件，可在 unit 内追加：
     ```ini
     StandardOutput=append:/var/log/hft-collector/%i.log
     StandardError=append:/var/log/hft-collector/%i.err
     ```
     并保证目录可写。

## 2. 多实例/分片示例

假设白名单 22 个交易对，按 2 分片部署 Binance 现货：

```bash
sudo cp /etc/hft-collector/binance-spot-0.env /etc/hft-collector/binance-spot-1.env
# 在 binance-spot-1.env 中把 SHARD_INDEX 改为 1，其余保持一致。

sudo systemctl daemon-reload
sudo systemctl enable --now hft-collector@binance-spot-0.service
sudo systemctl enable --now hft-collector@binance-spot-1.service
```

同理，Binance 永续可以创建 `binance-perp-0.env`/`1.env`，其余交易所可单实例运行（`SHARD_COUNT=1`）。

## 3. 常用运维命令

```bash
# 查看状态
systemctl status hft-collector@binance-spot-0

# 实时日志
journalctl -u hft-collector@binance-spot-0 -f

# 平滑重启
systemctl restart hft-collector@binance-spot-0

# 停止
systemctl stop hft-collector@binance-spot-0
```

## 4. 资源与告警

- 建议在 env 文件中调整 `BATCH_SIZE`（1000~2000）与 `FLUSH_MS`（1000~2000）平衡 CPU/延迟。
- 监控 ClickHouse 表 `system.query_log` 中对应表的写入耗时，对应 Prometheus 指标 `BATCH_INSERT_SECONDS`。
- 若需 Prometheus 指标，可在 env 文件加入 `METRICS_ADDR=0.0.0.0:91xx` 并启用 `use-adapters` feature 后使用 `ms` 子命令。

## 5. 符号白名单

- 若白名单在单文件，可上传到 `/etc/hft-collector/symbols.json` 并在 env 文件设置 `SYMBOLS_FILE` 而不是 `SYMBOLS`。
- 直接使用 `SYMBOLS=` 的方案下，collector 会自动补齐 USDT/USDC 后缀并去重，适合 20~30 个符号的固定清单。

## 6. 版本更新流程

1. 替换 `/usr/local/bin/hft-collector`。
2. `systemctl restart hft-collector@*.service`。
3. 通过 `journalctl` 和 ClickHouse 验证是否有新批量写入。

## 7. 故障排查

- **反复重启**：检查 ClickHouse URL/证书、白名单拼写是否正确。
- **CPU 占用高**：降低 `BATCH_SIZE`、增加 `SHARD_COUNT` 或拆分交易所到不同主机。
- **没有写入**：确认 `dry_run=false`，并查看 `journalctl` 是否有 `flush_error` 日志。

如需进一步自动化，可配合 `ansible` 或 `systemd` generator 批量生成 env 文件和 unit。
