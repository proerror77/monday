# 增強版交易所管理器 (Enhanced Exchange Manager)

本文檔介紹增強版 ExchangeManager 的新功能和使用方法。

## 🚀 主要增強功能

### 1. 智能健康度評估系統

- **延遲 EWMA (指數加權移動平均)**：實時追蹤延遲趨勢
- **錯誤率監控**：追蹤請求失敗率
- **心跳監控**：檢測連接活躍度
- **綜合可用性分數**：基於多個指標的整體健康評分

```rust
// 健康度指標自動更新
manager.record_latency("bitget", "main_account", 25.5).await;
manager.record_error("bitget", "main_account", "timeout").await;
```

### 2. 智能路由算法

支援多種路由策略：

- **HealthBased**：基於健康度分數選擇
- **LatencyBased**：基於延遲選擇最快實例
- **RoundRobin**：輪詢分配
- **Priority**：基於優先級選擇
- **Composite**：綜合評分策略（默認）

```rust
let manager = ExchangeManager::with_config(
    RoutingStrategy::Composite,
    FailoverConfig::default(),
);

// 智能選擇最佳實例
let exchange = manager.select_best_exchange("bitget").await?;
```

### 3. 多賬戶實例管理

支援同一交易所的多個賬戶實例：

```rust
// 添加主賬戶
let main_config = ExchangeInstanceConfig {
    name: "bitget".to_string(),
    account_id: "main_account".to_string(),
    api_key: "main_key".to_string(),
    secret_key: "main_secret".to_string(),
    enabled: true,
    priority: 1, // 最高優先級
    ..Default::default()
};

// 添加備用賬戶
let backup_config = ExchangeInstanceConfig {
    name: "bitget".to_string(),
    account_id: "backup_account".to_string(),
    api_key: "backup_key".to_string(),
    secret_key: "backup_secret".to_string(),
    enabled: true,
    priority: 2,
    ..Default::default()
};

manager.add_exchange_instance(main_config, Box::new(BitgetExchange::new())).await?;
manager.add_exchange_instance(backup_config, Box::new(BitgetExchange::new())).await?;
```

### 4. 高級故障轉移機制

- **自動故障檢測**：基於健康度閾值
- **漸進式重連**：指數退避重連策略
- **實例降級**：自動禁用故障實例
- **故障恢復**：自動重新啟用恢復實例

```rust
// 配置故障轉移參數
let failover_config = FailoverConfig {
    health_threshold: 0.7,        // 健康度閾值
    latency_threshold: 100.0,     // 延遲閾值 (ms)
    error_rate_threshold: 0.05,   // 錯誤率閾值 (5%)
    heartbeat_timeout: 60,        // 心跳超時 (秒)
    reconnect_interval: 30,       // 重連間隔 (秒)
    max_reconnect_attempts: 5,    // 最大重連次數
};
```

### 5. 完善的 Prometheus 指標

暴露詳細的運營指標：

```rust
// 健康度指標
exchange_health_score{exchange="bitget", account="main"} 0.85
exchange_latency_ewma_ms{exchange="bitget", account="main"} 25.3
exchange_error_rate{exchange="bitget", account="main"} 0.02

// 選擇統計
exchange_selection_total{exchange="bitget", account="main"} 1247

// 故障轉移統計
exchange_failover_triggered_total{exchange="bitget", account="main", reason="health_score"} 3
exchange_reconnect_success_total{exchange="bitget", account="main"} 2
exchange_reconnect_failure_total{exchange="bitget", account="main", error="timeout"} 1

// 請求統計
exchange_request_latency_ms{exchange="bitget", account="main"} 25.5
exchange_errors_total{exchange="bitget", account="main", error_type="timeout"} 5
```

## 📋 使用示例

### 基本設置

```rust
use rust_hft::exchanges::{ExchangeManager, RoutingStrategy, FailoverConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 創建管理器
    let manager = ExchangeManager::with_config(
        RoutingStrategy::Composite,
        FailoverConfig::default(),
    );
    
    // 啟動監控
    manager.start_monitoring().await?;
    
    // 使用管理器...
    
    // 停止監控
    manager.stop_monitoring().await;
    Ok(())
}
```

### 健康狀態監控

```rust
// 訂閱健康狀態變化
let mut health_receiver = manager.subscribe_health_changes();

tokio::spawn(async move {
    while let Ok((exchange, account, metrics)) = health_receiver.recv().await {
        println!("健康更新: {}:{} - 分數: {:.3}", 
                 exchange, account, metrics.availability_score);
        
        if metrics.availability_score < 0.5 {
            println!("⚠️ 健康度過低，可能需要故障轉移");
        }
    }
});
```

### 手動故障管理

```rust
// 手動禁用實例
manager.set_instance_enabled("bitget", "problematic_account", false).await?;

// 手動觸發重連
manager.reconnect_instance("bitget", "main_account").await?;

// 檢查重連狀態
let status = manager.get_reconnect_status().await;
for (instance, (attempts, is_reconnecting)) in status {
    println!("實例 {}: 重連次數 {}, 正在重連: {}", 
             instance, attempts, is_reconnecting);
}

// 重置重連計數器
manager.reset_reconnect_attempts("bitget", "main_account").await;
```

## 🎯 最佳實踐

### 1. 健康度閾值設置

```rust
let failover_config = FailoverConfig {
    health_threshold: 0.7,      // 生產環境建議 0.7-0.8
    latency_threshold: 50.0,    // 根據業務需求調整
    error_rate_threshold: 0.03, // 3% 錯誤率閾值
    heartbeat_timeout: 30,      // 心跳超時
    reconnect_interval: 15,     // 重連間隔
    max_reconnect_attempts: 3,  // 避免無限重連
};
```

### 2. 優先級配置

- **主賬戶**：priority = 1 (最高)
- **備用賬戶**：priority = 2-5
- **測試賬戶**：priority = 100+ (最低)

### 3. 監控和告警

```rust
// 設置 Prometheus 告警規則
// exchange_health_score < 0.7
// exchange_latency_ewma_ms > 100
// exchange_error_rate > 0.05
```

## 🔧 配置參考

### RoutingStrategy 選擇指南

- **Composite**：推薦用於生產環境，綜合考慮健康度和優先級
- **HealthBased**：當需要最大化可用性時使用
- **LatencyBased**：當延遲是關鍵因素時使用
- **Priority**：當需要嚴格按優先級路由時使用
- **RoundRobin**：當需要均勻分配負載時使用

### FailoverConfig 參數說明

| 參數 | 建議值 | 說明 |
|------|--------|------|
| health_threshold | 0.7 | 低於此值觸發故障轉移 |
| latency_threshold | 50-100ms | 延遲閾值 |
| error_rate_threshold | 0.03-0.05 | 3-5% 錯誤率 |
| heartbeat_timeout | 30-60s | 心跳超時 |
| reconnect_interval | 15-30s | 重連間隔 |
| max_reconnect_attempts | 3-5 | 最大重連次數 |

## 🚨 故障排除

### 常見問題

1. **實例一直重連失敗**
   ```rust
   // 檢查重連狀態
   let status = manager.get_reconnect_status().await;
   
   // 重置重連計數器
   manager.reset_reconnect_attempts("exchange", "account").await;
   ```

2. **健康度分數異常**
   ```rust
   // 手動記錄正常延遲來校正 EWMA
   manager.record_latency("exchange", "account", 25.0).await;
   ```

3. **路由不符合預期**
   ```rust
   // 檢查實例配置
   let instances = manager.list_exchange_instances().await;
   for (exchange, instance_list) in instances {
       for (account, enabled, health) in instance_list {
           println!("{}:{} - 啟用: {}, 健康度: {:.3}", 
                    exchange, account, enabled, health);
       }
   }
   ```

## 🔄 升級指南

從舊版本升級：

```rust
// 舊版本
let manager = ExchangeManager::new();
manager.add_exchange("bitget".to_string(), Box::new(BitgetExchange::new())).await;

// 新版本（向後兼容）
let manager = ExchangeManager::new();
manager.add_exchange("bitget".to_string(), Box::new(BitgetExchange::new())).await;

// 或使用新功能
let config = ExchangeInstanceConfig {
    name: "bitget".to_string(),
    account_id: "main".to_string(),
    // ... 其他配置
    ..Default::default()
};
manager.add_exchange_instance(config, Box::new(BitgetExchange::new())).await?;
```