/*!
 * Simple BTCUSDT DL Demo
 * 
 * 簡化版的BTCUSDT深度學習交易演示，專注於核心功能驗證
 */

use rust_hft::{
    engine::{
        TestCapitalPositionManager,
        TestCapitalConfig,
        PositionAssessment,
    },
    ml::{TrendPrediction, TrendClass},
};
use anyhow::Result;
use std::collections::HashMap;
use tracing::{info, warn};
use tracing_subscriber;
use tokio::time::{Duration, sleep};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();
    
    info!("🚀 Simple BTCUSDT DL Trading Demo");
    info!("專注於單商品DL優化");
    
    // 配置BTCUSDT專門的測試資金管理
    let config = TestCapitalConfig {
        test_capital: 100.0,
        capital_per_symbol: 90.0, // 90% 分配給BTCUSDT
        min_position_size: 2.0,
        max_position_size: 20.0, // 提高到20%
        daily_loss_limit: 8.0,   // 8% 每日損失限制
        position_loss_limit: 2.5, // 2.5% 單倉損失限制
        max_drawdown_limit: 4.0,  // 4% 最大回撤
        target_daily_profit: 2.0, // 2% 每日目標（更積極）
        profit_taking_threshold: 3.0, // 3% 止盈
        kelly_fraction_conservative: 0.20, // 更積極的Kelly分數
        max_concurrent_positions: 3, // 3個並發倉位
        position_hold_time_seconds: 12, // 12秒目標持倉時間
        rebalance_interval_seconds: 45, // 45秒重平衡
        track_trade_performance: true,
        log_position_changes: true,
        calculate_metrics_realtime: true,
    };
    
    // 創建位置管理器
    info!("初始化BTCUSDT專門的倉位管理器...");
    let position_manager = TestCapitalPositionManager::new(config.clone());
    
    // 運行增強的交易模擬
    info!("🔥 開始增強的BTCUSDT DL交易模擬...");
    
    for cycle in 1..=20 { // 增加到20個週期
        info!("=== BTCUSDT DL交易週期 {} ===", cycle);
        
        // 模擬更真實的BTCUSDT價格變動
        let base_price = 50000.0;
        let time_factor = cycle as f64;
        let price_volatility = 500.0; // 1% 波動
        let btc_price = base_price + (time_factor * 50.0) + 
                       ((time_factor * 0.1).sin() * price_volatility);
        
        // 更新價格
        let mut price_updates = HashMap::new();
        price_updates.insert("BTCUSDT".to_string(), btc_price);
        position_manager.update_positions(price_updates).await?;
        
        // 生成增強的DL預測
        let prediction = generate_enhanced_dl_prediction(cycle, btc_price)?;
        
        info!("🤖 DL預測: {:?} | 置信度: {:.3} | 預期變化: {:.1}bps",
              prediction.trend_class,
              prediction.confidence,
              prediction.expected_change_bps);
        
        // 評估新倉位
        let assessment = position_manager.assess_new_position(
            "BTCUSDT",
            &prediction,
            btc_price,
        ).await?;
        
        match assessment {
            PositionAssessment::Approved { 
                suggested_size, 
                kelly_fraction,
                stop_loss,
                take_profit,
                .. 
            } => {
                // 確定交易方向
                let side = match prediction.trend_class {
                    TrendClass::StrongUp | TrendClass::WeakUp => rust_hft::core::types::Side::Bid,
                    TrendClass::StrongDown | TrendClass::WeakDown => rust_hft::core::types::Side::Ask,
                    TrendClass::Neutral => {
                        info!("🔄 中性信號，跳過交易");
                        continue;
                    }
                };
                
                // 開倉
                let position_id = position_manager.open_position(
                    "BTCUSDT".to_string(),
                    side,
                    suggested_size,
                    btc_price,
                    &prediction,
                    kelly_fraction,
                    stop_loss,
                    take_profit,
                ).await?;
                
                info!("✅ 開倉: {} {} {:.2} USDT @ {:.2} | Kelly: {:.3} | 推理: {}μs",
                      position_id,
                      match side { 
                          rust_hft::core::types::Side::Bid => "LONG",
                          rust_hft::core::types::Side::Ask => "SHORT"
                      },
                      suggested_size,
                      btc_price,
                      kelly_fraction,
                      prediction.inference_latency_us);
            },
            PositionAssessment::Rejected { reason } => {
                info!("❌ 倉位被拒絕: {}", reason);
            }
        }
        
        // 檢查平倉
        let closed_positions = position_manager.check_position_exits().await?;
        if !closed_positions.is_empty() {
            info!("🔄 平倉 {} 個倉位", closed_positions.len());
        }
        
        // 顯示投資組合狀態
        let status = position_manager.get_portfolio_status().await;
        info!("📊 組合狀態: P&L {:.2} USDT ({:.2}%) | {} 活躍倉位 | {:.1}% 效率",
              status.metrics.total_pnl,
              (status.metrics.total_pnl / config.test_capital) * 100.0,
              status.active_positions.len(),
              status.metrics.capital_efficiency * 100.0);
        
        // 檢查是否達到目標
        if status.metrics.total_pnl >= config.target_daily_profit {
            info!("🎯 達到每日利潤目標！");
        }
        
        if status.metrics.total_pnl <= -config.daily_loss_limit {
            warn!("⚠️ 達到每日損失限制，停止交易");
            break;
        }
        
        // 短暫暫停
        sleep(Duration::from_millis(200)).await;
    }
    
    // 最終分析
    let final_status = position_manager.get_portfolio_status().await;
    
    info!("📈 === BTCUSDT DL交易最終結果 ===");
    info!("總P&L: {:.2} USDT ({:.2}%)", 
          final_status.metrics.total_pnl,
          (final_status.metrics.total_pnl / config.test_capital) * 100.0);
    info!("總交易: {}", final_status.metrics.total_trades);
    info!("勝率: {:.1}%", final_status.metrics.win_rate);
    info!("資金效率: {:.1}%", final_status.metrics.capital_efficiency * 100.0);
    info!("最大回撤: {:.2} USDT", final_status.metrics.max_drawdown);
    info!("Sharpe比率: {:.2}", final_status.metrics.sharpe_ratio);
    
    // 性能評估
    let daily_return_pct = (final_status.metrics.total_pnl / config.test_capital) * 100.0;
    let performance_score = assess_dl_performance(&final_status.metrics, &config);
    
    info!("🎯 DL模型表現評估:");
    info!("  每日回報: {:.2}% (目標: ≥2.0%)", daily_return_pct);
    info!("  勝率: {:.1}% (目標: ≥65%)", final_status.metrics.win_rate);
    info!("  回撤: {:.2}% (目標: ≤4%)", 
          (final_status.metrics.max_drawdown / config.test_capital) * 100.0);
    info!("  效率: {:.1}% (目標: ≥85%)", final_status.metrics.capital_efficiency * 100.0);
    info!("  總分: {:.1}/10.0", performance_score);
    
    if performance_score >= 8.0 {
        info!("🏆 優秀！準備進入Phase 2多商品交易");
    } else if performance_score >= 6.0 {
        info!("✅ 良好！需要小幅優化後進入Phase 2");
    } else {
        warn!("⚠️ 需要改進！繼續Phase 1優化");
    }
    
    info!("✅ BTCUSDT DL交易演示完成");
    
    Ok(())
}

/// 生成增強的DL預測
fn generate_enhanced_dl_prediction(cycle: i32, current_price: f64) -> Result<TrendPrediction> {
    // 模擬更真實的DL模型行為
    let confidence_base = 0.55 + (cycle as f64 * 0.01).min(0.25); // 隨時間增加置信度
    let time_variation = ((cycle as f64 * 0.3).sin() + 1.0) * 0.5;
    let confidence = (confidence_base + time_variation * 0.2).min(0.95);
    
    // 基於週期的趨勢偏好（模擬DL學習）
    let trend_bias = match cycle % 6 {
        0 | 1 => 0.3,  // 偏向看漲
        2 | 3 => -0.2, // 偏向看跌
        4 => 0.0,      // 中性
        _ => 0.1,      // 輕微看漲
    };
    
    let random_factor = ((cycle as f64 * 0.7).cos() + 1.0) * 0.5;
    let final_direction = trend_bias + (random_factor - 0.5) * 0.4;
    
    let trend_class = if final_direction > 0.4 {
        TrendClass::StrongUp
    } else if final_direction > 0.1 {
        TrendClass::WeakUp
    } else if final_direction > -0.1 {
        TrendClass::Neutral
    } else if final_direction > -0.4 {
        TrendClass::WeakDown
    } else {
        TrendClass::StrongDown
    };
    
    let expected_change_bps = match trend_class {
        TrendClass::StrongUp => 12.0 + random_factor * 8.0,
        TrendClass::WeakUp => 4.0 + random_factor * 6.0,
        TrendClass::Neutral => (random_factor - 0.5) * 3.0,
        TrendClass::WeakDown => -(4.0 + random_factor * 6.0),
        TrendClass::StrongDown => -(12.0 + random_factor * 8.0),
    };
    
    // 模擬推理延遲（超低延遲目標）
    let inference_latency_us = 300 + (cycle as u64 % 400); // 300-700μs
    
    Ok(TrendPrediction {
        trend_class,
        class_probabilities: generate_class_probabilities(trend_class, confidence),
        confidence,
        expected_change_bps,
        inference_latency_us,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64,
        sequence_length: 40, // 增強的序列長度
    })
}

/// 生成類別概率分佈
fn generate_class_probabilities(predicted_class: TrendClass, confidence: f64) -> Vec<f64> {
    let mut probs = vec![0.1, 0.15, 0.5, 0.15, 0.1]; // 基礎分佈
    
    let class_idx = predicted_class as usize;
    probs[class_idx] = 0.3 + confidence * 0.5; // 增強預測類別的概率
    
    // 歸一化
    let sum: f64 = probs.iter().sum();
    probs.iter_mut().for_each(|p| *p /= sum);
    
    probs
}

/// 評估DL性能
fn assess_dl_performance(
    metrics: &rust_hft::engine::TestPortfolioMetrics,
    config: &TestCapitalConfig,
) -> f64 {
    let mut score = 0.0;
    
    // 回報表現 (40%)
    let daily_return_pct = (metrics.total_pnl / config.test_capital) * 100.0;
    if daily_return_pct >= 2.0 {
        score += 4.0;
    } else if daily_return_pct >= 1.5 {
        score += 3.0;
    } else if daily_return_pct >= 1.0 {
        score += 2.0;
    } else if daily_return_pct >= 0.0 {
        score += 1.0;
    }
    
    // 勝率 (25%)
    if metrics.win_rate >= 65.0 {
        score += 2.5;
    } else if metrics.win_rate >= 60.0 {
        score += 2.0;
    } else if metrics.win_rate >= 55.0 {
        score += 1.5;
    } else if metrics.win_rate >= 50.0 {
        score += 1.0;
    }
    
    // 風險控制 (20%)
    let drawdown_pct = (metrics.max_drawdown / config.test_capital) * 100.0;
    if drawdown_pct <= 2.0 {
        score += 2.0;
    } else if drawdown_pct <= 3.0 {
        score += 1.5;
    } else if drawdown_pct <= 4.0 {
        score += 1.0;
    } else if drawdown_pct <= 5.0 {
        score += 0.5;
    }
    
    // 資金效率 (15%)
    if metrics.capital_efficiency >= 0.85 {
        score += 1.5;
    } else if metrics.capital_efficiency >= 0.8 {
        score += 1.0;
    } else if metrics.capital_efficiency >= 0.75 {
        score += 0.5;
    }
    
    score
}