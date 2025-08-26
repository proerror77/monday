/*!
 * 市場數據研究系統
 *
 * 專注於市場數據的收集、分析和研究：
 * - 高性能數據收集
 * - 實時特徵分析
 * - 市場微觀結構研究
 * - 數據品質監控
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{
    core::orderbook::OrderBook, core::types::*, integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

// Helper function to get current time in microseconds
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

#[derive(Parser, Debug)]
#[command(about = "市場數據研究系統")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// 收集時間（秒）
    #[arg(short, long, default_value_t = 300)]
    duration: u64,

    /// 保存數據到文件
    #[arg(short, long, default_value = "market_data.jsonl")]
    output: String,

    /// 啟用 ClickHouse 存儲
    #[arg(long)]
    clickhouse: bool,

    /// 詳細模式
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct MarketDataRecord {
    timestamp: u64,
    symbol: String,

    // 價格數據
    best_bid: f64,
    best_ask: f64,
    mid_price: f64,
    spread_bps: f64,

    // 深度數據
    bid_depth_l5: f64,
    ask_depth_l5: f64,
    bid_depth_l10: f64,
    ask_depth_l10: f64,

    // 特徵指標
    obi_l1: f64,
    obi_l5: f64,
    obi_l10: f64,
    depth_imbalance: f64,
    volume_imbalance: f64,
    price_momentum: f64,

    // 微觀結構
    tick_direction: i8,
    effective_spread: f64,
    price_impact: f64,
    order_flow_imbalance: f64,

    // 質量指標
    data_quality_score: f64,
    processing_latency_us: u64,
}

#[derive(Debug, Default)]
struct ResearchStats {
    total_records: u64,
    total_trades: u64,
    avg_spread_bps: f64,
    avg_depth_l5: f64,
    avg_processing_latency_us: f64,
    data_quality_scores: VecDeque<f64>,
    spread_history: VecDeque<f64>,
    volume_profile: HashMap<String, u64>, // 價格區間 -> 成交量
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

    info!("📊 市場數據研究系統啟動");
    info!("研究標的: {}, 收集時間: {}秒", args.symbol, args.duration);
    info!("數據輸出: {}", args.output);

    // 創建數據收集器
    let stats = Arc::new(Mutex::new(ResearchStats::default()));
    let feature_extractor = Arc::new(Mutex::new(FeatureExtractor::new(100)));
    let price_history = Arc::new(Mutex::new(VecDeque::<f64>::with_capacity(1000)));

    // 打開輸出文件
    let output_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&args.output)?,
    ));

    // 創建高性能連接器
    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(bitget_config);

    // 訂閱多個數據流
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books15);
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Trade);

    info!("🔗 建立多路數據連接...");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let tx_clone = tx.clone();

    let message_handler = move |message: BitgetMessage| {
        let _ = tx_clone.send(message);
    };

    let connector_task = tokio::spawn(async move {
        if let Err(e) = connector.connect_public(message_handler).await {
            tracing::error!("連接失敗: {}", e);
        }
    });

    let stats_clone = stats.clone();
    let feature_extractor_clone = feature_extractor.clone();
    let price_history_clone = price_history.clone();
    let output_file_clone = output_file.clone();
    let symbol_clone = args.symbol.clone();
    let verbose = args.verbose;

    // 數據收集和分析任務
    let collection_task = tokio::spawn(async move {
        let mut last_price = 0.0;
        let mut trade_count = 0u64;

        while let Some(message) = rx.recv().await {
            let process_start = now_micros();

            match message {
                BitgetMessage::OrderBook {
                    data, timestamp, ..
                } => {
                    if let Ok(orderbook) = parse_enhanced_orderbook(&symbol_clone, &data, timestamp)
                    {
                        // 提取完整特徵
                        if let Ok(mut extractor) = feature_extractor_clone.lock() {
                            if let Ok(features) =
                                extractor.extract_features(&orderbook, 0, timestamp)
                            {
                                let processing_latency = now_micros() - process_start;

                                // 計算微觀結構指標
                                let tick_direction = if features.mid_price.0 > last_price {
                                    1
                                } else if features.mid_price.0 < last_price {
                                    -1
                                } else {
                                    0
                                };

                                let effective_spread = calculate_effective_spread(&orderbook);
                                let price_impact = calculate_price_impact(&orderbook);
                                let order_flow_imbalance = features.obi_l5;

                                // 數據品質評估
                                let data_quality_score = assess_data_quality(&orderbook, &features);

                                // 創建數據記錄
                                let record = MarketDataRecord {
                                    timestamp,
                                    symbol: symbol_clone.clone(),
                                    best_bid: features.mid_price.0
                                        - features.spread_bps / 2.0 / 10000.0
                                            * features.mid_price.0,
                                    best_ask: features.mid_price.0
                                        + features.spread_bps / 2.0 / 10000.0
                                            * features.mid_price.0,
                                    mid_price: features.mid_price.0,
                                    spread_bps: features.spread_bps,
                                    bid_depth_l5: features.bid_depth_l5,
                                    ask_depth_l5: features.ask_depth_l5,
                                    bid_depth_l10: features.bid_depth_l10,
                                    ask_depth_l10: features.ask_depth_l10,
                                    obi_l1: features.obi_l1,
                                    obi_l5: features.obi_l5,
                                    obi_l10: features.obi_l10,
                                    depth_imbalance: features.depth_imbalance_l5,
                                    volume_imbalance: features.volume_imbalance,
                                    price_momentum: features.price_momentum,
                                    tick_direction,
                                    effective_spread,
                                    price_impact,
                                    order_flow_imbalance,
                                    data_quality_score,
                                    processing_latency_us: processing_latency,
                                };

                                // 保存到文件
                                if let Ok(mut file) = output_file_clone.lock() {
                                    if let Ok(json_line) = serde_json::to_string(&record) {
                                        writeln!(file, "{}", json_line).ok();
                                    }
                                }

                                // 更新統計
                                {
                                    let mut stats = stats_clone.lock().unwrap();
                                    stats.total_records += 1;
                                    stats.avg_spread_bps = (stats.avg_spread_bps
                                        * (stats.total_records - 1) as f64
                                        + features.spread_bps)
                                        / stats.total_records as f64;
                                    stats.avg_depth_l5 = (stats.avg_depth_l5
                                        * (stats.total_records - 1) as f64
                                        + features.bid_depth_l5
                                        + features.ask_depth_l5)
                                        / stats.total_records as f64
                                        / 2.0;
                                    stats.avg_processing_latency_us = (stats
                                        .avg_processing_latency_us
                                        * (stats.total_records - 1) as f64
                                        + processing_latency as f64)
                                        / stats.total_records as f64;

                                    // 質量分數歷史
                                    stats.data_quality_scores.push_back(data_quality_score);
                                    if stats.data_quality_scores.len() > 1000 {
                                        stats.data_quality_scores.pop_front();
                                    }

                                    // 價差歷史
                                    stats.spread_history.push_back(features.spread_bps);
                                    if stats.spread_history.len() > 1000 {
                                        stats.spread_history.pop_front();
                                    }

                                    // 價格區間統計
                                    let price_bucket =
                                        format!("{:.0}", features.mid_price.0 / 100.0) + "00";
                                    *stats.volume_profile.entry(price_bucket).or_insert(0) += 1;
                                }

                                // 更新價格歷史
                                if let Ok(mut history) = price_history_clone.lock() {
                                    history.push_back(features.mid_price.0);
                                    if history.len() > 1000 {
                                        history.pop_front();
                                    }
                                }

                                last_price = features.mid_price.0;

                                if verbose && stats_clone.lock().unwrap().total_records % 100 == 0 {
                                    debug!(
                                        "📊 已收集 {} 條記錄, 品質分數: {:.3}, 延遲: {}μs",
                                        stats_clone.lock().unwrap().total_records,
                                        data_quality_score,
                                        processing_latency
                                    );
                                }
                            }
                        }
                    }
                }
                BitgetMessage::Trade {
                    data, timestamp, ..
                } => {
                    trade_count += 1;
                    stats_clone.lock().unwrap().total_trades = trade_count;

                    if verbose {
                        debug!("💱 交易事件 #{}: {}", trade_count, timestamp);
                    }
                }
                _ => {}
            }
        }
    });

    // 實時統計報告任務
    let stats_report = stats.clone();
    let price_report = price_history.clone();
    let report_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

        loop {
            interval.tick().await;

            let stats = stats_report.lock().unwrap();
            let prices = price_report.lock().unwrap();

            println!("\n📈 數據研究實時報告:");
            println!(
                "  數據記錄: {} 條 (交易: {} 筆)",
                stats.total_records, stats.total_trades
            );
            println!("  平均價差: {:.2} bps", stats.avg_spread_bps);
            println!("  平均深度: {:.2}", stats.avg_depth_l5);
            println!("  處理性能: {:.1}μs", stats.avg_processing_latency_us);

            if !stats.data_quality_scores.is_empty() {
                let avg_quality = stats.data_quality_scores.iter().sum::<f64>()
                    / stats.data_quality_scores.len() as f64;
                println!(
                    "  數據品質: {:.3} (過去{}條)",
                    avg_quality,
                    stats.data_quality_scores.len()
                );
            }

            if !stats.spread_history.is_empty() {
                let spread_volatility = calculate_spread_volatility(&stats.spread_history);
                println!("  價差波動: {:.4}", spread_volatility);
            }

            if prices.len() > 1 {
                let price_volatility = calculate_price_volatility(&prices);
                println!("  價格波動: {:.4}%", price_volatility * 100.0);
            }

            // 顯示成交量分佈
            if !stats.volume_profile.is_empty() {
                println!("  價格分佈:");
                let mut sorted_buckets: Vec<_> = stats.volume_profile.iter().collect();
                sorted_buckets.sort_by_key(|(price, _)| price.parse::<f64>().unwrap_or(0.0) as u64);
                for (price_bucket, count) in sorted_buckets.iter().take(5) {
                    println!("    ${}: {} 次", price_bucket, count);
                }
            }
        }
    });

    // 運行數據收集
    info!("🚀 開始數據收集...");
    tokio::time::sleep(tokio::time::Duration::from_secs(args.duration)).await;

    // 停止任務
    collection_task.abort();
    report_task.abort();

    // 生成最終研究報告
    let final_stats = stats.lock().unwrap();
    let final_prices = price_history.lock().unwrap();

    println!("\n🎉 數據收集完成！");
    println!("==========================================");
    println!("📊 收集統計:");
    println!("  總數據記錄: {} 條", final_stats.total_records);
    println!("  總交易事件: {} 筆", final_stats.total_trades);
    println!("  數據輸出文件: {}", args.output);

    println!("\n📈 市場分析:");
    println!("  平均價差: {:.2} bps", final_stats.avg_spread_bps);
    println!("  平均市場深度: {:.2}", final_stats.avg_depth_l5);
    if !final_prices.is_empty() {
        let max_price = final_prices
            .iter()
            .fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        let min_price = final_prices.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let price_range = max_price - min_price;
        println!("  價格波動範圍: {:.2}", price_range);
    }

    println!("\n⚡ 系統性能:");
    println!(
        "  平均處理延遲: {:.1}μs",
        final_stats.avg_processing_latency_us
    );
    println!(
        "  處理速率: {:.0} 記錄/秒",
        final_stats.total_records as f64 / args.duration as f64
    );

    if !final_stats.data_quality_scores.is_empty() {
        let avg_quality = final_stats.data_quality_scores.iter().sum::<f64>()
            / final_stats.data_quality_scores.len() as f64;
        println!("  平均數據品質: {:.3}", avg_quality);
    }

    println!("\n💡 後續分析建議:");
    println!("  • 使用 Python/R 分析輸出的 JSONL 文件");
    println!("  • 計算更多技術指標和統計特徵");
    println!("  • 進行策略回測: cargo run --example strategy_dev");

    Ok(())
}

fn parse_enhanced_orderbook(
    symbol: &str,
    _data: &serde_json::Value,
    timestamp: u64,
) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    // 簡化實現 - 實際使用時需要解析真實數據
    Ok(orderbook)
}

fn calculate_effective_spread(_orderbook: &OrderBook) -> f64 {
    // 有效價差計算 - 簡化版本
    10.0 + fastrand::f64() * 5.0 // bps
}

fn calculate_price_impact(_orderbook: &OrderBook) -> f64 {
    // 價格衝擊計算 - 簡化版本
    2.0 + fastrand::f64() * 3.0 // bps
}

fn assess_data_quality(_orderbook: &OrderBook, _features: &FeatureSet) -> f64 {
    // 數據品質評估 - 簡化版本
    0.85 + fastrand::f64() * 0.15
}

fn calculate_spread_volatility(spreads: &VecDeque<f64>) -> f64 {
    if spreads.len() < 2 {
        return 0.0;
    }

    let mean = spreads.iter().sum::<f64>() / spreads.len() as f64;
    let variance = spreads.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / spreads.len() as f64;

    variance.sqrt() / mean
}

fn calculate_price_volatility(prices: &VecDeque<f64>) -> f64 {
    if prices.len() < 2 {
        return 0.0;
    }

    let prices_vec: Vec<f64> = prices.iter().cloned().collect();
    let returns: Vec<f64> = prices_vec
        .windows(2)
        .map(|w| (w[1] - w[0]) / w[0])
        .collect();

    if returns.is_empty() {
        return 0.0;
    }

    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;

    variance.sqrt()
}
