/*
 * 简化的ClickHouse连接测试
 * 用于诊断异步运行时问题
 */

use anyhow::Result;
use clickhouse::{Client, Row};
use serde::{Serialize, Deserialize};
use tracing::{info, error};
use rustls::crypto::aws_lc_rs;

#[derive(Debug, Clone, Row, Serialize, Deserialize)]
struct TestRecord {
    timestamp: i64,
    symbol: String,
    price: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化 rustls crypto provider
    let _ = aws_lc_rs::default_provider().install_default();
    
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    info!("🚀 启动简化ClickHouse测试");
    
    // 创建ClickHouse客户端
    let client = Client::default().with_url("http://localhost:8123");
    
    // 测试基本连接
    info!("📡 测试ClickHouse连接...");
    
    // 创建测试记录
    let test_record = TestRecord {
        timestamp: chrono::Utc::now().timestamp_micros(),
        symbol: "BTCUSDT".to_string(),
        price: 45000.0,
    };
    
    info!("✅ 测试记录创建: {:?}", test_record);
    
    // 测试数据库查询
    match client.query("SELECT 1").fetch_one::<u8>().await {
        Ok(result) => {
            info!("✅ ClickHouse连接成功，查询结果: {}", result);
        }
        Err(e) => {
            error!("❌ ClickHouse连接失败: {}", e);
            return Err(e.into());
        }
    }
    
    // 测试数据库列表
    match client.query("SHOW DATABASES").fetch_all::<String>().await {
        Ok(databases) => {
            info!("📊 可用数据库: {:?}", databases);
        }
        Err(e) => {
            error!("❌ 查询数据库列表失败: {}", e);
        }
    }
    
    info!("🎉 简化测试完成");
    Ok(())
}