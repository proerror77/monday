/*!
 * 策略回測框架
 * 
 * 使用歷史數據對交易策略進行回測和績效評估
 */

use rust_hft::{
    core::{types::*, orderbook::OrderBook},
    ml::features::FeatureExtractor,
    utils::backtesting::*,
};
use rust_decimal::prelude::ToPrimitive;
use anyhow::Result;
use tracing::{info, warn, error};
use std::fs::File;
use std::io::{BufRead, BufReader};
use serde_json::Value;
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value_t = String::from("market_data.jsonl"))]
    input_file: String,
    
    #[arg(long, default_value_t = 10000.0)]
    initial_capital: f64,
    
    #[arg(long, default_value_t = 0.001)]
    trading_fee: f64,
}

#[derive(Debug)]
struct BacktestResults {
    total_trades: u64,
    winning_trades: u64,
    total_pnl: f64,
    max_drawdown: f64,
    sharpe_ratio: f64,
    win_rate: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("📈 Starting Strategy Backtest");
    info!("Input: {}, Capital: ${:.2}, Fee: {:.3}%", 
          args.input_file, args.initial_capital, args.trading_fee * 100.0);

    // 初始化組件
    let mut feature_extractor = FeatureExtractor::new(50);
    let backtest_config = BacktestConfig {
        start_time: 0,
        end_time: u64::MAX,
        initial_balance: args.initial_capital,
        commission_rate: args.trading_fee,
        slippage_bps: 1.0,
        risk_free_rate: 0.02,
        max_position_size: 1.0,
        benchmark_symbol: "BTCUSDT".to_string(),
        max_daily_trades: 100,
        output_path: "backtest_results.json".to_string(),
    };
    let mut backtest_engine = BacktestEngine::new(backtest_config);

    // 讀取歷史數據
    let file = File::open(&args.input_file)?;
    let reader = BufReader::new(file);
    
    let mut processed_records = 0u64;
    
    for line in reader.lines() {
        let line = line?;
        
        if let Ok(record) = serde_json::from_str::<Value>(&line) {
            if record["type"] == "orderbook" {
                // 解析 orderbook 數據
                if let Ok(orderbook) = parse_orderbook_from_record(&record) {
                    // 提取特徵並進行簡單策略回測
                    if let Ok(features) = feature_extractor.extract_features(&orderbook, 50, orderbook.last_update) {
                        // 簡單的均值回歸策略
                        let signal = simple_mean_reversion_strategy(&features);
                        if let Some(signal) = signal {
                            // 執行回測交易
                            if let Err(e) = backtest_engine.execute_trade(&signal, *features.mid_price) {
                                warn!("交易執行失敗: {}", e);
                            }
                        }
                    }
                }
                
                processed_records += 1;
                if processed_records % 1000 == 0 {
                    info!("📊 Processed {} records", processed_records);
                }
            }
        }
    }
    
    // 生成回測結果
    let results = backtest_engine.generate_results();
    print_backtest_results(&results);
    
    Ok(())
}

// 簡單的均值回歸策略
fn simple_mean_reversion_strategy(features: &FeatureSet) -> Option<TradingSignal> {
    // 檢查是否有足夠的特徵數據
    if features.spread_bps.is_nan() || features.mid_price.is_nan() {
        return None;
    }
    
    // 簡單的策略邏輯：當價差過大時進行交易
    let spread_threshold = 100.0; // 100 基點價差閾值
    let spread_ratio = features.spread_bps;
    
    if spread_ratio > spread_threshold {
        // 賣出信號
        Some(TradingSignal {
            signal_type: SignalType::Sell,
            confidence: 0.7,
            suggested_price: features.mid_price,
            suggested_quantity: rust_decimal::Decimal::from_f64_retain(0.1).unwrap_or_default(),
            timestamp: now_micros(),
            features_timestamp: features.timestamp,
            signal_latency_us: 0,
        })
    } else if spread_ratio < spread_threshold * 0.5 {
        // 買入信號  
        Some(TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.7,
            suggested_price: features.mid_price,
            suggested_quantity: rust_decimal::Decimal::from_f64_retain(0.1).unwrap_or_default(),
            timestamp: now_micros(),
            features_timestamp: features.timestamp,
            signal_latency_us: 0,
        })
    } else {
        None
    }
}

fn parse_orderbook_from_record(record: &Value) -> Result<OrderBook> {
    let symbol = record["symbol"].as_str().unwrap_or("UNKNOWN").to_string();
    let timestamp = record["timestamp"].as_u64().unwrap_or(0);
    let data = &record["data"];
    
    let mut orderbook = OrderBook::new(symbol);
    orderbook.last_update = timestamp;
    
    // 簡化的解析邏輯
    if let Some(data_array) = data.as_array() {
        if let Some(first_item) = data_array.first() {
            // 解析 bids 和 asks...
            // (這裡應該包含完整的解析邏輯)
        }
    }
    
    Ok(orderbook)
}

fn print_backtest_results(results: &BacktestResults) {
    info!("📋 === BACKTEST RESULTS ===");
    info!("Total Trades: {}", results.total_trades);
    info!("Winning Trades: {}", results.winning_trades);
    info!("Win Rate: {:.2}%", results.win_rate * 100.0);
    info!("Total PnL: ${:.2}", results.total_pnl);
    info!("Max Drawdown: {:.2}%", results.max_drawdown * 100.0);
    info!("Sharpe Ratio: {:.3}", results.sharpe_ratio);
    info!("=========================");
}

// 簡化的回測引擎
struct BacktestEngine {
    initial_capital: f64,
    current_capital: f64,
    trading_fee: f64,
    current_position: f64,
    entry_price: f64,
    trades: Vec<Trade>,
}

impl BacktestEngine {
    fn execute_trade(&mut self, signal: &TradingSignal, current_price: f64) -> Result<()> {
        match signal.signal_type {
            SignalType::Buy => {
                if self.current_position <= 0.0 {
                    let position_size = signal.suggested_quantity.to_f64().unwrap_or(0.0);
                    self.current_position = position_size;
                    self.entry_price = current_price;
                }
            },
            SignalType::Sell => {
                if self.current_position >= 0.0 {
                    let position_size = signal.suggested_quantity.to_f64().unwrap_or(0.0);
                    self.current_position = -position_size;
                    self.entry_price = current_price;
                }
            },
            SignalType::Hold => {
                if self.current_position != 0.0 {
                    let _pnl = self.calculate_pnl(current_price);
                    self.close_position(current_price, signal.timestamp);
                }
            }
        }
        
        Ok(())
    }
}

#[derive(Debug)]
struct Trade {
    entry_price: f64,
    exit_price: f64,
    quantity: f64,
    pnl: f64,
    timestamp: u64,
}

impl BacktestEngine {
    fn new(config: BacktestConfig) -> Self {
        Self {
            initial_capital: config.initial_balance,
            current_capital: config.initial_balance,
            trading_fee: config.commission_rate,
            current_position: 0.0,
            entry_price: 0.0,
            trades: Vec::new(),
        }
    }
    
    fn calculate_pnl(&self, current_price: f64) -> f64 {
        if self.current_position == 0.0 {
            return 0.0;
        }
        
        let price_diff = if self.current_position > 0.0 {
            current_price - self.entry_price
        } else {
            self.entry_price - current_price
        };
        
        price_diff * self.current_position.abs()
    }
    
    fn close_position(&mut self, exit_price: f64, timestamp: u64) {
        let pnl = self.calculate_pnl(exit_price);
        let fee = self.current_position.abs() * exit_price * self.trading_fee;
        let net_pnl = pnl - fee;
        
        let trade = Trade {
            entry_price: self.entry_price,
            exit_price,
            quantity: self.current_position,
            pnl: net_pnl,
            timestamp,
        };
        
        self.trades.push(trade);
        self.current_capital += net_pnl;
        self.current_position = 0.0;
        self.entry_price = 0.0;
    }
    
    fn generate_results(&self) -> BacktestResults {
        let total_trades = self.trades.len() as u64;
        let winning_trades = self.trades.iter().filter(|t| t.pnl > 0.0).count() as u64;
        let total_pnl = self.current_capital - self.initial_capital;
        let win_rate = if total_trades > 0 { winning_trades as f64 / total_trades as f64 } else { 0.0 };
        
        // 簡化的計算
        let max_drawdown = 0.0; // 需要更複雜的計算
        let sharpe_ratio = 0.0;  // 需要更複雜的計算
        
        BacktestResults {
            total_trades,
            winning_trades,
            total_pnl,
            max_drawdown,
            sharpe_ratio,
            win_rate,
        }
    }
}