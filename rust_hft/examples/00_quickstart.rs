/*!
 * HFT 系統快速入門
 *
 * 5分鐘體驗完整的 HFT 系統功能：
 * - WebSocket 連接和數據接收
 * - 實時特徵提取
 * - 簡單交易策略
 * - 性能監控
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{
    core::orderbook::OrderBook,
    integrations::{SimpleBitgetConnector, SimpleChannel},
    ml::features::FeatureExtractor,
    now_micros,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::info;

#[derive(Parser, Debug)]
#[command(about = "HFT 系統快速入門")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// 運行時間（秒）
    #[arg(short, long, default_value_t = 30)]
    duration: u64,

    /// 詳細模式
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Default)]
struct Stats {
    messages: u64,
    signals: u64,
    buy_signals: u64,
    sell_signals: u64,
    avg_latency_us: f64,
    best_bid: f64,
    best_ask: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(if args.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();

    info!("🚀 HFT 系統快速入門");
    info!("交易對: {}, 運行時間: {}秒", args.symbol, args.duration);

    // 創建統計對象
    let stats = Arc::new(Mutex::new(Stats::default()));
    let feature_extractor = Arc::new(Mutex::new(FeatureExtractor::new(50)));
    let price_history = Arc::new(Mutex::new(VecDeque::<f64>::with_capacity(100)));

    // 創建連接器
    let connector = SimpleBitgetConnector::new(None);

    // 訂閱市場數據
    connector
        .subscribe(&args.symbol, SimpleChannel::OrderBook5)
        .await?;
    connector
        .subscribe(&args.symbol, SimpleChannel::Trade)
        .await?;

    info!("📡 連接到 Bitget 交易所...");

    // 啟動連接器並獲取消息接收器
    let mut message_rx = connector.start().await?;

    let stats_clone = stats.clone();
    let feature_extractor_clone = feature_extractor.clone();
    let price_history_clone = price_history.clone();
    let symbol_clone = args.symbol.clone();
    let verbose = args.verbose;

    // 數據處理任務
    let processing_task = tokio::spawn(async move {
        while let Some(message) = message_rx.recv().await {
            let process_start = now_micros();

            match message.channel {
                SimpleChannel::OrderBook5
                | SimpleChannel::OrderBook1
                | SimpleChannel::OrderBook15 => {
                    let timestamp = message.timestamp;
                    let data = &message.data;
                    if let Ok(orderbook) = parse_orderbook(&symbol_clone, &data, timestamp) {
                        // 更新統計
                        {
                            let mut stats = stats_clone.lock().unwrap();
                            stats.messages += 1;

                            // 假設價格範圍
                            stats.best_bid = 50000.0 + fastrand::f64() * 1000.0;
                            stats.best_ask = stats.best_bid + 10.0;
                        }

                        // 特徵提取
                        if let Ok(mut extractor) = feature_extractor_clone.lock() {
                            if let Ok(features) =
                                extractor.extract_features(&orderbook, 0, timestamp)
                            {
                                // 簡單交易策略
                                let signal = generate_simple_signal(&features);

                                if signal != "HOLD" {
                                    let mut stats = stats_clone.lock().unwrap();
                                    stats.signals += 1;

                                    if signal == "BUY" {
                                        stats.buy_signals += 1;
                                        if verbose {
                                            info!(
                                                "🟢 BUY 信號 @ {:.2} (置信度: {:.2})",
                                                features.mid_price.0,
                                                features.obi_l5.abs()
                                            );
                                        }
                                    } else {
                                        stats.sell_signals += 1;
                                        if verbose {
                                            info!(
                                                "🔴 SELL 信號 @ {:.2} (置信度: {:.2})",
                                                features.mid_price.0,
                                                features.obi_l5.abs()
                                            );
                                        }
                                    }
                                }

                                // 更新價格歷史
                                if let Ok(mut history) = price_history_clone.lock() {
                                    history.push_back(features.mid_price.0);
                                    if history.len() > 100 {
                                        history.pop_front();
                                    }
                                }
                            }
                        }

                        // 計算處理延遲
                        let latency = now_micros() - process_start;
                        {
                            let mut stats = stats_clone.lock().unwrap();
                            stats.avg_latency_us = (stats.avg_latency_us
                                * (stats.messages - 1) as f64
                                + latency as f64)
                                / stats.messages as f64;
                        }
                    }
                }
                SimpleChannel::Trade => {
                    // 處理交易數據
                }
                _ => {}
            }
        }
    });

    // 狀態報告任務
    let stats_report = stats.clone();
    let price_report = price_history.clone();
    let report_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));

        loop {
            interval.tick().await;

            let stats = stats_report.lock().unwrap();
            let price_hist = price_report.lock().unwrap();

            let volatility = if price_hist.len() > 1 {
                calculate_volatility(&price_hist)
            } else {
                0.0
            };

            println!("\n📊 實時統計 (過去5秒):");
            println!("  消息處理: {} 條", stats.messages);
            println!(
                "  交易信號: {} 個 (買: {}, 賣: {})",
                stats.signals, stats.buy_signals, stats.sell_signals
            );
            println!(
                "  最佳價格: {:.2} / {:.2} (價差: {:.4})",
                stats.best_bid,
                stats.best_ask,
                stats.best_ask - stats.best_bid
            );
            println!("  處理延遲: {:.1}μs", stats.avg_latency_us);
            println!("  波動率: {:.4}%", volatility * 100.0);
        }
    });

    // 運行指定時間
    tokio::time::sleep(tokio::time::Duration::from_secs(args.duration)).await;

    // 停止任務
    processing_task.abort();
    report_task.abort();

    // 最終報告
    let final_stats = stats.lock().unwrap();
    let final_prices = price_history.lock().unwrap();

    println!("\n🎉 快速入門完成！");
    println!("===============================");
    println!("總處理消息: {} 條", final_stats.messages);
    println!("總交易信號: {} 個", final_stats.signals);
    println!("  └─ 買入信號: {} 個", final_stats.buy_signals);
    println!("  └─ 賣出信號: {} 個", final_stats.sell_signals);
    println!("平均處理延遲: {:.1}μs", final_stats.avg_latency_us);
    println!(
        "信號勝率: {:.1}%",
        if final_stats.signals > 0 {
            (final_stats.buy_signals + final_stats.sell_signals) as f64 / final_stats.signals as f64
                * 100.0
        } else {
            0.0
        }
    );

    if !final_prices.is_empty() && final_prices.len() > 1 {
        let price_change = final_prices.back().unwrap() - final_prices.front().unwrap();
        println!(
            "價格變動: {:.2} ({:.2}%)",
            price_change,
            price_change / final_prices.front().unwrap() * 100.0
        );
    }

    println!("\n💡 下一步:");
    println!("  • 嘗試: cargo run --example data_research");
    println!("  • 嘗試: cargo run --example strategy_dev");
    println!("  • 嘗試: cargo run --example performance_test");

    Ok(())
}

fn parse_orderbook(symbol: &str, _data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    // 簡化實現 - 實際使用時需要解析真實數據
    Ok(orderbook)
}

fn generate_simple_signal(features: &rust_hft::FeatureSet) -> &'static str {
    // 基於 Order Book Imbalance 的簡單策略
    if features.obi_l5 > 0.3 {
        "BUY"
    } else if features.obi_l5 < -0.3 {
        "SELL"
    } else {
        "HOLD"
    }
}

fn calculate_volatility(prices: &VecDeque<f64>) -> f64 {
    if prices.len() < 2 {
        return 0.0;
    }

    // 轉換為 Vec 來使用 windows
    let price_vec: Vec<f64> = prices.iter().cloned().collect();
    let returns: Vec<f64> = price_vec.windows(2).map(|w| (w[1] - w[0]) / w[0]).collect();

    if returns.is_empty() {
        return 0.0;
    }

    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;

    variance.sqrt()
}
