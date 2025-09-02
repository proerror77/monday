/*!
 * 增強版風險管理系統演示
 * 
 * 展示完整的風險管理功能：
 * - 📊 實時持倉跟蹤和VaR計算
 * - 🎯 動態止損止盈機制
 * - 🚨 智能風險告警系統
 * - 📈 實時風險指標監控
 * - 💹 波動率調整和組合優化
 */

use rust_hft::core::types::*;
use rust_hft::exchanges::message_types::{ExecutionReport, OrderRequest};
use rust_hft::engine::{
    EnhancedRiskManager, EnhancedRiskConfig as RiskConfig, StopOrderType, 
    AlertSeverity, PositionRiskLevel, EnhancedRiskReport as RiskReport,
    RiskViolation, RiskRecommendation, RecommendationPriority,
};
use rust_hft::engine::enhanced_risk_manager::RiskEvent;
use std::collections::HashMap;
use tokio::time::{sleep, Duration, Instant};
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌系統
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🚀 啟動增強版風險管理系統演示");

    // 創建風險管理配置
    let mut config = RiskConfig::default();
    config.max_portfolio_value = 1000000.0;  // $1M 投資組合
    config.max_daily_loss = 10000.0;         // $10K 日損失限制
    config.default_stop_loss_pct = 2.0;      // 2% 止損
    config.default_take_profit_pct = 6.0;    // 6% 止盈
    config.trailing_stop_enabled = true;
    config.enable_dynamic_sizing = true;
    config.risk_check_interval_ms = 100;     // 100ms 風險檢查

    // 創建增強版風險管理器
    let (mut risk_manager, mut risk_events) = EnhancedRiskManager::new(config).await?;
    
    info!("✅ 風險管理器創建成功");

    // 啟動實時監控
    risk_manager.start_monitoring().await?;
    
    // 啟動風險事件監聽器
    let risk_manager_clone = std::sync::Arc::new(risk_manager);
    let risk_events_task = {
        let risk_manager = risk_manager_clone.clone();
        tokio::spawn(async move {
            while let Some(event) = risk_events.recv().await {
                handle_risk_event(event, &risk_manager).await;
            }
        })
    };

    info!("📊 開始風險管理演示...");

    // 演示1: 基本持倉管理和VaR計算
    demo_basic_position_management(&risk_manager_clone).await?;
    
    // 演示2: 動態止損止盈機制
    demo_dynamic_stop_loss(&risk_manager_clone).await?;
    
    // 演示3: 實時風險監控和告警
    demo_real_time_monitoring(&risk_manager_clone).await?;
    
    // 演示4: 風險暴露和組合優化
    demo_portfolio_optimization(&risk_manager_clone).await?;
    
    // 演示5: 壓力測試場景
    demo_stress_testing(&risk_manager_clone).await?;

    // 生成最終風險報告
    generate_final_risk_report(&risk_manager_clone).await?;

    // 清理任務
    risk_events_task.abort();
    
    info!("✅ 增強版風險管理系統演示完成！");
    Ok(())
}

/// 演示1: 基本持倉管理和VaR計算
async fn demo_basic_position_management(
    risk_manager: &std::sync::Arc<EnhancedRiskManager>
) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n📊 === 演示1: 基本持倉管理和VaR計算 ===");
    
    // 模擬交易執行報告
    let execution_reports = vec![
        ExecutionReport {
            order_id: "order_1".to_string(),
            client_order_id: Some("client_1".to_string()),
            exchange: "bitget".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            status: OrderStatus::Filled,
            original_quantity: 0.5,
            executed_quantity: 0.5,
            remaining_quantity: 0.0,
            price: 50000.0,
            avg_price: 50000.0,
            last_executed_price: 50000.0,
            last_executed_quantity: 0.5,
            commission: 25.0,
            commission_asset: "USDT".to_string(),
            create_time: chrono::Utc::now().timestamp_millis() as u64,
            update_time: chrono::Utc::now().timestamp_millis() as u64,
            transaction_time: chrono::Utc::now().timestamp_millis() as u64,
            reject_reason: None,
        },
        ExecutionReport {
            order_id: "order_2".to_string(),
            client_order_id: Some("client_2".to_string()),
            exchange: "bitget".to_string(),
            symbol: "ETHUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            status: OrderStatus::Filled,
            original_quantity: 10.0,
            executed_quantity: 10.0,
            remaining_quantity: 0.0,
            price: 3000.0,
            avg_price: 3000.0,
            last_executed_price: 3000.0,
            last_executed_quantity: 10.0,
            commission: 15.0,
            commission_asset: "USDT".to_string(),
            create_time: chrono::Utc::now().timestamp_millis() as u64,
            update_time: chrono::Utc::now().timestamp_millis() as u64,
            transaction_time: chrono::Utc::now().timestamp_millis() as u64,
            reject_reason: None,
        },
        ExecutionReport {
            order_id: "order_3".to_string(),
            client_order_id: Some("client_3".to_string()),
            exchange: "bitget".to_string(),
            symbol: "ADAUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            status: OrderStatus::Filled,
            original_quantity: 10000.0,
            executed_quantity: 10000.0,
            remaining_quantity: 0.0,
            price: 0.5,
            avg_price: 0.5,
            last_executed_price: 0.5,
            last_executed_quantity: 10000.0,
            commission: 2.5,
            commission_asset: "USDT".to_string(),
            create_time: chrono::Utc::now().timestamp_millis() as u64,
            update_time: chrono::Utc::now().timestamp_millis() as u64,
            transaction_time: chrono::Utc::now().timestamp_millis() as u64,
            reject_reason: None,
        },
    ];

    info!("🔄 模擬交易執行...");
    
    // 更新持倉
    for execution in &execution_reports {
        risk_manager.update_position(execution).await?;
        info!("📈 持倉更新: {} 數量: {} 價格: ${:.2}", 
              execution.symbol, execution.executed_quantity, execution.avg_price);
        
        // 模擬價格變化
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 模擬價格波動
    info!("📊 模擬市場價格波動...");
    let price_updates = vec![
        ("BTCUSDT", 51000.0),
        ("ETHUSDT", 3100.0),
        ("ADAUSDT", 0.52),
        ("BTCUSDT", 49500.0),
        ("ETHUSDT", 2950.0),
        ("ADAUSDT", 0.48),
        ("BTCUSDT", 52000.0),
        ("ETHUSDT", 3200.0),
        ("ADAUSDT", 0.55),
    ];

    for (symbol, price) in price_updates {
        risk_manager.update_market_price(symbol, price).await?;
        info!("💹 價格更新: {} -> ${:.2}", symbol, price);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 計算VaR
    info!("📊 計算 Value at Risk (VaR)...");
    let var_95 = risk_manager.calculate_var(0.95).await?;
    let var_99 = risk_manager.calculate_var(0.99).await?;
    
    info!("💰 VaR 計算結果:");
    info!("   95% VaR: ${:.2}", var_95);
    info!("   99% VaR: ${:.2}", var_99);

    // 生成持倉報告
    let risk_report = risk_manager.get_risk_report().await;
    info!("📋 持倉風險報告:");
    info!("   持倉數量: {}", risk_report.position_count);
    info!("   總投資組合價值: ${:.2}", risk_report.metrics.total_portfolio_value);
    info!("   未實現PnL: ${:.2}", risk_report.metrics.total_unrealized_pnl);
    info!("   已實現PnL: ${:.2}", risk_report.metrics.total_realized_pnl);
    info!("   淨PnL: ${:.2}", risk_report.metrics.net_pnl);
    info!("   集中度風險: {:.1}%", risk_report.metrics.concentration_risk);

    Ok(())
}

/// 演示2: 動態止損止盈機制
async fn demo_dynamic_stop_loss(
    risk_manager: &std::sync::Arc<EnhancedRiskManager>
) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🎯 === 演示2: 動態止損止盈機制 ===");
    
    // 設置動態止損
    info!("🎯 設置動態止損機制...");
    
    risk_manager.set_dynamic_stop_loss("BTCUSDT", StopOrderType::StopLoss).await?;
    risk_manager.set_dynamic_stop_loss("BTCUSDT", StopOrderType::TakeProfit).await?;
    risk_manager.set_dynamic_stop_loss("ETHUSDT", StopOrderType::TrailingStop).await?;
    
    info!("✅ 動態止損設置完成");

    // 模擬極端價格波動以觸發止損
    info!("⚡ 模擬極端市場波動...");
    let extreme_scenarios = vec![
        ("BTCUSDT", 48000.0, "下跌觸發止損"),
        ("BTCUSDT", 46000.0, "進一步下跌"),
        ("ETHUSDT", 2800.0, "ETH 大跌"),
        ("BTCUSDT", 53000.0, "反彈觸發止盈"),
        ("ADAUSDT", 0.35, "ADAUSDT 暴跌"),
    ];

    for (symbol, price, description) in extreme_scenarios {
        info!("📉 {}: {} -> ${:.2}", description, symbol, price);
        risk_manager.update_market_price(symbol, price).await?;
        
        // 檢查風險報告
        let risk_report = risk_manager.get_risk_report().await;
        if !risk_report.risk_violations.is_empty() {
            warn!("⚠️ 發現風險違規:");
            for violation in &risk_report.risk_violations {
                warn!("   - {}: {:.2} > {:.2}", 
                      violation.description, violation.current_value, violation.limit_value);
            }
        }
        
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // 檢查止損觸發情況
    let risk_report = risk_manager.get_risk_report().await;
    info!("📊 止損機制效果:");
    info!("   當前風險等級: {:?}", 
          if risk_report.risk_violations.is_empty() { "正常" } else { "警告" });
    info!("   當前回撤: {:.2}%", risk_report.metrics.current_drawdown);
    info!("   最大回撤: {:.2}%", risk_report.metrics.maximum_drawdown);

    Ok(())
}

/// 演示3: 實時風險監控和告警
async fn demo_real_time_monitoring(
    risk_manager: &std::sync::Arc<EnhancedRiskManager>
) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🚨 === 演示3: 實時風險監控和告警 ===");
    
    info!("📊 開始實時風險監控...");
    
    // 模擬持續的市場數據流
    let monitoring_duration = Duration::from_secs(10);
    let start_time = Instant::now();
    let mut price_tick = 0;
    
    while start_time.elapsed() < monitoring_duration {
        price_tick += 1;
        
        // 生成模擬價格數據
        let btc_price = 50000.0 + (price_tick as f64 * 0.1).sin() * 2000.0;
        let eth_price = 3000.0 + (price_tick as f64 * 0.15).sin() * 300.0;
        let ada_price = 0.5 + (price_tick as f64 * 0.2).sin() * 0.05;
        
        // 更新價格
        risk_manager.update_market_price("BTCUSDT", btc_price).await?;
        risk_manager.update_market_price("ETHUSDT", eth_price).await?;
        risk_manager.update_market_price("ADAUSDT", ada_price).await?;
        
        // 每秒生成風險報告
        if price_tick % 10 == 0 {
            let risk_report = risk_manager.get_risk_report().await;
            info!("📊 實時風險狀態 (T+{}s):", price_tick / 10);
            info!("   投資組合價值: ${:.2}", risk_report.metrics.total_portfolio_value);
            info!("   未實現PnL: ${:.2}", risk_report.metrics.total_unrealized_pnl);
            info!("   VaR 95%: ${:.2}", risk_report.metrics.portfolio_var_95);
            info!("   計算延遲: {}μs", risk_report.metrics.calculation_latency_us);
            
            // 檢查告警
            if !risk_report.risk_violations.is_empty() {
                warn!("🚨 風險告警:");
                for violation in &risk_report.risk_violations {
                    warn!("   {} - {}", violation.description, 
                          match violation.severity {
                              AlertSeverity::Critical => "🔴 危險",
                              AlertSeverity::Warning => "🟡 警告",
                              AlertSeverity::Info => "🔵 信息",
                              AlertSeverity::Emergency => "🚨 緊急",
                          });
                }
            }
            
            // 顯示建議
            if !risk_report.recommendations.is_empty() {
                info!("💡 風險管理建議:");
                for rec in risk_report.recommendations.iter().take(2) {
                    info!("   {} - {}", rec.description, rec.suggested_action);
                }
            }
        }
        
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    
    info!("✅ 實時風險監控演示完成");
    Ok(())
}

/// 演示4: 風險暴露和組合優化
async fn demo_portfolio_optimization(
    risk_manager: &std::sync::Arc<EnhancedRiskManager>
) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n💹 === 演示4: 風險暴露和組合優化 ===");
    
    // 模擬添加更多持倉以測試組合風險
    info!("📈 添加更多持倉以測試組合風險管理...");
    
    let additional_trades = vec![
        ("SOLUSDT", OrderSide::Buy, 50.0, 100.0),
        ("DOTUSDT", OrderSide::Buy, 100.0, 25.0),
        ("MATICUSDT", OrderSide::Buy, 1000.0, 1.2),
        ("LINKUSDT", OrderSide::Buy, 50.0, 20.0),
        ("AVAXUSDT", OrderSide::Buy, 25.0, 40.0),
    ];
    
    for (symbol, side, quantity, price) in additional_trades {
        let execution = ExecutionReport {
            order_id: format!("opt_{}", symbol),
            client_order_id: Some(format!("client_opt_{}", symbol)),
            exchange: "bitget".to_string(),
            symbol: symbol.to_string(),
            side,
            order_type: OrderType::Market,
            status: OrderStatus::Filled,
            original_quantity: quantity,
            executed_quantity: quantity,
            remaining_quantity: 0.0,
            price,
            avg_price: price,
            last_executed_price: price,
            last_executed_quantity: quantity,
            commission: quantity * price * 0.001,
            commission_asset: "USDT".to_string(),
            create_time: chrono::Utc::now().timestamp_millis() as u64,
            update_time: chrono::Utc::now().timestamp_millis() as u64,
            transaction_time: chrono::Utc::now().timestamp_millis() as u64,
            reject_reason: None,
        };
        
        risk_manager.update_position(&execution).await?;
        info!("💰 新增持倉: {} 數量: {} 價格: ${:.2}", symbol, quantity, price);
    }
    
    // 分析組合風險特徵
    let risk_report = risk_manager.get_risk_report().await;
    info!("\n📊 組合風險分析:");
    info!("   持倉總數: {}", risk_report.position_count);
    info!("   總投資組合價值: ${:.2}", risk_report.metrics.total_portfolio_value);
    info!("   總風險暴露: ${:.2}", risk_report.metrics.total_exposure);
    info!("   集中度風險: {:.1}%", risk_report.metrics.concentration_risk);
    info!("   槓桿比率: {:.2}x", risk_report.metrics.leverage_ratio);
    info!("   相關性風險: {:.1}%", risk_report.metrics.correlation_risk);
    
    // 測試不同市場情境
    info!("\n📊 測試市場情境分析...");
    
    let market_scenarios = vec![
        ("牛市情境", vec![1.15, 1.20, 1.12, 1.18, 1.25, 1.10, 1.08, 1.14]),
        ("熊市情境", vec![0.85, 0.75, 0.82, 0.78, 0.70, 0.88, 0.92, 0.80]),
        ("震盪情境", vec![1.05, 0.95, 1.08, 0.92, 1.03, 0.97, 1.06, 0.94]),
    ];
    
    for (scenario_name, multipliers) in market_scenarios {
        info!("🎭 測試{}", scenario_name);
        
        let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "SOLUSDT", "DOTUSDT", "MATICUSDT", "LINKUSDT", "AVAXUSDT"];
        let base_prices = [50000.0, 3000.0, 0.5, 100.0, 25.0, 1.2, 20.0, 40.0];
        
        // 應用情境價格
        for (i, (symbol, multiplier)) in symbols.iter().zip(multipliers.iter()).enumerate() {
            let new_price = base_prices[i] * multiplier;
            risk_manager.update_market_price(symbol, new_price).await?;
        }
        
        // 計算情境下的風險指標
        let var_95 = risk_manager.calculate_var(0.95).await?;
        let risk_report = risk_manager.get_risk_report().await;
        
        info!("   情境結果: PnL: ${:.2}, VaR 95%: ${:.2}, 回撤: {:.2}%",
              risk_report.metrics.net_pnl, var_95, risk_report.metrics.current_drawdown);
    }
    
    Ok(())
}

/// 演示5: 壓力測試場景
async fn demo_stress_testing(
    risk_manager: &std::sync::Arc<EnhancedRiskManager>
) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🧪 === 演示5: 壓力測試場景 ===");
    
    info!("⚡ 執行極端市場壓力測試...");
    
    // 場景1: 加密貨幣崩盤（類似2022年5月）
    info!("\n💥 場景1: 加密貨幣市場崩盤");
    let crash_multipliers = [0.5, 0.4, 0.3, 0.45, 0.35, 0.25, 0.4, 0.3];
    let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "SOLUSDT", "DOTUSDT", "MATICUSDT", "LINKUSDT", "AVAXUSDT"];
    let base_prices = [50000.0, 3000.0, 0.5, 100.0, 25.0, 1.2, 20.0, 40.0];
    
    for (i, (symbol, multiplier)) in symbols.iter().zip(crash_multipliers.iter()).enumerate() {
        let crash_price = base_prices[i] * multiplier;
        risk_manager.update_market_price(symbol, crash_price).await?;
        info!("📉 {} 崩跌至 ${:.2} (-{:.1}%)", 
              symbol, crash_price, (1.0 - multiplier) * 100.0);
    }
    
    let crash_report = risk_manager.get_risk_report().await;
    info!("🔴 崩盤場景結果:");
    info!("   總損失: ${:.2}", crash_report.metrics.net_pnl);
    info!("   回撤: {:.2}%", crash_report.metrics.current_drawdown);
    info!("   VaR 99%: ${:.2}", crash_report.metrics.portfolio_var_99);
    info!("   風險違規數: {}", crash_report.risk_violations.len());
    
    // 場景2: 流動性危機
    info!("\n💧 場景2: 流動性危機模擬");
    info!("⚠️ 模擬流動性枯竭和滑點擴大...");
    
    // 在崩盤基礎上進一步測試
    for symbol in &symbols[..4] { // 只測試主要幣種
        let current_price = base_prices[symbols.iter().position(|&s| s == *symbol).unwrap()] * 0.3;
        risk_manager.update_market_price(symbol, current_price * 0.9).await?; // 額外10%滑點
    }
    
    let liquidity_report = risk_manager.get_risk_report().await;
    info!("🟡 流動性危機結果:");
    info!("   流動性評分: {:.2}", liquidity_report.metrics.liquidity_score);
    info!("   市場衝擊風險: {:.2}%", liquidity_report.metrics.market_impact_risk);
    
    // 場景3: 恢復測試
    info!("\n🔄 場景3: 市場恢復測試");
    let recovery_multipliers = [0.7, 0.65, 0.6, 0.68, 0.62, 0.55, 0.65, 0.6];
    
    for (i, (symbol, multiplier)) in symbols.iter().zip(recovery_multipliers.iter()).enumerate() {
        let recovery_price = base_prices[i] * multiplier;
        risk_manager.update_market_price(symbol, recovery_price).await?;
    }
    
    let recovery_report = risk_manager.get_risk_report().await;
    info!("🟢 恢復場景結果:");
    info!("   恢復後PnL: ${:.2}", recovery_report.metrics.net_pnl);
    info!("   剩餘回撤: {:.2}%", recovery_report.metrics.current_drawdown);
    
    // 壓力測試總結
    info!("\n📋 壓力測試總結:");
    if !recovery_report.risk_violations.is_empty() {
        warn!("⚠️ 仍存在風險違規:");
        for violation in &recovery_report.risk_violations {
            warn!("   - {}", violation.description);
        }
    } else {
        info!("✅ 所有風險指標已恢復正常範圍");
    }
    
    if !recovery_report.recommendations.is_empty() {
        info!("💡 後續建議:");
        for rec in &recovery_report.recommendations {
            info!("   - {}: {}", rec.description, rec.suggested_action);
        }
    }
    
    Ok(())
}

/// 處理風險事件
async fn handle_risk_event(
    event: RiskEvent,
    risk_manager: &std::sync::Arc<EnhancedRiskManager>,
) {
    match event {
        RiskEvent::PositionRiskLevelChanged { symbol, old_level, new_level, reason } => {
            let severity = match new_level {
                PositionRiskLevel::Emergency => "🚨",
                PositionRiskLevel::Critical => "🔴",
                PositionRiskLevel::High => "🟠",
                PositionRiskLevel::Medium => "🟡",
                PositionRiskLevel::Low => "🟢",
            };
            
            info!("{} 持倉風險等級變化: {} {:?} -> {:?} (原因: {})", 
                  severity, symbol, old_level, new_level, reason);
        },
        
        RiskEvent::VarLimitTriggered { current_var, limit, confidence_level } => {
            warn!("🔴 VaR 限制觸發: {:.1}% 置信度 VaR ${:.2} 超過限制 ${:.2}", 
                  confidence_level * 100.0, current_var, limit);
        },
        
        RiskEvent::DrawdownAlert { current_drawdown, max_allowed, severity } => {
            let emoji = match severity {
                AlertSeverity::Emergency => "🚨",
                AlertSeverity::Critical => "🔴",
                AlertSeverity::Warning => "🟡",
                AlertSeverity::Info => "🔵",
            };
            warn!("{} 回撤告警: 當前 {:.2}% 超過允許 {:.2}%", 
                  emoji, current_drawdown, max_allowed);
        },
        
        RiskEvent::StopLossTriggered { symbol, trigger_price, stop_type, pnl } => {
            let result = if pnl >= 0.0 { "盈利" } else { "虧損" };
            info!("🎯 止損觸發: {} {:?} 於 ${:.2} ({}${:.2})", 
                  symbol, stop_type, trigger_price, result, pnl.abs());
        },
        
        RiskEvent::ConcentrationAlert { symbol, concentration_pct, threshold } => {
            warn!("⚠️ 持倉集中度警告: {} 佔比 {:.1}% 超過閾值 {:.1}%", 
                  symbol, concentration_pct, threshold);
        },
        
        RiskEvent::LiquidityAlert { symbol, liquidity_score, threshold } => {
            warn!("💧 流動性警告: {} 流動性評分 {:.2} 低於閾值 {:.2}", 
                  symbol, liquidity_score, threshold);
        },
        
        RiskEvent::MarketStressAlert { stress_level, regime, recommended_action } => {
            warn!("🌪️ 市場壓力警告: 壓力水平 {:.2} 波動率環境 {:?}", 
                  stress_level, regime);
            info!("   建議行動: {}", recommended_action);
        },
    }
}

/// 生成最終風險報告
async fn generate_final_risk_report(
    risk_manager: &std::sync::Arc<EnhancedRiskManager>
) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n📋 === 最終風險報告 ===");
    
    let final_report = risk_manager.get_risk_report().await;
    
    info!("🏛️ 投資組合概覽:");
    info!("   管理器ID: {}", final_report.manager_id);
    info!("   持倉數量: {}", final_report.position_count);
    info!("   總投資組合價值: ${:.2}", final_report.metrics.total_portfolio_value);
    info!("   總風險暴露: ${:.2}", final_report.total_exposure);
    
    info!("\n💰 損益統計:");
    info!("   未實現PnL: ${:.2}", final_report.metrics.total_unrealized_pnl);
    info!("   已實現PnL: ${:.2}", final_report.metrics.total_realized_pnl);
    info!("   淨PnL: ${:.2}", final_report.metrics.net_pnl);
    
    info!("\n📊 風險指標:");
    info!("   VaR 95%: ${:.2}", final_report.metrics.portfolio_var_95);
    info!("   VaR 99%: ${:.2}", final_report.metrics.portfolio_var_99);
    info!("   預期損失: ${:.2}", final_report.metrics.expected_shortfall);
    info!("   最大回撤: {:.2}%", final_report.metrics.maximum_drawdown);
    info!("   當前回撤: {:.2}%", final_report.metrics.current_drawdown);
    
    info!("\n📈 績效指標:");
    info!("   夏普比率: {:.2}", final_report.metrics.sharpe_ratio);
    info!("   索丁諾比率: {:.2}", final_report.metrics.sortino_ratio);
    info!("   卡爾馬比率: {:.2}", final_report.metrics.calmar_ratio);
    info!("   勝率: {:.1}%", final_report.metrics.win_rate * 100.0);
    
    info!("\n🎯 風險暴露分析:");
    info!("   槓桿比率: {:.2}x", final_report.metrics.leverage_ratio);
    info!("   集中度風險: {:.1}%", final_report.metrics.concentration_risk);
    info!("   相關性風險: {:.1}%", final_report.metrics.correlation_risk);
    info!("   流動性評分: {:.2}", final_report.metrics.liquidity_score);
    info!("   市場衝擊風險: {:.2}%", final_report.metrics.market_impact_risk);
    
    info!("\n🌡️ 市場環境:");
    info!("   波動率環境: {:?}", final_report.metrics.volatility_regime);
    info!("   市場壓力水平: {:.2}", final_report.metrics.market_stress_level);
    
    info!("\n⚡ 系統性能:");
    info!("   計算延遲: {}μs", final_report.metrics.calculation_latency_us);
    info!("   最後計算時間: {}", 
          chrono::DateTime::from_timestamp_micros(final_report.metrics.last_calculation as i64)
              .unwrap_or_default());
    
    // 違規警告
    if !final_report.risk_violations.is_empty() {
        warn!("\n⚠️ 風險違規警告:");
        for violation in &final_report.risk_violations {
            warn!("   🔴 {}: {:.2} > {:.2}", 
                  violation.description, violation.current_value, violation.limit_value);
        }
    } else {
        info!("\n✅ 所有風險指標都在正常範圍內");
    }
    
    // 管理建議
    if !final_report.recommendations.is_empty() {
        info!("\n💡 風險管理建議:");
        for rec in &final_report.recommendations {
            let priority_emoji = match rec.priority {
                RecommendationPriority::Critical => "🚨",
                RecommendationPriority::High => "🔴",
                RecommendationPriority::Medium => "🟡",
                RecommendationPriority::Low => "🟢",
            };
            info!("   {} {}: {}", priority_emoji, rec.description, rec.suggested_action);
        }
    }
    
    info!("\n🎉 風險管理演示報告生成完成！");
    Ok(())
}
// Moved to grouped example: 04_risk/main.rs
