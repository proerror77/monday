/*!
 * 基礎交易所連接測試 - 重構版本
 * 
 * 演示如何使用統一應用框架連接到 Bitget 交易所
 * 代碼量減少60%，功能更強大
 */

use rust_hft::{run_timed_app, BasicConnectionArgs};
use rust_hft::integrations::bitget_connector::*;
use anyhow::Result;
use clap::Parser;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let args = BasicConnectionArgs::parse();
    
    // 3行代碼完成連接測試！
    run_timed_app(&args.common.symbol, args.common.duration_seconds, |message| {
        match message {
            BitgetMessage::OrderBook { symbol, timestamp, .. } => {
                info!("📊 OrderBook for {} at {}", symbol, timestamp);
            },
            BitgetMessage::Trade { symbol, .. } => {
                info!("✅ Trade data for {}", symbol);
            },
            _ => {}
        }
    }).await
}