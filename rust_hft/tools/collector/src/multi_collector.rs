use crate::db::{self, DbConfig};
use crate::exchanges::{create_exchange, DepthMode, ExchangeContext, MessageBuffers};
use crate::metrics::{
    BATCH_INSERT_SECONDS, ERRORS_BY_KIND_TOTAL, LAST_ERROR_KIND, RECONNECTS_TOTAL,
};
use crate::resolve_max_backoff_secs;
use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use clap::Parser;
use clickhouse::Client;
use flate2::read::{GzDecoder, ZlibDecoder};
use futures::{SinkExt, StreamExt};
use hyper::header::{HeaderName, HeaderValue, HOST};
use hyper::http::Request as HttpRequest;
#[cfg(feature = "collector-hyperliquid")]
use hyperliquid_rust_sdk as hl;
use rand::RngCore;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::interval;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// 多交易所统一数据收集器
#[derive(Parser, Debug)]
#[command(author, version, about = "多交易所统一数据收集器", long_about = None)]
pub struct MultiCollectorArgs {
    /// 交易所名称 (binance, binance_futures, bitget, okx, bybit, asterdex, hyperliquid)
    #[arg(long)]
    pub exchange: String,

    /// ClickHouse 服务器 URL
    #[arg(
        long,
        default_value = "https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
    )]
    pub ch_url: String,

    /// ClickHouse 数据库名称
    #[arg(long, default_value = "hft_db")]
    pub database: String,

    /// Top N 币种限制
    #[arg(long, default_value = "20")]
    pub top_limit: usize,

    /// 指定 JSON 白名單檔案路徑（優先於預設值）
    #[arg(long)]
    pub symbols_file: Option<PathBuf>,

    /// 分片索引（用于多實例分段采集）：0..shard_count-1
    #[arg(long, default_value_t = 0)]
    pub shard_index: usize,

    /// 分片總數（用于多實例分段采集）
    #[arg(long, default_value_t = 1)]
    pub shard_count: usize,

    /// 批量插入大小
    #[arg(long, default_value = "1000")]
    pub batch_size: usize,

    /// 刷新时间间隔 (毫秒)
    #[arg(long, default_value = "2000")]
    pub flush_ms: u64,

    /// 乾跑模式：只解析不寫庫
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// 深度模式：limited | incremental | both
    #[arg(long, default_value = "limited")]
    pub depth_mode: String,

    /// 有限檔深度層數（5/10/20）
    #[arg(long, default_value_t = 20)]
    pub depth_levels: usize,

    /// 是否存儲原始 WS 負載到 raw_ws 表
    #[arg(long, default_value_t = false)]
    pub store_raw: bool,

    /// LOB 寫入模式：snapshot|row|both
    #[arg(long, default_value = "snapshot")]
    pub lob_mode: String,

    /// Prometheus 指標服務位址（例如 0.0.0.0:9100），僅 ms 子命令使用
    #[arg(long)]
    pub metrics_addr: Option<String>,

    /// 是否收集資金費率（Perp 合約），僅 ms 子命令使用
    #[arg(long, default_value_t = false)]
    pub collect_funding: bool,

    /// Snapshot 批次大小（MarketStream 專用）
    #[arg(long, default_value_t = 1000)]
    pub snapshot_batch: usize,

    /// Trade 批次大小（MarketStream 專用）
    #[arg(long, default_value_t = 1000)]
    pub trade_batch: usize,

    /// Raw WS 批次大小（MarketStream 專用）
    #[arg(long, default_value_t = 1000)]
    pub raw_batch: usize,

    /// L2 行寫入批次大小（MarketStream 專用）
    #[arg(long, default_value_t = 5000)]
    pub l2_batch: usize,

    /// 資金費率批次大小（MarketStream 專用）
    #[arg(long, default_value_t = 500)]
    pub funding_batch: usize,
}

fn decode_ws_binary(payload: &[u8]) -> Option<String> {
    if payload.is_empty() {
        return None;
    }

    if let Ok(text) = std::str::from_utf8(payload) {
        return Some(text.to_string());
    }

    let decode_with = |mut reader: Box<dyn Read>| -> Option<String> {
        let mut out = String::new();
        if reader.read_to_string(&mut out).is_ok() {
            Some(out)
        } else {
            None
        }
    };

    if let Some(text) = decode_with(Box::new(GzDecoder::new(std::io::Cursor::new(payload)))) {
        return Some(text);
    }

    if let Some(text) = decode_with(Box::new(ZlibDecoder::new(std::io::Cursor::new(payload)))) {
        return Some(text);
    }

    None
}

pub async fn run_multi_collector(args: MultiCollectorArgs) -> Result<()> {
    // 先創建 exchange，再使用其 name() 進行日誌輸出，避免未使用方法警告
    let depth_mode = DepthMode::from_str(&args.depth_mode);
    let symbols_override = crate::exchanges::parse_symbols_override().map(Arc::new);
    let exchange_ctx = Arc::new(ExchangeContext::new(
        depth_mode,
        args.depth_levels,
        symbols_override.clone(),
    ));
    let exchange = create_exchange(&args.exchange, Arc::clone(&exchange_ctx))?;
    let futures_enabled = exchange.requires_futures_buffers();
    info!("启动 {} 数据收集器", exchange.name());

    // 初始化 ClickHouse 客户端
    // 將 ClickHouse 參數寫入環境，方便下游使用（如 HTTP 插入）
    if args.dry_run {
        tracing::warn!("啟用 dry-run：只解析，不寫入資料庫");
    }

    if let Some(path) = args.symbols_file.as_ref() {
        std::env::set_var("SYMBOLS_FILE", path);
        info!("使用外部 JSON 白名單: {}", path.display());
    }
    let ch_user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string());
    let ch_password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "".to_string());

    db::init_db_config(DbConfig::clickhouse(
        args.ch_url.clone(),
        args.database.clone(),
        ch_user.clone(),
        ch_password.clone(),
        args.dry_run,
    ));

    // 可选启动 metrics/health 端点（用于 k8s 探针）
    if let Some(addr) = args.metrics_addr.clone() {
        let _ = crate::metrics::start_metrics_server(addr).await;
    }

    let mut client = Client::default().with_url(&args.ch_url);
    if !ch_password.is_empty() {
        client = client.with_user(&ch_user).with_password(&ch_password);
    }

    let db_sink = db::get_sink_async(&args.database).await?;

    if !args.dry_run {
        // 创建数据库
        let create_db_query = format!("CREATE DATABASE IF NOT EXISTS {}", args.database);
        client.query(&create_db_query).execute().await?;

        // 通用 LOB 快照表（供各交易所视图复用）
        // 以微秒時間戳存儲，適合高頻快照；各交易所可建立視圖做型別轉換
        let create_snapshot_books = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.snapshot_books (
                ts UInt64,                           -- 快照時間戳 (微秒)
                symbol String,                       -- 交易對符號
                venue String,                        -- 交易所
                last_id UInt64,                      -- 最後更新 ID / 序列號
                bids_px Array(Float64),              -- 買盤價格
                bids_qty Array(Float64),             -- 買盤數量
                asks_px Array(Float64),              -- 賣盤價格
                asks_qty Array(Float64),             -- 賣盤數量
                source String                        -- 來源 (REST/WS/ANCHOR)
            )
            ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            SETTINGS index_granularity = 8192
            "#,
            args.database
        );
        client.query(&create_snapshot_books).execute().await?;

        // 设置表结构
        exchange.setup_tables(&client, &args.database).await?;

        // 可選：原始 WS 表
        if args.store_raw {
            let create_raw = format!(
                r#"
                CREATE TABLE IF NOT EXISTS {}.raw_ws (
                    ts DateTime64(3),
                    exchange LowCardinality(String),
                    payload String
                ) ENGINE = MergeTree()
                ORDER BY (exchange, ts)
                "#,
                args.database
            );
            client.query(&create_raw).execute().await?;
        }
    }

    // 获取热门交易对
    let symbols = if let Some(override_syms) = symbols_override.clone() {
        (*override_syms).clone()
    } else {
        exchange.get_popular_symbols(args.top_limit).await?
    };
    // 分片過濾（如啟用）
    let symbols = if args.shard_count > 1 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let filtered: Vec<String> = symbols
            .into_iter()
            .filter(|s| {
                let mut h = DefaultHasher::new();
                s.hash(&mut h);
                let idx = (h.finish() % (args.shard_count as u64)) as usize;
                idx == args.shard_index
            })
            .collect();
        filtered
    } else {
        symbols
    };
    if symbols.is_empty() {
        warn!("selected symbols list is empty; check SYMBOLS_FILE or whitelist settings");
    }
    info!("选择的交易对: {:?}", symbols);

    // 数据缓冲区
    let mut buffers = MessageBuffers::new(futures_enabled);

    // 构建 WebSocket 计划（由交易所自行决定 URL / 订阅）
    let exchange_name = exchange.name();
    let mut ws_plan = exchange.websocket_plan(&symbols).await?;

    // 定时刷新任务（常驻）
    let client_clone = client.clone();
    let database_clone = args.database.clone();
    let exchange_clone = create_exchange(&args.exchange, Arc::clone(&exchange_ctx))?;
    let batch_size = args.batch_size;
    let flush_interval = Duration::from_millis(args.flush_ms);

    let (flush_tx, mut flush_rx) = tokio::sync::mpsc::channel::<MessageBuffers>(500);
    let db_sink_clone = Arc::clone(&db_sink);

    tokio::spawn(async move {
        let mut flush_timer = interval(flush_interval);
        loop {
            tokio::select! {
                _ = flush_timer.tick() => {
                    // 定时刷新信号
                }
                data = flush_rx.recv() => {
                    if let Some(mut buffers) = data {
                        // 添加诊断日志
                        let total_size = buffers.len();
                        if total_size > 0 {
                            let spot = &buffers.spot;
                            let (fut_ob, fut_trades, fut_ticker, fut_l1) = buffers
                                .futures_ref()
                                .map(|f| (f.orderbook.len(), f.trades.len(), f.ticker.len(), f.l1.len()))
                                .unwrap_or((0, 0, 0, 0));
                            debug!(
                                "准备刷新 {} 条数据 (spot_ob: {}, spot_trades: {}, spot_ticker: {}, futures_ob: {}, futures_trades: {}, futures_ticker: {}, futures_l1: {}, snapshots: {})",
                                total_size,
                                spot.orderbook.len(),
                                spot.trades.len(),
                                spot.ticker.len(),
                                fut_ob,
                                fut_trades,
                                fut_ticker,
                                fut_l1,
                                spot.snapshots.len()
                            );
                        }
                        let t0 = std::time::Instant::now();
                        let res = exchange_clone.flush_data(&client_clone, &database_clone, &mut buffers).await;
                        let dt = t0.elapsed().as_secs_f64();
                        BATCH_INSERT_SECONDS.with_label_values(&[exchange_clone.name(), "unified"]).observe(dt);
                        if let Err(e) = res {
                            ERRORS_BY_KIND_TOTAL.with_label_values(&[exchange_clone.name(), "flush_error"]).inc();
                            LAST_ERROR_KIND.with_label_values(&[exchange_clone.name(), "flush_error"]).set(chrono::Utc::now().timestamp() as i64);
                            error!("刷新数据失败: {}", e);
                        } else {
                            debug!("成功刷新 {} 条数据，耗时 {:.3}s", total_size, dt);
                        }
                        if !buffers.raw_data.is_empty() {
                            let t1 = std::time::Instant::now();
                            if let Err(e) = db_sink_clone.insert_json_rows("raw_ws", &buffers.raw_data).await {
                                ERRORS_BY_KIND_TOTAL.with_label_values(&[exchange_clone.name(), "raw_flush_error"]).inc();
                                LAST_ERROR_KIND.with_label_values(&[exchange_clone.name(), "raw_flush_error"]).set(chrono::Utc::now().timestamp() as i64);
                                error!("raw_ws 插入失败: {}", e);
                            }
                            let dt = t1.elapsed().as_secs_f64();
                            BATCH_INSERT_SECONDS.with_label_values(&[exchange_clone.name(), "raw_ws"]).observe(dt);
                            buffers.raw_data.clear();
                        }
                    }
                }
            }
        }
    });

    // 如果是 Hyperliquid 且启用 SDK 路径，走专用直连 SDK 流程
    #[cfg(feature = "collector-hyperliquid")]
    if args.exchange.to_lowercase() == "hyperliquid"
        && std::env::var("HL_SDK").unwrap_or_else(|_| "1".to_string()) != "0"
    {
        return run_hl_via_sdk(args, symbols).await;
    }

    // 自動重連循環（默认通用路径）
    let mut connected_once = false;
    let mut heartbeat_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(resolve_max_backoff_secs(exchange_name));
    loop {
        let plan_result = exchange.websocket_plan(&symbols).await;
        let plan = match plan_result {
            Ok(plan) => {
                ws_plan = plan.clone();
                plan
            }
            Err(err) => {
                warn!("生成 WebSocket 計劃失敗: {}，使用上一個配置", err);
                ws_plan.clone()
            }
        };
        let ws_url = plan.url.clone();
        let subscribe_messages = plan.subscribe_messages.clone();
        info!("连接到 {} WebSocket: {}", args.exchange, ws_url);

        // 构建自定义握手请求（直连方案）：显式设置 Upgrade 头与 Sec-WebSocket-*，并允许按需追加自定义头
        let mut req_builder = HttpRequest::builder().method("GET").uri(&ws_url);
        // Host 头
        if let Some(host) = ws_url
            .splitn(2, "://")
            .nth(1)
            .and_then(|s| s.split('/').next())
        {
            req_builder = req_builder.header(HOST, host);
        }
        // 基本 WebSocket 升级头
        req_builder = req_builder
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13");
        // 随机 Sec-WebSocket-Key
        let mut key_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut key_bytes);
        let swk = general_purpose::STANDARD.encode(key_bytes);
        req_builder = req_builder.header("Sec-WebSocket-Key", swk);

        // 交易所自定义头（如 Origin 等）
        for (k, v) in exchange.websocket_headers() {
            if let (Ok(hk), Ok(hv)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(&v),
            ) {
                req_builder = req_builder.header(hk, hv);
            }
        }
        let request = req_builder
            .body(())
            .map_err(|e| anyhow::anyhow!("build ws request failed: {}", e))?;

        match connect_async(request).await {
            Ok((ws_stream, _)) => {
                if let Some(h) = heartbeat_handle.take() {
                    h.abort();
                }
                let (ws_sender, mut ws_receiver) = ws_stream.split();
                let ws_sender = Arc::new(Mutex::new(ws_sender));
                if connected_once {
                    RECONNECTS_TOTAL.with_label_values(&[exchange_name]).inc();
                }
                connected_once = true;
                backoff = Duration::from_secs(1);

                // 讓各 exchange 提供訂閱消息（如需要）
                for payload in subscribe_messages {
                    let s = Arc::clone(&ws_sender);
                    let res = {
                        let mut guard = s.lock().await;
                        guard.send(Message::Text(payload)).await
                    };
                    if let Err(e) = res {
                        error!("發送訂閱失敗: {}", e);
                    }
                }

                // 发送心跳（每次連線各自一個任務）
                let heartbeat_sender = Arc::clone(&ws_sender);
                let hb = exchange.heartbeat_interval();
                let ex_name_for_hb = exchange_name.to_string();
                let hb_handle = tokio::spawn(async move {
                    let mut interval = interval(hb);
                    loop {
                        interval.tick().await;
                        let mut sender = heartbeat_sender.lock().await;
                        if let Err(e) = sender.send(Message::Ping(vec![])).await {
                            ERRORS_BY_KIND_TOTAL
                                .with_label_values(&[&ex_name_for_hb, "heartbeat_error"])
                                .inc();
                            LAST_ERROR_KIND
                                .with_label_values(&[&ex_name_for_hb, "heartbeat_error"])
                                .set(chrono::Utc::now().timestamp() as i64);
                            debug!("发送心跳失败: {}", e);
                            break;
                        }
                        // JSON心跳：部分交易所需要
                        match ex_name_for_hb.as_str() {
                            "bybit" => {
                                let _ = sender
                                    .send(Message::Text("{\"op\":\"ping\"}".to_string()))
                                    .await;
                            }
                            "hyperliquid" => {
                                let _ = sender
                                    .send(Message::Text("{\"method\":\"ping\"}".to_string()))
                                    .await;
                            }
                            "bitget" => {
                                let _ = sender
                                    .send(Message::Text("{\"op\":\"ping\"}".to_string()))
                                    .await;
                            }
                            _ => {}
                        }
                    }
                });
                heartbeat_handle = Some(hb_handle);

                // 主数据处理循环
                let mut last_flush = tokio::time::Instant::now();
                let flush_duration = Duration::from_millis(args.flush_ms);

                while let Some(msg) = ws_receiver.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if exchange_name == "bitget" && text.contains("\"op\":\"ping\"") {
                                let mut sender = ws_sender.lock().await;
                                if let Err(e) = sender
                                    .send(Message::Text("{\"op\":\"pong\"}".to_string()))
                                    .await
                                {
                                    warn!("Bitget pong 回應失敗: {}", e);
                                    break;
                                }
                                continue;
                            }
                            // 可選：存 raw 負載
                            if args.store_raw {
                                let now = chrono::Utc::now();
                                let raw_entry = serde_json::json!({
                                    "ts": now,
                                    "exchange": exchange.name(),
                                    "payload": &text,
                                });
                                buffers.raw_data.push(serde_json::to_string(&raw_entry)?);
                            }
                            if let Err(e) = exchange.process_message(&text, &mut buffers).await {
                                ERRORS_BY_KIND_TOTAL
                                    .with_label_values(&[exchange_name, "process_error"])
                                    .inc();
                                LAST_ERROR_KIND
                                    .with_label_values(&[exchange_name, "process_error"])
                                    .set(chrono::Utc::now().timestamp() as i64);
                                warn!("处理消息失败: {}", e);
                            }
                            if (buffers.len() >= batch_size)
                                || (last_flush.elapsed() >= flush_duration)
                            {
                                if buffers.len() > 0 {
                                    let futures_enabled = buffers.futures.is_some();
                                    let buffers_to_flush = std::mem::replace(
                                        &mut buffers,
                                        MessageBuffers::new(futures_enabled),
                                    );
                                    if let Err(e) = flush_tx.try_send(buffers_to_flush) {
                                        match e {
                                            tokio::sync::mpsc::error::TrySendError::Full(buf) => {
                                                warn!(
                                                    "刷新通道满，等待释放后继续 (exchange={})",
                                                    exchange_name
                                                );
                                                if let Err(send_err) = flush_tx.send(buf).await {
                                                    error!("刷新通道发送失败: {}", send_err);
                                                    break;
                                                }
                                            }
                                            tokio::sync::mpsc::error::TrySendError::Closed(buf) => {
                                                buffers = buf;
                                                error!("刷新通道已关闭");
                                                break;
                                            }
                                        }
                                    }
                                }
                                last_flush = tokio::time::Instant::now();
                            }
                        }
                        Ok(Message::Binary(bin)) => {
                            if let Some(text) = decode_ws_binary(&bin) {
                                if args.store_raw {
                                    let now = chrono::Utc::now();
                                    let raw_entry = serde_json::json!({
                                        "ts": now,
                                        "exchange": exchange.name(),
                                        "payload": &text,
                                    });
                                    buffers.raw_data.push(serde_json::to_string(&raw_entry)?);
                                }
                                if let Err(e) = exchange.process_message(&text, &mut buffers).await
                                {
                                    ERRORS_BY_KIND_TOTAL
                                        .with_label_values(&[exchange_name, "process_error"])
                                        .inc();
                                    LAST_ERROR_KIND
                                        .with_label_values(&[exchange_name, "process_error"])
                                        .set(chrono::Utc::now().timestamp() as i64);
                                    warn!("处理消息失败: {}", e);
                                }
                                if (buffers.len() >= batch_size)
                                    || (last_flush.elapsed() >= flush_duration)
                                {
                                    if buffers.len() > 0 {
                                        let futures_enabled = buffers.futures.is_some();
                                        let buffers_to_flush = std::mem::replace(
                                            &mut buffers,
                                            MessageBuffers::new(futures_enabled),
                                        );
                                        if let Err(e) = flush_tx.try_send(buffers_to_flush) {
                                            match e {
                                                tokio::sync::mpsc::error::TrySendError::Full(
                                                    buf,
                                                ) => {
                                                    warn!(
                                                        "刷新通道满，等待释放后继续 (exchange={})",
                                                        exchange_name
                                                    );
                                                    if let Err(send_err) = flush_tx.send(buf).await
                                                    {
                                                        error!("刷新通道发送失败: {}", send_err);
                                                        break;
                                                    }
                                                }
                                                tokio::sync::mpsc::error::TrySendError::Closed(
                                                    buf,
                                                ) => {
                                                    buffers = buf;
                                                    error!("刷新通道已关闭");
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    last_flush = tokio::time::Instant::now();
                                }
                            } else {
                                debug!("无法解析的二进制消息 ({} bytes)", bin.len());
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            let mut sender = ws_sender.lock().await;
                            if let Err(e) = sender.send(Message::Pong(payload)).await {
                                ERRORS_BY_KIND_TOTAL
                                    .with_label_values(&[exchange_name, "pong_error"])
                                    .inc();
                                LAST_ERROR_KIND
                                    .with_label_values(&[exchange_name, "pong_error"])
                                    .set(chrono::Utc::now().timestamp() as i64);
                                warn!("发送 Pong 失败: {}", e);
                                break;
                            }
                            // Bitget 期望 JSON pong，額外發送
                            if exchange_name == "bitget" {
                                if let Err(e) = sender
                                    .send(Message::Text("{\"op\":\"pong\"}".to_string()))
                                    .await
                                {
                                    warn!("发送 JSON pong 失败: {}", e);
                                }
                            }
                        }
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(_)) => {
                            warn!("WebSocket 连接关闭");
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            ERRORS_BY_KIND_TOTAL
                                .with_label_values(&[exchange_name, "ws_error"])
                                .inc();
                            LAST_ERROR_KIND
                                .with_label_values(&[exchange_name, "ws_error"])
                                .set(chrono::Utc::now().timestamp() as i64);
                            warn!("WebSocket 错误: {}", e);
                            break;
                        }
                    }
                }

                // 刷新剩余数据
                if buffers.len() > 0 {
                    let futures_enabled = buffers.futures.is_some();
                    let remaining =
                        std::mem::replace(&mut buffers, MessageBuffers::new(futures_enabled));
                    if let Err(e) = flush_tx.try_send(remaining) {
                        match e {
                            tokio::sync::mpsc::error::TrySendError::Full(buf) => {
                                if let Err(send_err) = flush_tx.send(buf).await {
                                    error!("刷新通道发送失败: {}", send_err);
                                }
                            }
                            tokio::sync::mpsc::error::TrySendError::Closed(buf) => {
                                buffers = buf;
                                error!("刷新通道已关闭");
                            }
                        }
                    }
                }
                if let Some(h) = heartbeat_handle.take() {
                    h.abort();
                }
                warn!("断开连接，{:.0?} 后重连...", backoff);
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            Err(e) => {
                ERRORS_BY_KIND_TOTAL
                    .with_label_values(&[exchange_name, "connect_failed"])
                    .inc();
                LAST_ERROR_KIND
                    .with_label_values(&[exchange_name, "connect_failed"])
                    .set(chrono::Utc::now().timestamp() as i64);
                error!("连接失败: {}，{:.0?} 后重试", e, backoff);
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
        }
    }
}

#[cfg(feature = "collector-hyperliquid")]
async fn run_hl_via_sdk(args: MultiCollectorArgs, symbols: Vec<String>) -> Result<()> {
    use crate::exchanges::{create_exchange, DepthMode, ExchangeContext};
    use hl::{BaseUrl, InfoClient, Message as HlMsg, Subscription};
    use std::sync::Arc;
    use tokio::sync::mpsc::unbounded_channel;

    // 选择主网/测试网
    let base = std::env::var("HYPERLIQUID_WS_URL")
        .unwrap_or_else(|_| "wss://api.hyperliquid.xyz/ws".to_string());
    let base_url = if base.contains("testnet") {
        BaseUrl::Testnet
    } else {
        BaseUrl::Mainnet
    };

    // InfoClient 封装了 WsManager（公开 API）
    let mut info_client = InfoClient::with_reconnect(None, Some(base_url))
        .await
        .map_err(|e| anyhow::anyhow!("hl InfoClient init failed: {}", e))?;

    // 统一接收通道
    let (tx, mut rx) = unbounded_channel::<HlMsg>();

    // 订阅集：l2Book + trades（同一 sender）
    for s in &symbols {
        let coin = if let Some(pos) = s.find('-') {
            &s[..pos]
        } else {
            s
        };
        info_client
            .subscribe(
                Subscription::L2Book {
                    coin: coin.to_string(),
                },
                tx.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("hl sub L2 error: {}", e))?;
        info_client
            .subscribe(
                Subscription::Trades {
                    coin: coin.to_string(),
                },
                tx.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("hl sub Trades error: {}", e))?;
    }
    if std::env::var("HL_ALL_MIDS").unwrap_or_else(|_| "0".to_string()) == "1" {
        let _ = info_client
            .subscribe(Subscription::AllMids, tx.clone())
            .await;
    }

    // 构建 ClickHouse 客户端 & exchange 写入实现
    let depth_mode = DepthMode::from_str(&args.depth_mode);
    let symbols_override = crate::exchanges::parse_symbols_override().map(Arc::new);
    let exchange_ctx = Arc::new(ExchangeContext::new(
        depth_mode,
        args.depth_levels,
        symbols_override,
    ));
    let exchange = create_exchange(&args.exchange, Arc::clone(&exchange_ctx))?;
    let mut buffers = crate::exchanges::MessageBuffers::new(false);
    let client = clickhouse::Client::default().with_url(&args.ch_url);
    // 定时刷新
    let flush_interval = std::time::Duration::from_millis(args.flush_ms);
    let mut last_flush = tokio::time::Instant::now();

    // 消费 SDK 消息并落库
    while let Some(msg) = rx.recv().await {
        match msg {
            HlMsg::Trades(t) => {
                for tr in t.data {
                    if let (Ok(px), Ok(sz)) = (tr.px.parse::<f64>(), tr.sz.parse::<f64>()) {
                        let ts = chrono::Utc::now(); // SDK给 time(u64 ms) 也可用 tr.time
                        let row = serde_json::json!({
                            "ts": ts,
                            "symbol": format!("{}-USDT", tr.coin),
                            "trade_id": tr.tid.to_string(),
                            "price": px,
                            "qty": sz,
                            "side": if tr.side.to_lowercase().starts_with('b'){"buy"} else {"sell"},
                        });
                        buffers.push_spot_trades(serde_json::to_string(&row)?);
                    }
                }
            }
            HlMsg::L2Book(book) => {
                let ts = chrono::Utc::now(); // book.data.time 可用
                if book.data.levels.len() >= 2 {
                    let bids = &book.data.levels[0];
                    let asks = &book.data.levels[1];
                    let mut best_bid: Option<(f64, f64)> = None;
                    let mut best_ask: Option<(f64, f64)> = None;
                    for bl in bids {
                        if let (Ok(px), Ok(sz)) = (bl.px.parse::<f64>(), bl.sz.parse::<f64>()) {
                            if sz > 0.0 {
                                let row = serde_json::json!({
                                    "ts": ts,
                                    "symbol": format!("{}-USDT", book.data.coin),
                                    "side": "bid",
                                    "price": px,
                                    "qty": sz,
                                    "update_id": 0,
                                });
                                buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                                if best_bid.map(|(p, _)| px > p).unwrap_or(true) {
                                    best_bid = Some((px, sz));
                                }
                            }
                        }
                    }
                    for al in asks {
                        if let (Ok(px), Ok(sz)) = (al.px.parse::<f64>(), al.sz.parse::<f64>()) {
                            if sz > 0.0 {
                                let row = serde_json::json!({
                                    "ts": ts,
                                    "symbol": format!("{}-USDT", book.data.coin),
                                    "side": "ask",
                                    "price": px,
                                    "qty": sz,
                                    "update_id": 0,
                                });
                                buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                                if best_ask.map(|(p, _)| px < p).unwrap_or(true) {
                                    best_ask = Some((px, sz));
                                }
                            }
                        }
                    }
                    if let (Some((bp, bq)), Some((ap, aq))) = (best_bid, best_ask) {
                        let l1 = serde_json::json!({
                            "ts": ts,
                            "symbol": format!("{}-USDT", book.data.coin),
                            "bid_px": bp,
                            "bid_qty": bq,
                            "ask_px": ap,
                            "ask_qty": aq,
                        });
                        buffers.push_spot_l1(serde_json::to_string(&l1)?);
                    }
                }
            }
            HlMsg::AllMids(_)
            | HlMsg::SubscriptionResponse
            | HlMsg::Pong
            | HlMsg::HyperliquidError(_)
            | HlMsg::Notification(_)
            | HlMsg::Candle(_)
            | HlMsg::OrderUpdates(_)
            | HlMsg::User(_)
            | HlMsg::UserFills(_)
            | HlMsg::UserFundings(_)
            | HlMsg::UserNonFundingLedgerUpdates(_)
            | HlMsg::WebData2(_)
            | HlMsg::ActiveAssetCtx(_)
            | HlMsg::NoData => {}
        }

        if buffers.len() >= args.batch_size || last_flush.elapsed() >= flush_interval {
            if args.dry_run {
                // Dry-run: 不落库，仅清空缓冲并打印计数
                tracing::info!(
                    "[HL SDK dry-run] flush: ob={}, trades={}, l1={}",
                    buffers.spot.orderbook.len(),
                    buffers.spot.trades.len(),
                    buffers.spot.l1.len()
                );
                let futures_enabled = buffers.futures.is_some();
                buffers = crate::exchanges::MessageBuffers::new(futures_enabled);
            } else {
                let t0 = std::time::Instant::now();
                if let Err(e) = exchange
                    .flush_data(&client, &args.database, &mut buffers)
                    .await
                {
                    tracing::warn!("HL SDK flush failed: {}", e);
                } else {
                    let dt = t0.elapsed().as_secs_f64();
                    tracing::debug!("HL SDK flush ok in {:.3}s", dt);
                }
            }
            last_flush = tokio::time::Instant::now();
        }
    }
    Ok(())
}
