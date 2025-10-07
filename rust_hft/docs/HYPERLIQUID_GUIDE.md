# Hyperliquid 集成使用指南

## 🎯 概述

本指南将帮助您完成 Hyperliquid 永续合约交易的完整配置和使用流程。

## 📋 前置要求

### 1. 创建 Hyperliquid 账户

1. **访问官网**: https://app.hyperliquid.xyz/
2. **连接钱包**: 使用 MetaMask 或其他 Web3 钱包
3. **完成验证**: 根据需要完成 KYC 验证
4. **充值资金**: 向您的地址转入 USDC 作为保证金

### 2. 准备私钥

⚠️ **重要安全提醒**: 私钥是您资金的唯一凭证，请妥善保管！

#### 从 MetaMask 导出私钥:
1. 打开 MetaMask 扩展
2. 点击右上角菜单 → 账户详情
3. 点击 "导出私钥"
4. 输入 MetaMask 密码
5. 复制 64 位十六进制字符串（不含 0x 前缀）

## 🚀 快速开始

### 1. Paper 模式测试（推荐首先使用）

Paper 模式使用真实市场数据但不执行真实交易，非常适合测试。

```bash
# 启动 Paper 模式
./scripts/start_hyperliquid.sh paper
```

### 2. Live 模式配置

#### Step 1: 配置私钥
编辑 `config/hyperliquid_live.yaml`：

```yaml
venues:
  - name: "HYPERLIQUID"
    execution_config:
      mode: "Live"
      # 将您的私钥替换此处
      private_key: "your_64_character_hex_private_key_here"
```

#### Step 2: 设置风控参数
```yaml
risk_management:
  global_limits:
    max_daily_loss_usd: 1000.0      # 每日最大亏损
    max_position_usd: 10000.0       # 最大持仓
    max_orders_per_second: 5        # 每秒最大订单数
    
  symbol_limits:
    "BTC-PERP":
      max_position_usd: 5000.0      # BTC 最大持仓
      max_order_size: 0.1           # BTC 单笔最大下单量
```

#### Step 3: 启动实盘交易
```bash
# 启动 Live 模式（会提示确认）
./scripts/start_hyperliquid.sh live
```

## ⚙️ 配置详解

### 1. 基础配置

```yaml
venues:
  - name: "HYPERLIQUID"
    venue_type: "perp"
    
    # 数据配置 - 公共 WebSocket
    data_config:
      adapter_type: "hyperliquid"
      ws_url: "wss://api.hyperliquid.xyz/ws"
      reconnect_interval_ms: 5000
      heartbeat_interval_ms: 30000
    
    # 执行配置 - 私有 API
    execution_config:
      adapter_type: "hyperliquid"
      mode: "Live"  # "Live" 或 "Paper"
      rest_base_url: "https://api.hyperliquid.xyz"
      ws_private_url: "wss://api.hyperliquid.xyz/ws"
      timeout_ms: 5000
      private_key: "your_private_key"
      vault_address: null  # 可选，vault 地址
```

### 2. 支持的交易对

| 交易对 | 最小下单量 | 价格精度 | 资产索引 |
|-------|-----------|----------|----------|
| BTC-PERP | 0.001 BTC | 0.1 USD | 0 |
| ETH-PERP | 0.01 ETH | 0.01 USD | 1 |
| SOL-PERP | 0.1 SOL | 0.001 USD | 2 |
| SUI-PERP | 1.0 SUI | 0.0001 USD | 3 |

### 3. 策略配置

```yaml
strategies:
  - name: "hyperliquid_trend_btc"
    type: "trend"
    symbols: ["BTC-PERP"]
    target_venue: "HYPERLIQUID"
    enabled: true
    params:
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14
      signal_threshold: 0.02
```

## 🔧 编程接口使用

### 1. 基础交易操作

```rust
use adapter_hyperliquid_execution::{
    HyperliquidExecutionClient, 
    HyperliquidExecutionConfig, 
    ExecutionMode
};
use hft_core::{Symbol, Side, Price, Quantity, OrderType, TimeInForce};
use ports::{ExecutionClient, OrderIntent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建配置
    let config = HyperliquidExecutionConfig {
        mode: ExecutionMode::Paper, // 或 ExecutionMode::Live
        private_key: "your_private_key".to_string(),
        ..Default::default()
    };
    
    // 创建客户端
    let mut client = HyperliquidExecutionClient::new(config);
    
    // 连接到 Hyperliquid
    client.connect().await?;
    
    // 下单
    let order_intent = OrderIntent {
        symbol: Symbol("BTC-PERP".to_string()),
        side: Side::Buy,
        quantity: Quantity::from_f64(0.001)?,
        price: Some(Price::from_f64(50000.0)?),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        strategy_id: "my_strategy".to_string(),
        target_venue: None,
    };
    
    let order_id = client.place_order(order_intent).await?;
    println!("下单成功: {:?}", order_id);
    
    // 查询未结订单
    let open_orders = client.list_open_orders().await?;
    println!("未结订单数量: {}", open_orders.len());
    
    // 撤单
    client.cancel_order(&order_id).await?;
    
    // 断开连接
    client.disconnect().await?;
    
    Ok(())
}
```

### 2. 实时事件流

```rust
use futures::StreamExt;

// 获取执行事件流
let mut event_stream = client.execution_stream().await?;

while let Some(event) = event_stream.next().await {
    match event? {
        ExecutionEvent::OrderAck { order_id, timestamp } => {
            println!("订单确认: {:?} at {}", order_id, timestamp);
        },
        ExecutionEvent::Fill { order_id, price, quantity, .. } => {
            println!("成交: {:?} - {} @ {}", order_id, quantity, price);
        },
        ExecutionEvent::OrderCanceled { order_id, .. } => {
            println!("订单取消: {:?}", order_id);
        },
        _ => {}
    }
}
```

## 🛡️ 安全最佳实践

### 1. 私钥管理
- ✅ 使用环境变量或加密配置文件存储私钥
- ✅ 定期轮换私钥
- ❌ 不要将私钥硬编码在源代码中
- ❌ 不要在日志中输出私钥

### 2. 风控设置
- ✅ 设置合理的止损限额
- ✅ 限制单笔订单大小
- ✅ 设置每日最大亏损
- ✅ 监控异常交易行为

### 3. 网络安全
- ✅ 使用 HTTPS/WSS 连接
- ✅ 验证 API 响应签名
- ✅ 实施重试和熔断机制

## 📊 监控和日志

### 1. 关键指标监控

```yaml
# 在配置中启用监控
logging:
  level: "info"
  file: "logs/hyperliquid.log"
  
# 监控指标
- 连接状态: client.health().await
- 延迟监控: latency_ms
- 订单成功率: execution_rate
- 资金使用率: position_utilization
```

### 2. 告警设置

- 连接断开超过 30 秒
- 订单延迟超过 1 秒
- 持仓超过风控限额
- 异常大额交易

## 🐛 故障排除

### 常见问题

1. **连接失败**
   ```
   错误: WebSocket 连接失败
   解决: 检查网络连接和防火墙设置
   ```

2. **认证错误**
   ```
   错误: Live 模式需要私钥
   解决: 确保配置文件中正确设置了 64 位私钥
   ```

3. **下单失败**
   ```
   错误: 不支持的交易对
   解决: 检查交易对名称是否正确（如 BTC-PERP）
   ```

4. **资金不足**
   ```
   错误: Insufficient margin
   解决: 向账户充值更多 USDC
   ```

### 调试模式

```bash
# 启用调试日志
RUST_LOG=debug ./scripts/start_hyperliquid.sh paper

# 查看详细日志
tail -f logs/hyperliquid_paper.log
```

## 🔄 升级和维护

### 1. 版本更新
```bash
# 更新代码
git pull origin main

# 重新编译
cargo build --release

# 运行测试
cargo test -p hft-execution-adapter-hyperliquid
```

### 2. 配置备份
```bash
# 备份配置文件
cp config/hyperliquid_live.yaml config/hyperliquid_live.yaml.bak

# 备份日志
tar -czf logs_backup_$(date +%Y%m%d).tar.gz logs/
```

## 📞 支持与反馈

如果您遇到问题或有改进建议，请：

1. 检查日志文件寻找错误信息
2. 参考本文档的故障排除部分
3. 在测试环境中复现问题
4. 联系技术支持并提供详细信息

## ⚖️ 免责声明

- 加密货币交易存在高风险，可能导致资金损失
- 请仅使用您能承受损失的资金进行交易
- 本软件仅为技术工具，不构成投资建议
- 使用前请充分测试并了解相关风险