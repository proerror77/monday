#![allow(unexpected_cfgs)]

mod db;
mod exchanges;
mod spool;

use anyhow::Result;
use clap::Parser;
use clickhouse::Client;
use exchanges::{create_exchange, DepthMode, ExchangeContext, MessageBuffers};
use futures::{SinkExt, StreamExt};
use rand::Rng;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};
use http::Request;

#[derive(Parser, Debug)]
#[command(author, version, about = "多交易所數據收集器：WebSocket → ClickHouse")]
struct Args {
    /// 交易所列表（逗號分隔，支持: bitget, binance, bybit, okx 等）
    #[arg(long, default_value = "bitget")]
    exchanges: String,

    /// 交易對列表（逗號分隔）
    #[arg(long, default_value = "BTCUSDT,ETHUSDT")]
    symbols: String,

    /// 深度模式 (limited/incremental/both)
    #[arg(long, default_value = "limited")]
    depth_mode: String,

    /// 深度檔位數
    #[arg(long, default_value = "15")]
    depth_levels: usize,

    /// ClickHouse URL
    #[arg(
        long,
        default_value = "https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
    )]
    ch_url: String,

    /// ClickHouse 資料庫
    #[arg(long, default_value = "hft_db")]
    database: String,

    /// 批量大小
    #[arg(long, default_value = "1000")]
    batch_size: usize,

    /// Flush 間隔（毫秒）
    #[arg(long, default_value = "2000")]
    flush_ms: u64,

    /// Dry-run 模式
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    // 解析交易所列表
    let exchange_names: Vec<String> = args
        .exchanges
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    // 解析交易對列表
    let symbols: Vec<String> = args
        .symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    // ClickHouse 配置
    let ch_user =
        std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string());
    let ch_password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "".to_string());

    db::init_db_config(db::DbConfig::clickhouse(
        args.ch_url.clone(),
        args.database.clone(),
        ch_user.clone(),
        ch_password.clone(),
        args.dry_run,
    ));

    // dry-run 模式提示
    if args.dry_run {
        info!("🔬 Dry-run 模式：跳過 ClickHouse 連接");
    }

    // 為每個交易所啟動數據收集任務
    let mut tasks = vec![];

    for exchange_name in &exchange_names {
        let exchange_name = exchange_name.clone();
        let symbols = symbols.clone();
        let depth_mode = DepthMode::from_str(&args.depth_mode);
        let depth_levels = args.depth_levels;
        let ch_url = args.ch_url.clone();
        let database = args.database.clone();
        let ch_user = ch_user.clone();
        let ch_password = ch_password.clone();
        let batch_size = args.batch_size;
        let flush_ms = args.flush_ms;
        let dry_run = args.dry_run;

        let task = tokio::spawn(async move {
            if let Err(e) = run_exchange_collector(
                exchange_name.clone(),
                symbols,
                depth_mode,
                depth_levels,
                database,
                ch_url,
                ch_user,
                ch_password,
                batch_size,
                flush_ms,
                dry_run,
            )
            .await
            {
                error!("交易所 {} 收集器失敗: {:?}", exchange_name, e);
            }
        });

        tasks.push(task);
    }

    // 等待所有任務完成
    futures::future::join_all(tasks).await;

    Ok(())
}

async fn run_exchange_collector(
    exchange_name: String,
    symbols: Vec<String>,
    depth_mode: DepthMode,
    depth_levels: usize,
    database: String,
    ch_url: String,
    ch_user: String,
    ch_password: String,
    batch_size: usize,
    flush_ms: u64,
    dry_run: bool,
) -> Result<()> {
    // 創建交易所上下文
    let ctx = Arc::new(ExchangeContext::new(
        depth_mode,
        depth_levels,
        Some(Arc::new(symbols.clone())),
    ));

    // 創建交易所實例
    let exchange = create_exchange(&exchange_name, ctx)?;

    info!(
        "啟動 {} 收集器: {:?} (深度模式: {:?}, 檔位: {})",
        exchange.name(),
        symbols,
        depth_mode,
        depth_levels
    );

    // 確保資料庫與表存在（IF NOT EXISTS，僅缺失時創建）
    if !dry_run {
        let mut client = Client::default().with_url(&ch_url);
        if !ch_password.is_empty() {
            client = client.with_user(&ch_user).with_password(&ch_password);
        }
        client
            .query(&format!("CREATE DATABASE IF NOT EXISTS {}", database))
            .execute()
            .await?;
        // 使用運行時 exchange 執行 setup_tables（與實際運行配置一致）
        exchange.setup_tables(&client, &database).await?;
    }

    // 獲取 WebSocket 連接計劃
    // 穩定運行：自動重連 + 指數退避
    let mut backoff_secs: u64 = 1;
    let max_backoff: u64 = 60;
    let futures_enabled = exchange.requires_futures_buffers();
    loop {
        let plan = match exchange.websocket_plan(&symbols).await {
            Ok(p) => p,
            Err(e) => {
                error!("生成訂閱計劃失敗: {:?}", e);
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(max_backoff);
                continue;
            }
        };
        let ws_url = plan.url;
        info!("連接 WebSocket: {}", ws_url);

        // 獲取自定義 headers
        let custom_headers = exchange.websocket_headers();

        // 如果有自定義 headers，需要用 Request + 手動添加 WebSocket headers
        let connect_res = if !custom_headers.is_empty() {
            let mut req = Request::builder()
                .uri(&ws_url)
                .header("Host", ws_url.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or(""))
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13");

            // 生成 WebSocket key
            let key = {
                use base64::Engine;
                let mut nonce = [0u8; 16];
                rand::thread_rng().fill(&mut nonce);
                base64::engine::general_purpose::STANDARD.encode(&nonce)
            };
            req = req.header("Sec-WebSocket-Key", key);

            // 添加自定義 headers
            for (k, v) in custom_headers {
                req = req.header(k, v);
            }

            let req = match req.body(()) {
                Ok(r) => r,
                Err(e) => {
                    error!("構建 WebSocket 請求失敗: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(max_backoff);
                    continue;
                }
            };
            connect_async(req).await
        } else {
            // 沒有自定義 headers，直接用 URL（讓庫自動處理）
            connect_async(&ws_url).await
        };
        let (mut ws_sender, mut ws_receiver) = match connect_res {
            Ok((ws_stream, _)) => {
                backoff_secs = 1; // reset backoff on successful connect
                ws_stream.split()
            }
            Err(e) => {
                error!("連接 WebSocket 失敗: {:?}", e);
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(max_backoff);
                continue;
            }
        };

        for msg in plan.subscribe_messages {
            info!("發送訂閱消息: {}", msg);
            if let Err(e) = ws_sender.send(Message::Text(msg.clone())).await {
                error!("發送訂閱消息失敗: {:?}", e);
            }
        }

        let mut buffers = MessageBuffers::new(futures_enabled);
        let mut heartbeat_timer = interval(exchange.heartbeat_interval());
        let mut flush_timer = interval(Duration::from_millis(flush_ms));

        // 內層讀寫循環；若出現錯誤/關閉則跳出，外層進行重連
        loop {
            tokio::select! {
                Some(msg) = ws_receiver.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Err(e) = exchange.process_message(&text, &mut buffers).await {
                                warn!("處理消息失敗: {:?}", e);
                            }
                            if buffers.len() >= batch_size {
                                if !dry_run {
                                    if let Err(e) = exchange.flush_data(&database, &mut buffers).await {
                                        error!("Flush 失敗: {:?}", e);
                                    }
                                } else {
                                    info!("📦 [Dry-run] 緩衝區達到 {} 條", buffers.len());
                                    buffers = MessageBuffers::new(futures_enabled);
                                }
                            }
                        }
                        Ok(Message::Binary(_)) => {}
                        Ok(Message::Ping(payload)) => {
                            if let Err(e) = ws_sender.send(Message::Pong(payload)).await { error!("Pong 失敗: {:?}", e); break; }
                        }
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(_)) => { warn!("WebSocket 連接關閉"); break; }
                        Err(e) => { error!("WebSocket 錯誤: {:?}", e); break; }
                        _ => {}
                    }
                }
                _ = heartbeat_timer.tick() => {
                    if let Err(e) = ws_sender.send(Message::Ping(vec![])).await { error!("心跳失敗: {:?}", e); break; }
                }
                _ = flush_timer.tick() => {
                    if buffers.len() > 0 && !dry_run {
                        if let Err(e) = exchange.flush_data(&database, &mut buffers).await {
                            error!("定期 Flush 失敗: {:?}", e);
                        }
                    }
                }
            }
        }

        // 斷線後等待退避時間再重連
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(max_backoff);
    }
}
