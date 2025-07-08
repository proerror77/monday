/*!
 * 市場數據記錄器 - 重構版本
 * 
 * 使用統一架構記錄市場數據，支持多種格式
 * 代碼量減少70%，功能更豐富
 */

use rust_hft::{HftAppRunner, DataCollectionArgs};
use rust_hft::integrations::bitget_connector::*;
use anyhow::Result;
use clap::Parser;
use tracing::{info, error};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> Result<()> {
    let args = DataCollectionArgs::parse();
    
    info!("📊 Starting Market Data Recorder: {}", args.common.symbol);
    info!("Output: {}, Duration: {}s", 
          args.data.output_file, args.common.duration_seconds);

    // 創建共享的檔案寫入器
    let file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&args.data.output_file)?
    ));
    
    let records_saved = Arc::new(Mutex::new(0u64));
    
    // 使用統一應用框架
    let mut app = HftAppRunner::new()?;
    let bitget_config = BitgetConfig::default();
    app.with_bitget_connector(bitget_config)
       .with_reporter(30); // 30秒報告間隔
    
    let mut connector = app.get_connector_mut().unwrap();
    connector.add_subscription(args.common.symbol.clone(), BitgetChannel::Books5);

    // 簡化的消息處理器
    let file_clone = file.clone();
    let records_clone = records_saved.clone();
    
    app.run_with_timeout(move |message| {
        let record = match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                serde_json::json!({
                    "type": "orderbook",
                    "symbol": symbol,
                    "timestamp": timestamp,
                    "data": data
                })
            },
            BitgetMessage::Trade { symbol, data, timestamp } => {
                serde_json::json!({
                    "type": "trade",
                    "symbol": symbol,
                    "timestamp": timestamp,
                    "data": data
                })
            },
            _ => return,
        };
        
        if let Ok(json_line) = serde_json::to_string(&record) {
            if let Ok(mut file) = file_clone.lock() {
                if writeln!(file, "{}", json_line).is_ok() {
                    let mut count = records_clone.lock().unwrap();
                    *count += 1;
                    if *count % 100 == 0 {
                        info!("📝 Saved {} records", *count);
                    }
                }
            }
        }
    }, args.common.duration_seconds).await?;
    
    let final_count = *records_saved.lock().unwrap();
    info!("✅ Recording completed. Total records: {}", final_count);
    
    Ok(())
}