use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde_json::Value as JsonValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn, error};
use clickhouse::Client as ChClient;
use std::collections::HashMap;
use std::sync::Arc;
use chrono::{DateTime, Utc};
use crc32fast::Hasher as Crc32;
use rust_decimal::Decimal;
use serde::{Serialize, Deserialize};

mod top_symbols;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "USDT-FUTURES 專業數據收集器：四流同收，專業 schema", long_about = None)]
struct Args {
    /// ClickHouse URL（HTTP/HTTPS）
    #[arg(long, default_value = "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443")]
    ch_url: String,
    /// ClickHouse 資料庫
    #[arg(long, default_value = "hft")]
    database: String,
    /// 每批次寫入最大筆數
    #[arg(long, default_value_t = 5000)]
    batch_size: usize,
    /// Flush 週期（毫秒）
    #[arg(long, default_value_t = 1000)]
    flush_ms: u64,
    /// 獲取 TOP-N 合約（按 24h 成交量）
    #[arg(long, default_value_t = 30)]
    top_limit: usize,
}

// Integerization scales per (venue,symbol)
#[derive(Debug, Clone, Default)]
struct ScalesMap(Arc<HashMap<(String, String), (u64, u64)>>);

fn load_scales_from_yaml(path: &str) -> ScalesMap {
    let mut map: HashMap<(String, String), (u64, u64)> = HashMap::new();
    if let Ok(yaml) = std::fs::read_to_string(path) {
        if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&yaml) {
            if let Some(venues) = doc.get("venues").and_then(|v| v.as_mapping()) {
                for (venue_key, venue_val) in venues.iter() {
                    let venue = venue_key.as_str().unwrap_or("").to_uppercase();
                    if let Some(spot) = venue_val.get(&serde_yaml::Value::from("spot")) {
                        if let Some(symbols) = spot.as_mapping() {
                            for (sym_key, spec_val) in symbols.iter() {
                                let symbol = sym_key.as_str().unwrap_or("").to_uppercase();
                                let tick_size = spec_val.get(&serde_yaml::Value::from("tick_size")).and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let lot_size = spec_val.get(&serde_yaml::Value::from("lot_size")).and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let ps = pow10(decimal_places(tick_size));
                                let qs = pow10(decimal_places(lot_size));
                                map.insert((venue.clone(), symbol.clone()), (ps, qs));
                            }
                        }
                    }
                }
            }
        }
    }
    ScalesMap(Arc::new(map))
}

fn decimal_places(v: f64) -> u32 {
    let s = format!("{}", v);
    Decimal::from_str_exact(&s).map(|d| d.scale()).unwrap_or_else(|_| {
        let frac = s.split('.').nth(1).unwrap_or("");
        (frac.len() as u32).min(9)
    })
}

fn pow10(n: u32) -> u64 { 10u64.saturating_pow(n) }

fn quantize_i64(values: &[f64], scale: u64) -> Vec<i64> {
    let s = scale as f64;
    values.iter().map(|x| (x * s).round() as i64).collect()
}

#[derive(clickhouse::Row, Debug, serde::Serialize)]
struct RawEventRow {
    timestamp: u64,                // 本地接收微秒
    venue: String,
    channel: String,
    symbol: String,
    payload: String,
}

#[derive(clickhouse::Row, Debug, serde::Serialize)]
struct RawDepthBinanceRow {
    event_ts: u64,
    ingest_ts: u64,
    symbol: String,
    U: u64,
    u: u64,
    pu: u64,
    bids_px: Vec<f64>,
    bids_qty: Vec<f64>,
    asks_px: Vec<f64>,
    asks_qty: Vec<f64>,
    price_scale: u64,
    qty_scale: u64,
    bids_px_i64: Vec<i64>,
    bids_qty_i64: Vec<i64>,
    asks_px_i64: Vec<i64>,
    asks_qty_i64: Vec<i64>,
}

#[derive(clickhouse::Row, Debug, serde::Serialize)]
struct RawBooksBitgetRow {
    event_ts: u64,
    ingest_ts: u64,
    symbol: String,
    inst_type: String,
    channel: String,
    action: String,
    seq: u64,
    checksum: i64,
    bids_px: Vec<f64>,
    bids_qty: Vec<f64>,
    asks_px: Vec<f64>,
    asks_qty: Vec<f64>,
    // 原始字串（CRC 準確校驗用）
    bids_px_s: Vec<String>,
    bids_qty_s: Vec<String>,
    asks_px_s: Vec<String>,
    asks_qty_s: Vec<String>,
    price_scale: u64,
    qty_scale: u64,
    bids_px_i64: Vec<i64>,
    bids_qty_i64: Vec<i64>,
    asks_px_i64: Vec<i64>,
    asks_qty_i64: Vec<i64>,
}

#[derive(clickhouse::Row, Debug, serde::Serialize)]
struct RawTradesBitgetRow {
    event_ts: u64,
    ingest_ts: u64,
    symbol: String,
    inst_type: String,
    trade_id: String,
    price: f64,
    quantity: f64,
    side: String,
    price_i64: i64,
    qty_i64: i64,
    price_scale: u64,
    qty_scale: u64,
}

#[derive(clickhouse::Row, Debug, serde::Serialize)]
struct SnapshotBooksRow {
    ts: u64,
    symbol: String,
    venue: String,
    last_id: u64,
    bids_px: Vec<f64>,
    bids_qty: Vec<f64>,
    asks_px: Vec<f64>,
    asks_qty: Vec<f64>,
    source: String,
}

#[derive(clickhouse::Row, Debug, serde::Serialize)]
struct GapLogRow {
    ts: u64,
    venue: String,
    symbol: String,
    reason: String,
    detail: String,
}

// 簡單 LOB 狀態，用於 Anchor 快照
#[derive(Default)]
struct AnchorBook { bids: BTreeMap<OrderedFloat<f64>, f64>, asks: BTreeMap<OrderedFloat<f64>, f64> }
impl AnchorBook {
    fn apply_updates(&mut self, bids_px: &[f64], bids_qty: &[f64], asks_px: &[f64], asks_qty: &[f64]) {
        for (i, p) in bids_px.iter().enumerate() {
            let q = *bids_qty.get(i).unwrap_or(&0.0);
            let key = OrderedFloat(*p);
            if q == 0.0 { self.bids.remove(&key); } else { self.bids.insert(key, q); }
        }
        for (i, p) in asks_px.iter().enumerate() {
            let q = *asks_qty.get(i).unwrap_or(&0.0);
            let key = OrderedFloat(*p);
            if q == 0.0 { self.asks.remove(&key); } else { self.asks.insert(key, q); }
        }
    }
    fn top_n(&self, n: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut bpx = Vec::new(); let mut bqt = Vec::new();
        let mut apx = Vec::new(); let mut aqt = Vec::new();
        for (p, q) in self.bids.iter().rev().take(n) { bpx.push(p.0); bqt.push(*q); }
        for (p, q) in self.asks.iter().take(n) { apx.push(p.0); aqt.push(*q); }
        (bpx, bqt, apx, aqt)
    }
}

// Binance 橋接狀態
#[derive(Clone, Debug)]
struct BinanceBridgeState { bridged: bool, last_update_id: u64, last_u: u64 }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();

    // 準備 ClickHouse client 並建表
    let mut client = clickhouse::Client::default()
        .with_url(&args.ch_url)
        .with_database(&args.database);

    // 如果提供了用户名和密码，则添加认证
    if let Ok(user) = std::env::var("CLICKHOUSE_USER") {
        client = client.with_user(user);
    }
    if let Ok(password) = std::env::var("CLICKHOUSE_PASSWORD") {
        client = client.with_password(password);
    }

    // 載入精度定義，用於整數化（避免浮點誤差）
    let scales = load_scales_from_yaml("config/venue_specs.yaml");

    let create_sql = r#"
        CREATE TABLE IF NOT EXISTS raw_ws_events (
            timestamp UInt64,
            venue LowCardinality(String),
            channel LowCardinality(String),
            symbol String,
            payload String
        )
        ENGINE = MergeTree()
        ORDER BY (venue, channel, symbol, timestamp)
        TTL toDateTime(intDiv(timestamp, 1000000)) + INTERVAL 30 DAY
    "#;
    client.query(create_sql).execute().await?;
    info!("raw_ws_events 表已就緒");

    // 結構化表：用於重建/重放/校驗
    let create_struct = r#"
        CREATE TABLE IF NOT EXISTS raw_depth_binance (
            event_ts UInt64, ingest_ts UInt64, symbol String,
            U UInt64, u UInt64, pu UInt64,
            bids_px Array(Float64), bids_qty Array(Float64),
            asks_px Array(Float64), asks_qty Array(Float64),
            price_scale UInt64, qty_scale UInt64,
            bids_px_i64 Array(Int64), bids_qty_i64 Array(Int64),
            asks_px_i64 Array(Int64), asks_qty_i64 Array(Int64)
        ) ENGINE = MergeTree()
        ORDER BY (symbol, event_ts, u)
    "#; client.query(create_struct).execute().await?;
    let create_books = r#"
        CREATE TABLE IF NOT EXISTS raw_books_bitget (
            event_ts UInt64, ingest_ts UInt64, symbol String, inst_type String, channel String,
            action String, seq UInt64, checksum Int64,
            bids_px Array(Float64), bids_qty Array(Float64),
            asks_px Array(Float64), asks_qty Array(Float64),
            bids_px_s Array(String), bids_qty_s Array(String),
            asks_px_s Array(String), asks_qty_s Array(String),
            price_scale UInt64, qty_scale UInt64,
            bids_px_i64 Array(Int64), bids_qty_i64 Array(Int64),
            asks_px_i64 Array(Int64), asks_qty_i64 Array(Int64)
        ) ENGINE = MergeTree()
        ORDER BY (symbol, event_ts, seq)
    "#; client.query(create_books).execute().await?;
    // 升級兼容：若已存在表，補齊缺失欄位
    let _ = client.query("ALTER TABLE raw_books_bitget ADD COLUMN IF NOT EXISTS bids_px_s Array(String)").execute().await;
    let _ = client.query("ALTER TABLE raw_books_bitget ADD COLUMN IF NOT EXISTS bids_qty_s Array(String)").execute().await;
    let _ = client.query("ALTER TABLE raw_books_bitget ADD COLUMN IF NOT EXISTS asks_px_s Array(String)").execute().await;
    let _ = client.query("ALTER TABLE raw_books_bitget ADD COLUMN IF NOT EXISTS asks_qty_s Array(String)").execute().await;
    let create_snap = r#"
        CREATE TABLE IF NOT EXISTS snapshot_books (
            ts UInt64, symbol String, venue String, last_id UInt64,
            bids_px Array(Float64), bids_qty Array(Float64),
            asks_px Array(Float64), asks_qty Array(Float64),
            source String
        ) ENGINE = MergeTree()
        ORDER BY (symbol, ts)
    "#; client.query(create_snap).execute().await?;
    let create_gap = r#"
        CREATE TABLE IF NOT EXISTS gap_log (
            ts UInt64, venue String, symbol String, reason String, detail String
        ) ENGINE = MergeTree()
        ORDER BY (symbol, ts)
    "#; client.query(create_gap).execute().await?;
    info!("結構化表已就緒");

    let mut tasks = Vec::new();
    let venues: Vec<&str> = match args.venue.to_lowercase().as_str() {
        "binance" => vec!["binance"],
        "bitget" => vec!["bitget"],
        _ => vec!["binance", "bitget"],
    };

    // 獲取 symbols - 支持自動獲取熱門標的
    let symbols_by_venue = if args.auto_top_symbols {
        info!("自動獲取 {} 市場前 {} 大標的...", args.market, args.top_limit);
        let fetcher = top_symbols::TopSymbolsFetcher::new();
        let top_symbols = fetcher.get_all_top_symbols(&args.market, args.top_limit, &venues).await?;

        if top_symbols.is_empty() {
            error!("未獲取到任何熱門標的，回退到默認標的");
            let default_symbols: Vec<String> = args.symbols.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            venues.iter().map(|v| (v.to_string(), default_symbols.clone())).collect()
        } else {
            fetcher.print_statistics(&top_symbols, 10);
            fetcher.format_symbols_by_venue(&top_symbols)
        }
    } else {
        let manual_symbols: Vec<String> = args.symbols.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        info!("使用手動指定的標的: {:?}", manual_symbols);
        venues.iter().map(|v| (v.to_string(), manual_symbols.clone())).collect()
    };

    let channels: Vec<String> = args.channels.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    for v in venues {
        let venue_name = v.to_string();
        let sym = symbols_by_venue.get(&venue_name).cloned().unwrap_or_default();

        if sym.is_empty() {
            warn!("交易所 {} 沒有可用的標的，跳過", venue_name);
            continue;
        }

        info!("啟動 {} 收集器，標的數量: {}", venue_name, sym.len());
        info!("  標的列表: {:?}", sym);

        let client = client.clone();
        let chs = channels.clone();
        let batch_size = args.batch_size;
        let inst_type = args.inst_type.clone();
        let binance_market = args.binance_market.clone();
        let flush_ms = args.flush_ms;
        let ping_ms = args.ping_ms;
        let scales = scales.clone();
        let anchor_sec = args.anchor_sec;

        tasks.push(tokio::spawn(async move {
            if venue_name == "binance" {
                if let Err(e) = run_binance_loop(sym, chs, client, batch_size, flush_ms, &binance_market, ping_ms, scales, anchor_sec).await { error!("binance collector 失敗: {}", e); }
            } else if venue_name == "bitget" {
                if let Err(e) = run_bitget_loop(sym, chs, client, batch_size, flush_ms, &inst_type, ping_ms, scales, anchor_sec).await { error!("bitget collector 失敗: {}", e); }
            }
        }));
    }

    for t in tasks { let _ = t.await; }
    Ok(())
}

fn now_micros() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64
}

async fn run_binance_loop(symbols: Vec<String>, channels: Vec<String>, client: clickhouse::Client, batch_size: usize, flush_ms: u64, market: &str, ping_ms: u64, scales: ScalesMap, anchor_sec: u64) -> anyhow::Result<()> {
    let mut backoff_ms = 1000u64;
    loop {
        match run_binance_once(symbols.clone(), channels.clone(), client.clone(), batch_size, flush_ms, market, ping_ms, scales.clone(), anchor_sec).await {
            Ok(()) => { backoff_ms = 1000; }
            Err(e) => { warn!("Binance 連線失敗: {}，{}ms 後重連", e, backoff_ms); tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await; backoff_ms = (backoff_ms*2).min(30000); }
        }
    }
}

async fn run_binance_once(symbols: Vec<String>, channels: Vec<String>, client: clickhouse::Client, batch_size: usize, flush_ms: u64, market: &str, ping_ms: u64, scales: ScalesMap, anchor_sec: u64) -> anyhow::Result<()> {
    // 構造 streams，binance 需要小寫 symbol
    let mut streams = Vec::new();
    for s in &symbols {
        for c in &channels {
            let mut sym = s.to_lowercase();
            streams.push(format!("{}@{}", sym, c));
        }
    }
    let joined = streams.join("/");
    let streams_param = urlencoding::encode(&joined);
    let base = match market.to_lowercase().as_str() {
        "usdt-m" => "wss://fstream.binance.com/stream?streams=",
        "coin-m" => "wss://dstream.binance.com/stream?streams=",
        _ => "wss://stream.binance.com:9443/stream?streams=",
    };
    let url = format!("{}{}", base, streams_param);
    info!("連接 Binance: {}", url);

    // 啟動前為每個 symbol 取一次 REST 快照（Binance）並寫入 snapshot_books，並獲取 lastUpdateId
    let mut bridge_map = fetch_and_store_binance_snapshots(&client, &symbols, market).await.unwrap_or_default();
    let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();
    let mut ping_interval = if ping_ms>0 { Some(tokio::time::interval(std::time::Duration::from_millis(ping_ms))) } else { None };

    let mut batch: Vec<RawEventRow> = Vec::with_capacity(batch_size);
    let mut flusher = tokio::time::interval(std::time::Duration::from_millis(flush_ms));
    let mut bridge_state: HashMap<String, BinanceBridgeState> = symbols.iter().map(|s| {
        let last_id = bridge_map.remove(s).unwrap_or(0);
        (s.clone(), BinanceBridgeState { bridged: false, last_update_id: last_id, last_u: 0 })
    }).collect();
    let mut struct_batch: Vec<RawDepthBinanceRow> = Vec::with_capacity(batch_size);
    let mut books: HashMap<String, AnchorBook> = HashMap::new();
    let mut last_event_ts: HashMap<String, u64> = HashMap::new();
    let mut anchor = tokio::time::interval(std::time::Duration::from_secs(anchor_sec.max(1)));
    loop {
        tokio::select! {
            _ = flusher.tick() => {
                if !batch.is_empty() { flush_batch(&client, &mut batch).await?; }
                if !struct_batch.is_empty() {
                    let mut ins = client.insert::<RawDepthBinanceRow>("raw_depth_binance")?; for row in struct_batch.iter() { ins.write(row).await?; } ins.end().await?; struct_batch.clear();
                }
            }
            _ = async {
                if let Some(pi) = ping_interval.as_mut() { pi.tick().await; }
            } => {
                if ping_ms>0 {
                    if let Err(e) = write.send(Message::Ping(Vec::new())).await { warn!("Binance ping 失敗: {}", e); }
                }
            }
            ,msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // binance 包裝: { "stream": "btcusdt@depth@100ms", "data": {..} }
                        if let Ok(val) = serde_json::from_str::<JsonValue>(&text) {
                            let (channel, symbol) = parse_binance_stream(val.get("stream"));
                            batch.push(RawEventRow { timestamp: now_micros(), venue: "binance".into(), channel: channel.clone(), symbol: symbol.clone(), payload: text.clone() });
                            if batch.len() >= batch_size { flush_batch(&client, &mut batch).await?; }
                            if let Some(data) = val.get("data") {
                                if data.get("e").and_then(|v| v.as_str()) == Some("depthUpdate") {
                                    let event_ts = data.get("E").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let U = data.get("U").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let u = data.get("u").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let pu = data.get("pu").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let (bpx,bqty) = parse_price_qty_array(data.get("b"));
                                    let (apx,aqty) = parse_price_qty_array(data.get("a"));
                                    // 橋接與連續性
                                    let st = bridge_state.entry(symbol.clone()).or_insert(BinanceBridgeState{bridged:false,last_update_id:0,last_u:0});
                                    if !st.bridged {
                                        let need = st.last_update_id.saturating_add(1);
                                        if u < need { continue; }
                                        if U <= need && u >= need { st.bridged = true; st.last_u = u; } else { continue; }
                                    } else {
                                        if pu != st.last_u {
                                            let _ = insert_gap(&client, "binance", &symbol, "seq_gap", format!("pu={} prev_u={}", pu, st.last_u)).await;
                                            if let Ok(map) = fetch_and_store_binance_snapshots(&client, &[symbol.clone()], market).await { if let Some(id) = map.get(&symbol) { st.last_update_id = *id; } }
                                            st.bridged = false; st.last_u = 0; continue;
                                        }
                                        st.last_u = u;
                                    }
                                    let (ps, qs) = scales.0.get(&("BINANCE".to_string(), symbol.clone())).cloned().unwrap_or((1,1));
                                    struct_batch.push(RawDepthBinanceRow {
                                        event_ts,
                                        ingest_ts: now_micros(),
                                        symbol: symbol.clone(),
                                        U, u, pu,
                                        bids_px: bpx.clone(), bids_qty: bqty.clone(),
                                        asks_px: apx.clone(), asks_qty: aqty.clone(),
                                        price_scale: ps, qty_scale: qs,
                                        bids_px_i64: quantize_i64(&bpx, ps),
                                        bids_qty_i64: quantize_i64(&bqty, qs),
                                        asks_px_i64: quantize_i64(&apx, ps),
                                        asks_qty_i64: quantize_i64(&aqty, qs),
                                    });
                                    // 更新 Anchor 狀態
                                    let book = books.entry(symbol.clone()).or_default();
                                    book.apply_updates(&bpx, &bqty, &apx, &aqty);
                                    last_event_ts.insert(symbol.clone(), event_ts.saturating_mul(1000));
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        let payload = String::from_utf8_lossy(&bin).to_string();
                        batch.push(RawEventRow { timestamp: now_micros(), venue: "binance".into(), channel: "binary".into(), symbol: "".into(), payload });
                        if batch.len() >= batch_size { flush_batch(&client, &mut batch).await?; }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { warn!("Binance WS 錯誤: {}", e); break; }
                    None => { warn!("Binance WS 結束"); break; }
                }
            }
        }
    }
    // 最後 flush
    if !batch.is_empty() { flush_batch(&client, &mut batch).await?; }
    Ok(())
}

fn parse_binance_stream(stream_field: Option<&JsonValue>) -> (String, String) {
    if let Some(JsonValue::String(s)) = stream_field {
        // 例: btcusdt@depth@100ms 或 ethusdt@trade
        let parts: Vec<&str> = s.split('@').collect();
        if parts.len() >= 2 {
            let symbol = parts[0].to_uppercase();
            let channel = parts[1..].join("@");
            return (channel, symbol);
        }
    }
    ("unknown".into(), "".into())
}

fn parse_price_qty_array(arr: Option<&JsonValue>) -> (Vec<f64>, Vec<f64>) {
    let mut px = Vec::new(); let mut qty = Vec::new();
    if let Some(JsonValue::Array(rows)) = arr {
        for r in rows {
            if let Some(pair) = r.as_array() {
                if pair.len()>=2 { px.push(pair[0].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0)); qty.push(pair[1].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0)); }
            }
        }
    }
    (px, qty)
}

fn parse_price_qty_array_str(arr: Option<&JsonValue>) -> (Vec<String>, Vec<String>) {
    let mut px = Vec::new(); let mut qty = Vec::new();
    if let Some(JsonValue::Array(rows)) = arr {
        for r in rows {
            if let Some(pair) = r.as_array() {
                if pair.len()>=2 {
                    px.push(pair[0].as_str().unwrap_or("0").to_string());
                    qty.push(pair[1].as_str().unwrap_or("0").to_string());
                }
            }
        }
    }
    (px, qty)
}

async fn insert_gap(client: &ChClient, venue: &str, symbol: &str, reason: &str, detail: String) -> anyhow::Result<()> {
    let row = GapLogRow { ts: now_micros(), venue: venue.into(), symbol: symbol.into(), reason: reason.into(), detail };
    let mut ins = client.insert::<GapLogRow>("gap_log")?; ins.write(&row).await?; ins.end().await?; Ok(())
}

async fn fetch_and_store_binance_snapshots(client: &ChClient, symbols: &[String], market: &str) -> anyhow::Result<HashMap<String, u64>> {
    let mut map: HashMap<String, u64> = HashMap::new();
    let base = match market.to_lowercase().as_str() { "usdt-m" => "https://fapi.binance.com", "coin-m" => "https://dapi.binance.com", _=>"https://api.binance.com" };
    for s in symbols {
        let url = if market.eq_ignore_ascii_case("usdt-m") || market.eq_ignore_ascii_case("coin-m") {
            format!("{}/fapi/v1/depth?symbol={}&limit=1000", base, s)
        } else {
            format!("{}/api/v3/depth?symbol={}&limit=1000", base, s)
        };
        if let Ok(resp) = reqwest::get(&url).await { if let Ok(json) = resp.json::<JsonValue>().await {
            let last_id = json.get("lastUpdateId").and_then(|v| v.as_u64()).unwrap_or(0);
            let (bpx,bqty) = parse_price_qty_array(json.get("bids"));
            let (apx,aqty) = parse_price_qty_array(json.get("asks"));
            let row = SnapshotBooksRow { ts: now_micros(), symbol: s.clone(), venue: "binance".into(), last_id, bids_px: bpx, bids_qty: bqty, asks_px: apx, asks_qty: aqty, source: "REST".into() };
            let mut ins = client.insert::<SnapshotBooksRow>("snapshot_books")?; ins.write(&row).await?; ins.end().await?;
            map.insert(s.clone(), last_id);
        }}
    }
    Ok(map)
}

async fn run_bitget_loop(symbols: Vec<String>, channels: Vec<String>, client: clickhouse::Client, batch_size: usize, flush_ms: u64, inst_type: &str, ping_ms: u64, scales: ScalesMap, anchor_sec: u64) -> anyhow::Result<()> {
    let mut backoff_ms = 1000u64;
    loop {
        match run_bitget_once(symbols.clone(), channels.clone(), client.clone(), batch_size, flush_ms, inst_type, ping_ms, scales.clone(), anchor_sec).await {
            Ok(()) => { backoff_ms = 1000; }
            Err(e) => { warn!("Bitget 連線失敗: {}，{}ms 後重連", e, backoff_ms); tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await; backoff_ms = (backoff_ms*2).min(30000); }
        }
    }
}

async fn run_bitget_once(symbols: Vec<String>, channels: Vec<String>, client: clickhouse::Client, batch_size: usize, flush_ms: u64, inst_type: &str, ping_ms: u64, scales: ScalesMap, anchor_sec: u64) -> anyhow::Result<()> {
    use tokio_tungstenite::connect_async;
    let url = "wss://ws.bitget.com/v2/ws/public";
    info!("連接 Bitget: {}", url);
    let (mut ws_stream, _) = connect_async(url).await?;

    // 發送訂閱
    let itype = inst_type.to_string();
    // 自動加入 books1 白名單符號（20ms）
    let whitelist: [&str; 10] = [
        "BTCUSDT","ETHUSDT","XRPUSDT","SOLUSDT","SUIUSDT",
        "DOGEUSDT","ADAUSDT","PEPEUSDT","LINKUSDT","HBARUSDT"
    ];
    let mut effective_channels = channels.clone();
    let need_books1 = symbols.iter().any(|s| whitelist.contains(&s.as_str()));
    if need_books1 && !effective_channels.iter().any(|c| c.eq_ignore_ascii_case("books1")) {
        effective_channels.push("books1".to_string());
        info!("Bitget 自動啟用 books1（20ms） for whitelist symbols");
    }
    for c in &effective_channels {
        for s in &symbols {
            let arg = serde_json::json!({"instType": itype, "channel": c, "instId": s});
            let sub = serde_json::json!({"op":"subscribe","args":[arg]});
            let text = sub.to_string();
            ws_stream.send(Message::Text(text)).await?;
        }
    }

    let (mut write, mut read) = ws_stream.split();
    let mut batch: Vec<RawEventRow> = Vec::with_capacity(batch_size);
    let mut struct_batch: Vec<RawBooksBitgetRow> = Vec::with_capacity(batch_size);
    let mut trades_batch: Vec<RawTradesBitgetRow> = Vec::with_capacity(batch_size);
    let mut flusher = tokio::time::interval(std::time::Duration::from_millis(flush_ms));
    let mut ping_interval = if ping_ms>0 { Some(tokio::time::interval(std::time::Duration::from_millis(ping_ms))) } else { None };
    let mut anchor = tokio::time::interval(std::time::Duration::from_secs(anchor_sec.max(1)));
    let mut books: HashMap<String, AnchorBook> = HashMap::new();
    let mut last_seq: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::select! {
            _ = flusher.tick() => {
                if !batch.is_empty() { flush_batch(&client, &mut batch).await?; }
                if !struct_batch.is_empty() { let mut ins = client.insert::<RawBooksBitgetRow>("raw_books_bitget")?; for row in struct_batch.iter() { ins.write(row).await?; } ins.end().await?; struct_batch.clear(); }
                if !trades_batch.is_empty() { let mut ins = client.insert::<RawTradesBitgetRow>("raw_trades_bitget")?; for row in trades_batch.iter() { ins.write(row).await?; } ins.end().await?; trades_batch.clear(); }
            }
            _ = async {
                if let Some(pi) = ping_interval.as_mut() { pi.tick().await; }
            } => {
                if ping_ms>0 {
                    if let Err(e) = write.send(Message::Ping(Vec::new())).await { warn!("Bitget ping 失敗: {}", e); }
                }
            }
            ,_ = anchor.tick() => {
                for (sym, book) in books.iter() {
                    let (bpx, bqt, apx, aqt) = book.top_n(50);
                    let seq = last_seq.get(sym).cloned().unwrap_or(0);
                    let row = SnapshotBooksRow { ts: now_micros(), symbol: sym.clone(), venue: "bitget".into(), last_id: seq, bids_px: bpx, bids_qty: bqt, asks_px: apx, asks_qty: aqt, source: "ANCHOR".into() };
                    let mut ins = client.insert::<SnapshotBooksRow>("snapshot_books")?; ins.write(&row).await?; ins.end().await?;
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // bitget: { "arg": {"channel":"books", "instId":"BTCUSDT", ...}, "data": [...] }
                        if let Ok(val) = serde_json::from_str::<JsonValue>(&text) {
                            let (channel, symbol) = parse_bitget_arg(val.get("arg"));
                            batch.push(RawEventRow { timestamp: now_micros(), venue: "bitget".into(), channel: channel.clone(), symbol: symbol.clone(), payload: text.clone() });
                            if batch.len() >= batch_size { flush_batch(&client, &mut batch).await?; }
                            if channel.starts_with("books") {
                                if let Some(action) = val.get("action").and_then(|v| v.as_str()) {
                                    if let Some(data_arr) = val.get("data").and_then(|v| v.as_array()) {
                                        for ob in data_arr {
                                            let event_ts = ob.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
                                            let seq = ob.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                                            let checksum = ob.get("checksum").and_then(|v| v.as_i64()).unwrap_or(0);
                                            let (bpx,bqty) = parse_price_qty_array(ob.get("bids"));
                                            let (apx,aqty) = parse_price_qty_array(ob.get("asks"));
                                            let (bpxs,bqtys) = parse_price_qty_array_str(ob.get("bids"));
                                            let (apxs,aqtys) = parse_price_qty_array_str(ob.get("asks"));
                                            // 即時 CRC 校驗（使用原始字串）
                                            let calc = compute_bitget_crc_strings(&bpxs, &bqtys, &apxs, &aqtys);
                                            if calc as i64 != checksum { let _ = insert_gap(&client, "bitget", &symbol, "checksum_fail", format!("calc={} vs msg={}", calc, checksum)).await; }
                                            let (ps, qs) = scales.0.get(&("BITGET".to_string(), symbol.clone())).cloned().unwrap_or((1,1));
                                            struct_batch.push(RawBooksBitgetRow {
                                                event_ts,
                                                ingest_ts: now_micros(),
                                                symbol: symbol.clone(),
                                                inst_type: inst_type.to_string(),
                                                channel: channel.clone(),
                                                action: action.to_string(),
                                                seq, checksum,
                                                bids_px: bpx.clone(), bids_qty: bqty.clone(),
                                                asks_px: apx.clone(), asks_qty: aqty.clone(),
                                                bids_px_s: bpxs.clone(), bids_qty_s: bqtys.clone(),
                                                asks_px_s: apxs.clone(), asks_qty_s: aqtys.clone(),
                                                price_scale: ps, qty_scale: qs,
                                                bids_px_i64: quantize_i64(&bpx, ps),
                                                bids_qty_i64: quantize_i64(&bqty, qs),
                                                asks_px_i64: quantize_i64(&apx, ps),
                                                asks_qty_i64: quantize_i64(&aqty, qs),
                                            });
                                            if action == "snapshot" {
                                                let snap = SnapshotBooksRow { ts: now_micros(), symbol: symbol.clone(), venue: "bitget".into(), last_id: seq, bids_px: bpx.clone(), bids_qty: bqty.clone(), asks_px: apx.clone(), asks_qty: aqty.clone(), source: "WS".into() };
                                                let mut ins2 = client.insert::<SnapshotBooksRow>("snapshot_books")?; ins2.write(&snap).await?; ins2.end().await?;
                                            }
                                            // 更新 Anchor 狀態與最後序列
                                            let book = books.entry(symbol.clone()).or_default();
                                            book.apply_updates(&bpx, &bqty, &apx, &aqty);
                                            last_seq.insert(symbol.clone(), seq);
                                        }
                                    }
                                }
                            } else if channel == "trade" || channel == "trades" {
                                if let Some(data_arr) = val.get("data").and_then(|v| v.as_array()) {
                                    for tr in data_arr {
                                        let event_ts = tr.get("ts").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok())
                                            .or_else(|| tr.get("ts").and_then(|v| v.as_u64())).unwrap_or(0);
                                        let price = tr.get("px").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())
                                            .or_else(|| tr.get("price").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()))
                                            .or_else(|| tr.get("px").and_then(|v| v.as_f64()))
                                            .or_else(|| tr.get("price").and_then(|v| v.as_f64()))
                                            .unwrap_or(0.0);
                                        let quantity = tr.get("sz").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())
                                            .or_else(|| tr.get("size").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()))
                                            .or_else(|| tr.get("sz").and_then(|v| v.as_f64()))
                                            .or_else(|| tr.get("size").and_then(|v| v.as_f64()))
                                            .unwrap_or(0.0);
                                        let side = tr.get("side").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let trade_id = tr.get("tradeId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let (ps, qs) = scales.0.get(&("BITGET".to_string(), symbol.clone())).cloned().unwrap_or((1,1));
                                        let price_i64 = ((price * ps as f64).round()) as i64;
                                        let qty_i64 = ((quantity * qs as f64).round()) as i64;
                                        trades_batch.push(RawTradesBitgetRow {
                                            event_ts,
                                            ingest_ts: now_micros(),
                                            symbol: symbol.clone(),
                                            inst_type: inst_type.to_string(),
                                            trade_id,
                                            price,
                                            quantity,
                                            side,
                                            price_i64,
                                            qty_i64,
                                            price_scale: ps,
                                            qty_scale: qs,
                                        });
                                        if trades_batch.len() >= batch_size { let mut ins = client.insert::<RawTradesBitgetRow>("raw_trades_bitget")?; for row in trades_batch.iter() { ins.write(row).await?; } ins.end().await?; trades_batch.clear(); }
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        let payload = String::from_utf8_lossy(&bin).to_string();
                        batch.push(RawEventRow { timestamp: now_micros(), venue: "bitget".into(), channel: "binary".into(), symbol: "".into(), payload });
                        if batch.len() >= batch_size { flush_batch(&client, &mut batch).await?; }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { warn!("Bitget WS 錯誤: {}", e); break; }
                    None => { warn!("Bitget WS 結束"); break; }
                }
            }
        }
    }
    if !batch.is_empty() { flush_batch(&client, &mut batch).await?; }
    if !struct_batch.is_empty() { let mut ins = client.insert::<RawBooksBitgetRow>("raw_books_bitget")?; for row in struct_batch.iter() { ins.write(row).await?; } ins.end().await?; }
    if !trades_batch.is_empty() { let mut ins = client.insert::<RawTradesBitgetRow>("raw_trades_bitget")?; for row in trades_batch.iter() { ins.write(row).await?; } ins.end().await?; }
    Ok(())
}

fn parse_bitget_arg(arg: Option<&JsonValue>) -> (String, String) {
    if let Some(JsonValue::Object(map)) = arg {
        let channel = map.get("channel").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let symbol = map.get("instId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        return (channel, symbol);
    }
    ("unknown".into(), "".into())
}

fn compute_bitget_crc_strings(bpx: &[String], bqt: &[String], apx: &[String], aqt: &[String]) -> u32 {
    let n = usize::min(25, usize::min(usize::min(bpx.len(), bqt.len()), usize::min(apx.len(), aqt.len())));
    let mut parts: Vec<&str> = Vec::with_capacity(n*4);
    for i in 0..n { parts.push(&bpx[i]); parts.push(&bqt[i]); parts.push(&apx[i]); parts.push(&aqt[i]); }
    let s = parts.join(":");
    let mut hasher = Crc32::new();
    hasher.update(s.as_bytes());
    hasher.finalize()
}

async fn flush_batch(client: &clickhouse::Client, batch: &mut Vec<RawEventRow>) -> anyhow::Result<()> {
    if batch.is_empty() { return Ok(()); }
    // 使用 insert API 批量寫入
    let mut inserter = client.insert::<RawEventRow>("raw_ws_events")?;
    for row in batch.iter() { inserter.write(row).await?; }
    inserter.end().await?;
    info!("批量寫入 raw_ws_events: {}", batch.len());
    batch.clear();
    Ok(())
}
