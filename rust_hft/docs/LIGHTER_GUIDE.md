# Lighter 集成使用指南（行情）

本指南介绍如何在本仓库启用 Lighter DEX 公有行情订阅（quotes‑only）。执行端（下单/撤单）需接入官方 Signer 库，后续提供。

## 功能概览
- 公有 WebSocket: `wss://<host>/stream`
- 订阅通道: `order_book/{market_id}`（需先通过 REST 映射 `symbol -> market_id`）
- REST 元数据: `GET /api/v1/orderBooks`

## 启用方式
- Feature: 在 `hft-runtime` 启用 `lighter`（包含 `adapter-lighter-data` 与 `adapter-lighter-execution`）。

示例：
```bash
cargo run -p hft-paper --features "lighter"
```

## 环境变量
- `LIGHTER_REST`（默认 `https://mainnet.zklighter.elliot.ai`）
- `LIGHTER_WS_PUBLIC`（默认 `wss://mainnet.zklighter.elliot.ai/stream`）

执行（可选，Live 模式）
- `LIGHTER_SIGNER_LIB_PATH`: 指向官方 signer 库（`signer-amd64.so` 或 `signer-arm64.dylib`）
- `LIGHTER_API_KEY_PRIVATE_KEY`: Lighter API Key 私钥（hex）
- `LIGHTER_API_KEY_INDEX`: API Key Index（整型）
- `LIGHTER_ACCOUNT_INDEX`: Account Index（整型）

## 配置示例（SystemConfig 片段）
```yaml
venues:
  - name: Lighter DEX
    venue_type: Lighter
    # 可选自定义端点（不填使用默认）
    rest: "https://mainnet.zklighter.elliot.ai"
    ws_public: "wss://mainnet.zklighter.elliot.ai/stream"
    # 仅行情
    simulate_execution: true
    symbol_catalog:
      - "BTCUSDT"    # 对应 Lighter `orderBooks` 中的 symbol
      - "ETHUSDT"
```

## 注意事项
- Lighter 的 WS `update/order_book` 为价格层增量，适配器会直接下发 `MarketEvent::Update`，引擎侧可按需聚合。
- 若符号未在 `GET /api/v1/orderBooks` 返回中匹配到，将跳过该符号的订阅并打印告警。
- 执行端（下单）需要官方 Signer 库生成交易签名（本仓库通过 FFI 动态加载）。撤单/改单/查单涉及 `order_index` 与授权签名，后续逐步补齐。
