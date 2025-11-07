#[cfg(feature = "use-adapters")]
use ports::{events::MarketEvent, MarketStream};

#[cfg(feature = "use-adapters")]
use anyhow::Result;
#[cfg(feature = "use-adapters")]
use chrono::Utc;
#[cfg(feature = "use-adapters")]
use clickhouse::Client;
#[cfg(feature = "use-adapters")]
use futures::StreamExt;
#[cfg(feature = "use-adapters")]
use tracing::{error, info, warn};

#[cfg(feature = "use-adapters")]
use std::sync::Arc;

#[cfg(feature = "use-adapters")]
use crate::db::{self, DbConfig};
use crate::metrics::{ERRORS_TOTAL, EVENTS_TOTAL};

#[cfg(feature = "use-adapters")]
#[derive(Clone, Copy, Debug)]
pub struct BatchThresholds {
    pub snapshot: usize,
    pub trade: usize,
    pub raw: usize,
    pub l2: usize,
    pub funding: usize,
}

#[cfg(feature = "use-adapters")]
impl BatchThresholds {
    pub fn with_overrides(
        snapshot: usize,
        trade: usize,
        raw: usize,
        l2: usize,
        funding: usize,
    ) -> Self {
        use std::cmp::max;
        Self {
            snapshot: max(snapshot, 1),
            trade: max(trade, 1),
            raw: max(raw, 1),
            l2: max(l2, 1),
            funding: max(funding, 1),
        }
    }
}

#[cfg(feature = "use-adapters")]
impl Default for BatchThresholds {
    fn default() -> Self {
        Self {
            snapshot: 1000,
            trade: 1000,
            raw: 1000,
            l2: 5000,
            funding: 500,
        }
    }
}

#[cfg(feature = "use-adapters")]
async fn flush_rows(
    rows: &mut Vec<String>,
    sink: &Arc<dyn db::DbSink>,
    table: &str,
    context: &str,
) {
    let batch = std::mem::take(rows);
    if let Err(err) = sink.insert_json_rows(table, &batch).await {
        warn!(
            table = table,
            context = context,
            error = %err,
            "failed to insert batch",
        );
        rows.extend(batch);
    }
}

#[cfg(feature = "use-adapters")]
async fn flush_when_full(
    rows: &mut Vec<String>,
    sink: &Arc<dyn db::DbSink>,
    table: &str,
    threshold: usize,
    context: &str,
) {
    if rows.len() >= threshold {
        flush_rows(rows, sink, table, context).await;
    }
}

#[cfg(feature = "use-adapters")]
async fn flush_remaining(
    rows: &mut Vec<String>,
    sink: &Arc<dyn db::DbSink>,
    table: &str,
    context: &str,
) {
    if !rows.is_empty() {
        flush_rows(rows, sink, table, context).await;
    }
}

#[cfg(feature = "use-adapters")]
fn dec_to_f64(d: &rust_decimal::Decimal) -> f64 {
    d.to_string().parse::<f64>().unwrap_or(0.0)
}

#[cfg(feature = "use-adapters")]
fn symbol_to_string(sym: &hft_core::Symbol) -> String {
    sym.as_str().to_string()
}

#[cfg(feature = "use-adapters")]
fn ts_micros_to_dt64(ts: u64) -> chrono::DateTime<Utc> {
    let ms = (ts / 1000) as i64;
    chrono::DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(|| Utc::now())
}

#[cfg(feature = "use-adapters")]
#[derive(Default)]
struct ObState {
    bids: std::collections::BTreeMap<ordered_float::OrderedFloat<f64>, f64>,
    asks: std::collections::BTreeMap<ordered_float::OrderedFloat<f64>, f64>,
    last_seq: u64,
}

#[cfg(feature = "use-adapters")]
impl ObState {
    fn apply_snapshot(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)], seq: u64) {
        self.bids.clear();
        self.asks.clear();
        for (p, q) in bids.iter().cloned() {
            if p > 0.0 && q > 0.0 {
                self.bids.insert(ordered_float::OrderedFloat(p), q);
            }
        }
        for (p, q) in asks.iter().cloned() {
            if p > 0.0 && q > 0.0 {
                self.asks.insert(ordered_float::OrderedFloat(p), q);
            }
        }
        self.last_seq = seq;
    }
    fn apply_update(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)], seq: u64) {
        for (p, q) in bids.iter().cloned() {
            let k = ordered_float::OrderedFloat(p);
            if q <= 0.0 {
                self.bids.remove(&k);
            } else if p > 0.0 {
                self.bids.insert(k, q);
            }
        }
        for (p, q) in asks.iter().cloned() {
            let k = ordered_float::OrderedFloat(p);
            if q <= 0.0 {
                self.asks.remove(&k);
            } else if p > 0.0 {
                self.asks.insert(k, q);
            }
        }
        self.last_seq = seq;
    }
    fn topn(&self, n: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut bpx = Vec::with_capacity(n);
        let mut bqty = Vec::with_capacity(n);
        for (p, q) in self.bids.iter().rev().take(n) {
            bpx.push(p.0);
            bqty.push(*q);
        }
        let mut apx = Vec::with_capacity(n);
        let mut aqty = Vec::with_capacity(n);
        for (p, q) in self.asks.iter().take(n) {
            apx.push(p.0);
            aqty.push(*q);
        }
        (bpx, bqty, apx, aqty)
    }
}

#[cfg(feature = "use-adapters")]
pub async fn run_with_marketstream(
    exchange: &str,
    symbols: Vec<String>,
    ch_url: &str,
    database: &str,
    top_n: usize,
    dry_run: bool,
    store_raw: bool,
    lob_mode: &str,
    batches: BatchThresholds,
    collect_funding: bool,
) -> Result<()> {
    let ch_user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string());
    let ch_password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "".to_string());

    db::init_db_config(DbConfig::clickhouse(
        ch_url.to_string(),
        database.to_string(),
        ch_user.clone(),
        ch_password.clone(),
        dry_run,
    ));

    let mut client = Client::default().with_url(ch_url);
    if !ch_password.is_empty() {
        client = client.with_user(&ch_user).with_password(&ch_password);
    }

    if !dry_run {
        let create_db_query = format!("CREATE DATABASE IF NOT EXISTS {}", database);
        client.query(&create_db_query).execute().await?;
        let create_snapshot_books = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.snapshot_books (
                ts UInt64,
                symbol String,
                venue String,
                last_id UInt64,
                bids_px Array(Float64),
                bids_qty Array(Float64),
                asks_px Array(Float64),
                asks_qty Array(Float64),
                source String
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            SETTINGS index_granularity = 8192
            "#,
            database
        );
        client.query(&create_snapshot_books).execute().await?;

        let create_trades = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.unified_trades (
                ts DateTime64(3),
                exchange LowCardinality(String),
                symbol LowCardinality(String),
                price Float64,
                qty Float64,
                side Enum8('buy'=1, 'sell'=2),
                trade_id String
            ) ENGINE = MergeTree()
            ORDER BY (exchange, symbol, ts)
            "#,
            database
        );
        client.query(&create_trades).execute().await?;
        if store_raw {
            let create_raw = format!(
                r#"CREATE TABLE IF NOT EXISTS {}.raw_ws (
                    ts DateTime64(3),
                    exchange LowCardinality(String),
                    payload String
                ) ENGINE = MergeTree() ORDER BY (exchange, ts)"#,
                database
            );
            client.query(&create_raw).execute().await?;
        }
        if matches!(lob_mode, "row" | "both") {
            let create_l2 = format!(
                r#"CREATE TABLE IF NOT EXISTS {}.unified_l2 (
                    ts DateTime64(3),
                    exchange LowCardinality(String),
                    symbol LowCardinality(String),
                    side Enum8('bid'=1,'ask'=2),
                    price Float64,
                    qty Float64,
                    seq UInt64,
                    is_snapshot UInt8
                ) ENGINE = MergeTree() ORDER BY (exchange, symbol, ts)"#,
                database
            );
            client.query(&create_l2).execute().await?;
        }
        if collect_funding {
            let create_funding = format!(
                r#"CREATE TABLE IF NOT EXISTS {}.unified_funding (
                    ts DateTime64(3),
                    exchange LowCardinality(String),
                    symbol LowCardinality(String),
                    funding_rate Float64,
                    next_funding_time DateTime64(3),
                    mark_price Float64
                ) ENGINE = MergeTree() ORDER BY (exchange, symbol, ts)"#,
                database
            );
            client.query(&create_funding).execute().await?;
        }
    }

    // 建立 MarketStream
    let ms = create_marketstream(exchange, &symbols)?;
    let hft_symbols: Vec<hft_core::Symbol> = symbols
        .iter()
        .map(|s| hft_core::Symbol::from(s.clone()))
        .collect();
    let mut stream = ms
        .subscribe(hft_symbols)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let sink = db::get_sink_async(database).await?;

    info!("MarketStream running for {} on {:?}", exchange, symbols);
    let mut snap_rows: Vec<String> = Vec::with_capacity(2048);
    let mut trade_rows: Vec<String> = Vec::with_capacity(2048);
    let mut ob_states: std::collections::HashMap<String, ObState> =
        std::collections::HashMap::new();
    let mut raw_rows: Vec<String> = Vec::with_capacity(2048);
    let mut l2_rows: Vec<String> = Vec::with_capacity(4096);

    // 資金費率收集（WS 方式：Binance/Aster 的 markPrice）
    if collect_funding {
        let ex = exchange.to_lowercase();
        let syms = symbols.clone();
        let sink_clone = Arc::clone(&sink);
        tokio::spawn(async move {
            if ex == "asterdex" || ex == "binance_futures" || ex == "binance" {
                let base = if ex == "asterdex" {
                    "wss://fstream.asterdex.com"
                } else {
                    "wss://fstream.binance.com"
                };
                let streams: Vec<String> = syms
                    .iter()
                    .map(|s| format!("{}@markPrice@1s", s.to_lowercase()))
                    .collect();
                let url = format!("{}/stream?streams={}", base, streams.join("/"));
                if let Ok((mut ws, _)) = tokio_tungstenite::connect_async(url).await {
                    let mut rows: Vec<String> = Vec::with_capacity(1024);
                    while let Some(msg) = ws.next().await {
                        if let Ok(tokio_tungstenite::tungstenite::Message::Text(txt)) = msg {
                            let v: serde_json::Value = match serde_json::from_str(&txt) {
                                Ok(x) => x,
                                Err(_) => continue,
                            };
                            let data = v.get("data").cloned().unwrap_or(v.clone());
                            let e = data.get("e").and_then(|x| x.as_str()).unwrap_or("");
                            if e == "markPriceUpdate" {
                                let sym = data.get("s").and_then(|x| x.as_str()).unwrap_or("");
                                let r = data
                                    .get("r")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                                    .unwrap_or(0.0);
                                let t = data.get("T").and_then(|x| x.as_i64()).unwrap_or(0) as u64;
                                let p = data
                                    .get("p")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                                    .unwrap_or(0.0);
                                let row = serde_json::json!({
                                    "ts": chrono::Utc::now(),
                                    "exchange": ex,
                                    "symbol": sym,
                                    "funding_rate": r,
                                    "next_funding_time": chrono::DateTime::<chrono::Utc>::from_timestamp_millis(t as i64).unwrap_or_else(|| chrono::Utc::now()),
                                    "mark_price": p,
                                });
                                if let Ok(line) = serde_json::to_string(&row) {
                                    rows.push(line);
                                    flush_when_full(
                                        &mut rows,
                                        &sink_clone,
                                        "unified_funding",
                                        batches.funding,
                                        "funding_ws",
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    flush_remaining(
                        &mut rows,
                        &sink_clone,
                        "unified_funding",
                        "funding_ws_final",
                    )
                    .await;
                }
            }
        });
        // REST Fallback（可選，Bybit/Bitget）：COLLECTOR_FUNDING_REST_FALLBACK=true
        let rest_fallback = matches!(
            std::env::var("COLLECTOR_FUNDING_REST_FALLBACK")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );
        let ex_rest = exchange.to_lowercase();
        if rest_fallback && ((ex_rest.as_str() == "bybit") || (ex_rest.as_str() == "bitget")) {
            let tmpl = if ex_rest.as_str() == "bybit" {
                std::env::var("BYBIT_FUNDING_URL_TMPL").ok()
            } else {
                std::env::var("BITGET_FUNDING_URL_TMPL").ok()
            };
            if let Some(url_tmpl) = tmpl {
                let syms2 = symbols.clone();
                let sink2 = Arc::clone(&sink);
                let ex_name = exchange.to_string();
                let poll_secs: u64 = std::env::var("COLLECTOR_FUNDING_POLL_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(120);
                tokio::spawn(async move {
                    let http = reqwest::Client::new();
                    loop {
                        let mut rows: Vec<String> = Vec::new();
                        for s in &syms2 {
                            let url = url_tmpl.replace("{symbol}", &s);
                            match http
                                .get(&url)
                                .timeout(std::time::Duration::from_secs(10))
                                .send()
                                .await
                            {
                                Ok(resp) => {
                                    if let Ok(v) = resp.json::<serde_json::Value>().await {
                                        // 嘗試從多層結構提取 funding
                                        let (funding, next_ts, mark) =
                                            extract_funding_fields(&v, s);
                                        if funding.is_some() {
                                            if let Ok(line) = serde_json::to_string(
                                                &serde_json::json!({
                                                    "ts": chrono::Utc::now(),
                                                    "exchange": ex_name,
                                                    "symbol": s,
                                                    "funding_rate": funding.unwrap_or(0.0),
                                                    "next_funding_time": next_ts.unwrap_or_else(|| chrono::Utc::now()),
                                                    "mark_price": mark.unwrap_or(0.0),
                                                }),
                                            ) {
                                                rows.push(line);
                                                flush_when_full(
                                                    &mut rows,
                                                    &sink2,
                                                    "unified_funding",
                                                    batches.funding,
                                                    "funding_rest",
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                        flush_remaining(&mut rows, &sink2, "unified_funding", "funding_rest").await;
                        tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                    }
                });
            }
        }
    }

    while let Some(evt) = stream.next().await {
        match evt {
            Ok(MarketEvent::Snapshot(s)) => {
                EVENTS_TOTAL
                    .with_label_values(&[exchange, "snapshot"])
                    .inc();
                // 取 Top-N
                let mut bids_px = Vec::with_capacity(top_n);
                let mut bids_qty = Vec::with_capacity(top_n);
                for bl in s.bids.iter().take(top_n) {
                    bids_px.push(dec_to_f64(&bl.price.0));
                    bids_qty.push(dec_to_f64(&bl.quantity.0));
                }
                let mut asks_px = Vec::with_capacity(top_n);
                let mut asks_qty = Vec::with_capacity(top_n);
                for bl in s.asks.iter().take(top_n) {
                    asks_px.push(dec_to_f64(&bl.price.0));
                    asks_qty.push(dec_to_f64(&bl.quantity.0));
                }
                // 更新本地狀態（供純增量來源使用）
                let key = symbol_to_string(&s.symbol);
                let mut bids_vec = Vec::with_capacity(s.bids.len());
                for bl in &s.bids {
                    bids_vec.push((dec_to_f64(&bl.price.0), dec_to_f64(&bl.quantity.0)));
                }
                let mut asks_vec = Vec::with_capacity(s.asks.len());
                for bl in &s.asks {
                    asks_vec.push((dec_to_f64(&bl.price.0), dec_to_f64(&bl.quantity.0)));
                }
                ob_states
                    .entry(key.clone())
                    .or_default()
                    .apply_snapshot(&bids_vec, &asks_vec, s.sequence);

                let row = serde_json::json!({
                    "ts": s.timestamp, // already micros
                    "symbol": key,
                    "venue": exchange,
                    "last_id": s.sequence,
                    "bids_px": bids_px,
                    "bids_qty": bids_qty,
                    "asks_px": asks_px,
                    "asks_qty": asks_qty,
                    "source": "WS-MS",
                });
                if let Ok(line) = serde_json::to_string(&row) {
                    snap_rows.push(line);
                    flush_when_full(
                        &mut snap_rows,
                        &sink,
                        "snapshot_books",
                        batches.snapshot,
                        "snapshot_batch",
                    )
                    .await;
                }
                if matches!(lob_mode, "row" | "both") {
                    // 行寫入（依據事件內容全部檔位）
                    let ts = ts_micros_to_dt64(s.timestamp);
                    let sym = symbol_to_string(&s.symbol);
                    for bl in &s.bids {
                        let row = serde_json::json!({
                            "ts": ts,
                            "exchange": exchange,
                            "symbol": sym,
                            "side": "bid",
                            "price": dec_to_f64(&bl.price.0),
                            "qty": dec_to_f64(&bl.quantity.0),
                            "seq": s.sequence,
                            "is_snapshot": 1u8,
                        });
                        if let Ok(line) = serde_json::to_string(&row) {
                            l2_rows.push(line);
                            flush_when_full(
                                &mut l2_rows,
                                &sink,
                                "unified_l2",
                                batches.l2,
                                "l2_snapshot",
                            )
                            .await;
                        }
                    }
                    for bl in &s.asks {
                        let row = serde_json::json!({
                            "ts": ts,
                            "exchange": exchange,
                            "symbol": sym,
                            "side": "ask",
                            "price": dec_to_f64(&bl.price.0),
                            "qty": dec_to_f64(&bl.quantity.0),
                            "seq": s.sequence,
                            "is_snapshot": 1u8,
                        });
                        if let Ok(line) = serde_json::to_string(&row) {
                            l2_rows.push(line);
                            flush_when_full(
                                &mut l2_rows,
                                &sink,
                                "unified_l2",
                                batches.l2,
                                "l2_snapshot",
                            )
                            .await;
                        }
                    }
                }
                if store_raw {
                    if let Ok(line) = serde_json::to_string(&serde_json::json!({
                        "ts": ts_micros_to_dt64(s.timestamp),
                        "exchange": exchange,
                        "payload": serde_json::to_string(&serde_json::json!({"event":"snapshot","symbol":symbol_to_string(&s.symbol),"seq":s.sequence})).unwrap_or_default()
                    })) {
                        raw_rows.push(line);
                        flush_when_full(
                            &mut raw_rows,
                            &sink,
                            "raw_ws",
                            batches.raw,
                            "raw_ws_snapshot",
                        )
                        .await;
                    }
                }
            }
            Ok(MarketEvent::Trade(t)) => {
                EVENTS_TOTAL.with_label_values(&[exchange, "trade"]).inc();
                let dt = ts_micros_to_dt64(t.timestamp);
                let side = match t.side {
                    hft_core::Side::Buy => "buy",
                    _ => "sell",
                };
                let row = serde_json::json!({
                    "ts": dt,
                    "exchange": exchange,
                    "symbol": symbol_to_string(&t.symbol),
                    "price": dec_to_f64(&t.price.0),
                    "qty": dec_to_f64(&t.quantity.0),
                    "side": side,
                    "trade_id": t.trade_id,
                });
                if let Ok(line) = serde_json::to_string(&row) {
                    trade_rows.push(line);
                    flush_when_full(
                        &mut trade_rows,
                        &sink,
                        "unified_trades",
                        batches.trade,
                        "trade_batch",
                    )
                    .await;
                }
                if store_raw {
                    if let Ok(line) = serde_json::to_string(&serde_json::json!({
                        "ts": dt,
                        "exchange": exchange,
                        "payload": serde_json::to_string(&serde_json::json!({"event":"trade","symbol":symbol_to_string(&t.symbol),"id":t.trade_id})).unwrap_or_default()
                    })) {
                        raw_rows.push(line);
                        flush_when_full(
                            &mut raw_rows,
                            &sink,
                            "raw_ws",
                            batches.raw,
                            "raw_ws_trade",
                        )
                        .await;
                    }
                }
            }
            Ok(MarketEvent::Update(u)) => {
                EVENTS_TOTAL.with_label_values(&[exchange, "update"]).inc();
                // 套用增量並輸出 Top-N 快照
                let key = symbol_to_string(&u.symbol);
                let state = ob_states.entry(key.clone()).or_default();
                let mut bids = Vec::with_capacity(u.bids.len());
                for bl in &u.bids {
                    bids.push((dec_to_f64(&bl.price.0), dec_to_f64(&bl.quantity.0)));
                }
                let mut asks = Vec::with_capacity(u.asks.len());
                for bl in &u.asks {
                    asks.push((dec_to_f64(&bl.price.0), dec_to_f64(&bl.quantity.0)));
                }
                state.apply_update(&bids, &asks, u.sequence);
                let (bpx, bqty, apx, aqty) = state.topn(top_n);
                let row = serde_json::json!({
                    "ts": u.timestamp,
                    "symbol": key,
                    "venue": exchange,
                    "last_id": u.sequence,
                    "bids_px": bpx,
                    "bids_qty": bqty,
                    "asks_px": apx,
                    "asks_qty": aqty,
                    "source": "WS-MS",
                });
                if let Ok(line) = serde_json::to_string(&row) {
                    snap_rows.push(line);
                    flush_when_full(
                        &mut snap_rows,
                        &sink,
                        "snapshot_books",
                        batches.snapshot,
                        "snapshot_update",
                    )
                    .await;
                }
                if matches!(lob_mode, "row" | "both") {
                    let ts = ts_micros_to_dt64(u.timestamp);
                    for bl in &u.bids {
                        let row = serde_json::json!({
                            "ts": ts,
                            "exchange": exchange,
                            "symbol": key,
                            "side": "bid",
                            "price": dec_to_f64(&bl.price.0),
                            "qty": dec_to_f64(&bl.quantity.0),
                            "seq": u.sequence,
                            "is_snapshot": 0u8,
                        });
                        if let Ok(line) = serde_json::to_string(&row) {
                            l2_rows.push(line);
                            flush_when_full(
                                &mut l2_rows,
                                &sink,
                                "unified_l2",
                                batches.l2,
                                "l2_update",
                            )
                            .await;
                        }
                    }
                    for bl in &u.asks {
                        let row = serde_json::json!({
                            "ts": ts,
                            "exchange": exchange,
                            "symbol": key,
                            "side": "ask",
                            "price": dec_to_f64(&bl.price.0),
                            "qty": dec_to_f64(&bl.quantity.0),
                            "seq": u.sequence,
                            "is_snapshot": 0u8,
                        });
                        if let Ok(line) = serde_json::to_string(&row) {
                            l2_rows.push(line);
                            flush_when_full(
                                &mut l2_rows,
                                &sink,
                                "unified_l2",
                                batches.l2,
                                "l2_update",
                            )
                            .await;
                        }
                    }
                }
                if store_raw {
                    if let Ok(line) = serde_json::to_string(&serde_json::json!({
                        "ts": ts_micros_to_dt64(u.timestamp),
                        "exchange": exchange,
                        "payload": serde_json::to_string(&serde_json::json!({"event":"update","symbol":key,"seq":u.sequence})).unwrap_or_default()
                    })) {
                        raw_rows.push(line);
                        flush_when_full(
                            &mut raw_rows,
                            &sink,
                            "raw_ws",
                            batches.raw,
                            "raw_ws_update",
                        )
                        .await;
                    }
                }
            }
            Ok(_other) => {
                // 暫不處理 Bar/Disconnect
            }
            Err(e) => {
                ERRORS_TOTAL.with_label_values(&[exchange]).inc();
                warn!("MarketStream event error: {}", e);
            }
        }
    }

    // flush any remaining
    flush_remaining(&mut snap_rows, &sink, "snapshot_books", "snapshot_final").await;
    flush_remaining(&mut trade_rows, &sink, "unified_trades", "trade_final").await;
    if matches!(lob_mode, "row" | "both") {
        flush_remaining(&mut l2_rows, &sink, "unified_l2", "l2_final").await;
    }
    if store_raw {
        flush_remaining(&mut raw_rows, &sink, "raw_ws", "raw_ws_final").await;
    }

    Ok(())
}

#[cfg(feature = "use-adapters")]
fn extract_funding_fields(
    v: &serde_json::Value,
    symbol: &str,
) -> (
    Option<f64>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<f64>,
) {
    use chrono::Utc;
    // 嘗試常見層級：result.list[] / data / 直接字段
    // fundingRate / funding_rate；nextFundingTime / nextFundingTs；markPrice / mark_price
    fn num(obj: &serde_json::Value, k: &str) -> Option<f64> {
        obj.get(k).and_then(|x| {
            x.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| x.as_f64())
        })
    }
    fn ts_dt(obj: &serde_json::Value, k: &str) -> Option<chrono::DateTime<Utc>> {
        obj.get(k)
            .and_then(|x| x.as_i64())
            .and_then(|ms| chrono::DateTime::<Utc>::from_timestamp_millis(ms))
    }
    // 直接字段
    let direct_f = num(v, "fundingRate").or_else(|| num(v, "funding_rate"));
    let direct_t = ts_dt(v, "nextFundingTime").or_else(|| ts_dt(v, "nextFundingTs"));
    let direct_m = num(v, "markPrice").or_else(|| num(v, "mark_price"));
    if direct_f.is_some() {
        return (direct_f, direct_t.or(Some(Utc::now())), direct_m);
    }
    // data 層
    if let Some(data) = v.get("data") {
        let f = num(data, "fundingRate").or_else(|| num(data, "funding_rate"));
        let t = ts_dt(data, "nextFundingTime").or_else(|| ts_dt(data, "nextFundingTs"));
        let m = num(data, "markPrice").or_else(|| num(data, "mark_price"));
        if f.is_some() {
            return (f, t.or(Some(Utc::now())), m);
        }
    }
    // result.list[]
    if let Some(result) = v.get("result") {
        if let Some(list) = result.get("list").and_then(|x| x.as_array()) {
            for item in list {
                let sym_ok = item
                    .get("symbol")
                    .and_then(|x| x.as_str())
                    .map(|s| s.eq_ignore_ascii_case(symbol))
                    .unwrap_or(true);
                if !sym_ok {
                    continue;
                }
                let f = num(item, "fundingRate").or_else(|| num(item, "funding_rate"));
                let t = ts_dt(item, "nextFundingTime").or_else(|| ts_dt(item, "nextFundingTs"));
                let m = num(item, "markPrice").or_else(|| num(item, "mark_price"));
                if f.is_some() {
                    return (f, t.or(Some(Utc::now())), m);
                }
            }
        }
    }
    (None, None, None)
}

#[cfg(feature = "use-adapters")]
fn create_marketstream(exchange: &str, symbols: &[String]) -> Result<Box<dyn MarketStream>> {
    match exchange.to_lowercase().as_str() {
        "binance" => {
            // 可通過環境變數控制 REST 回退等能力（若適配器暴露）
            Ok(Box::new(data_adapter_binance::BinanceMarketStream::new()))
        }
        "bybit" => Ok(Box::new(adapter_bybit_data::BybitMarketStream::new())),
        "bitget" => {
            let inst =
                std::env::var("BITGET_INST_TYPE").unwrap_or_else(|_| "USDT-FUTURES".to_string());
            let ch = std::env::var("BITGET_DEPTH_CHANNEL").unwrap_or_else(|_| "books5".to_string());
            let incr = ch.to_lowercase() == "books";
            let mut s = data_adapter_bitget::BitgetMarketStream::new_with_incremental(incr)
                .with_inst_type(inst)
                .with_depth_channel(ch);
            Ok(Box::new(s))
        }
        "asterdex" => Ok(Box::new(adapter_asterdex_data::AsterdexMarketStream::new())),
        "lighter" => Ok(Box::new(adapter_lighter_data::LighterMarketStream::new())),
        "hyperliquid" => {
            let syms: Vec<hft_core::Symbol> = symbols
                .iter()
                .map(|s| hft_core::Symbol::from(s.clone()))
                .collect();
            Ok(Box::new(
                adapter_hyperliquid_data::HyperliquidMarketStream::with_symbols(syms),
            ))
        }
        other => Err(anyhow::anyhow!(format!(
            "Unsupported exchange for adapters: {}",
            other
        ))),
    }
}
