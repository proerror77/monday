//! HFT 歷史回放應用
//!
//! 用於歷史數據回放、回測和策略驗證

use chrono::{DateTime, Utc};
use clap::Parser;
use clickhouse::Client as ChClient;
use crc32fast::Hasher as Crc32;
use ordered_float::OrderedFloat;
use serde::Deserialize;
use std::collections::BTreeMap;
use tracing::{debug, error, info};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置檔案路徑
    #[arg(short, long, default_value = "config/dev/system.yaml")]
    config: String,

    /// ClickHouse URL
    #[arg(long, default_value = "http://localhost:8123")]
    ch_url: String,

    /// ClickHouse Database
    #[arg(long, default_value = "hft")]
    database: String,

    /// 交易所（binance/bitget）
    #[arg(long, default_value = "binance")]
    venue: String,

    /// 符號（例如 BTCUSDT）
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,

    /// 回放開始（Unix微秒）
    #[arg(long, default_value_t = 0u64)]
    start_us: u64,

    /// 回放結束（Unix微秒，0 表示不限）
    #[arg(long, default_value_t = 0u64)]
    end_us: u64,

    /// 回放速度倍數 (1.0 = 原速)
    #[arg(long, default_value = "1.0")]
    speed: f64,

    /// 啟動後自動退出的毫秒數（用於測試）
    #[arg(long)]
    exit_after_ms: Option<u64>,

    /// Metrics HTTP 服務器端口（啟用 metrics feature 時生效）
    #[cfg(feature = "metrics")]
    #[arg(long, default_value = "9092")]
    metrics_port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt().with_env_filter("info").init();

    let args = Args::parse();

    info!("啟動 HFT 歷史回放系統");
    info!("配置檔案: {}", args.config);

    // 啟動 metrics HTTP 服務器（如果啟用 metrics feature）
    #[cfg(feature = "metrics")]
    {
        use infra_metrics::http_server::{MetricsServer, MetricsServerConfig};
        let cfg = MetricsServerConfig {
            bind_address: "0.0.0.0".to_string(),
            port: args.metrics_port,
            verbose_logging: false,
            readiness_max_idle_secs: 5,
            readiness_max_utilization: 0.9,
        };
        let _handle = MetricsServer::start_background(cfg);
        info!(
            "Metrics 服務器已啟動: http://0.0.0.0:{}/metrics",
            args.metrics_port
        );
    }

    info!("回放速度: {}x", args.speed);

    // 建立 ClickHouse 連線
    let ch = ChClient::default()
        .with_url(&args.ch_url)
        .with_database(&args.database);

    // 加載起始快照
    let snapshot = load_latest_snapshot(&ch, &args.venue, &args.symbol, args.start_us).await?;
    info!(
        "載入快照: ts={}, bids={} asks={}",
        snapshot.ts,
        snapshot.bids_px.len(),
        snapshot.asks_px.len()
    );

    // 初始化 LOB 狀態
    let mut book = OrderBookState::from_snapshot(&snapshot);

    // 回放增量
    let end_us = if args.end_us == 0 {
        u64::MAX
    } else {
        args.end_us
    };
    let (table, order) = if args.venue.eq_ignore_ascii_case("binance") {
        ("raw_depth_binance", "ORDER BY event_ts ASC, u ASC")
    } else {
        ("raw_books_bitget", "ORDER BY event_ts ASC, seq ASC")
    };
    let q = format!(
        "SELECT event_ts, ingest_ts, symbol, U, u, pu, bids_px, bids_qty, asks_px, asks_qty, action, checksum, price_scale, qty_scale, bids_px_s, bids_qty_s, asks_px_s, asks_qty_s \
         FROM {} WHERE symbol = ? AND event_ts >= ? AND event_ts <= ? {}",
        table, order
    );
    let mut cursor = ch
        .query(&q)
        .bind(&args.symbol)
        .bind(snapshot.ts)
        .bind(end_us)
        .fetch::<EventRecord>()?;
    let speed = if args.speed <= 0.0 { 1.0 } else { args.speed } as f64;
    let mut last_ts = snapshot.ts;
    let mut prev_u: Option<u64> = None; // Binance U/u/pu 連續性
    let mut seq: u64 = 0;

    while let Some(r) = cursor.next().await? {
        let ev = to_event_row(&args.venue, r);
        // pacing
        if args.speed > 0.0 && last_ts > 0 && ev.event_ts > last_ts {
            let dt = ev.event_ts - last_ts;
            let sleep_ns = (dt as f64 / speed) as u64 * 1000; // us→ns
            if sleep_ns > 0 {
                tokio::time::sleep(std::time::Duration::from_nanos(sleep_ns)).await;
            }
        }
        last_ts = ev.event_ts;

        // 套用更新
        match ev.kind {
            EventKind::Binance {
                bids_px,
                bids_qty,
                asks_px,
                asks_qty,
                u,
                pu,
                ..
            } => {
                // 連續性：pu == prev_u
                if let Some(prev) = prev_u {
                    if pu != prev {
                        insert_gap(
                            &ch,
                            &args.venue,
                            &args.symbol,
                            "seq_gap",
                            format!("pu={} prev_u={}", pu, prev),
                        )
                        .await
                        .ok();
                    }
                }
                prev_u = Some(u);
                book.apply_updates(&bids_px, &bids_qty, true);
                book.apply_updates(&asks_px, &asks_qty, false);
            }
            EventKind::Bitget {
                action,
                bids_px,
                bids_qty,
                asks_px,
                asks_qty,
                bids_px_s,
                bids_qty_s,
                asks_px_s,
                asks_qty_s,
                checksum,
                price_scale,
                qty_scale,
                ..
            } => {
                // Bitget 校驗：優先使用原始字串，否則以固定小數位格式化
                if let Some(cs) = checksum {
                    let crc = compute_bitget_crc(
                        bids_px_s.as_ref().map(|v| v.as_slice()),
                        bids_qty_s.as_ref().map(|v| v.as_slice()),
                        asks_px_s.as_ref().map(|v| v.as_slice()),
                        asks_qty_s.as_ref().map(|v| v.as_slice()),
                        &bids_px,
                        &bids_qty,
                        &asks_px,
                        &asks_qty,
                        price_scale,
                        qty_scale,
                    );
                    if crc as i64 != cs {
                        insert_gap(
                            &ch,
                            &args.venue,
                            &args.symbol,
                            "checksum_fail",
                            format!("calc={} vs msg={}", crc, cs),
                        )
                        .await
                        .ok();
                    }
                }
                if action == "snapshot" {
                    book.reset();
                }
                book.apply_updates(&bids_px, &bids_qty, true);
                book.apply_updates(&asks_px, &asks_qty, false);
            }
        }

        // 輸出部分觀測（可擴展為寫入 lob_depth 對照）
        // 將回放狀態寫入 lob_depth（便於和 live 對照）
        if let (Some(bb), Some(ba)) = (book.best_bid(), book.best_ask()) {
            seq = seq.saturating_add(1);
            let spread = ba - bb;
            let mid = (bb + ba) / 2.0;
            let (bpx, bqt) = book.top_n(true, 50);
            let (apx, aqt) = book.top_n(false, 50);
            let row = LobDepthRow {
                timestamp: to_dt64(ev.event_ts),
                symbol: args.symbol.clone(),
                exchange: args.venue.to_uppercase(),
                sequence: seq,
                is_snapshot: 0,
                bid_prices: bpx,
                bid_quantities: bqt,
                ask_prices: apx,
                ask_quantities: aqt,
                bid_levels: 0u16,
                ask_levels: 0u16,
                spread,
                mid_price: mid,
                data_source: "replay".to_string(),
            };
            if let Err(e) = insert_lob_depth(&ch, row).await {
                error!("寫入 lob_depth 失敗: {}", e);
            }
            debug!(
                "{} {} best_bid={:.8} best_ask={:.8} spread={:.8}",
                args.venue, args.symbol, bb, ba, spread
            );
        }

        if let Some(ms) = args.exit_after_ms {
            // allow bounded playback for tests
            if last_ts > snapshot.ts + ms * 1000 {
                break;
            }
        }
    }

    info!("回放完成");
    Ok(())
}

// ----- Simple LOB state -----
#[derive(Default)]
struct OrderBookState {
    bids: BTreeMap<OrderedFloat<f64>, f64>, // price -> qty
    asks: BTreeMap<OrderedFloat<f64>, f64>, // price -> qty
}

impl OrderBookState {
    fn from_snapshot(s: &SnapshotRow) -> Self {
        let mut ob = Self::default();
        for (i, p) in s.bids_px.iter().enumerate() {
            ob.bids
                .insert(OrderedFloat(*p), *s.bids_qty.get(i).unwrap_or(&0.0));
        }
        for (i, p) in s.asks_px.iter().enumerate() {
            ob.asks
                .insert(OrderedFloat(*p), *s.asks_qty.get(i).unwrap_or(&0.0));
        }
        ob
    }
    fn reset(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }
    fn apply_updates(&mut self, px: &[f64], qty: &[f64], is_bid: bool) {
        let book = if is_bid {
            &mut self.bids
        } else {
            &mut self.asks
        };
        for (i, p) in px.iter().enumerate() {
            let q = *qty.get(i).unwrap_or(&0.0);
            let key = OrderedFloat(*p);
            if q == 0.0 {
                book.remove(&key);
            } else {
                book.insert(key, q);
            }
        }
    }
    fn best_bid(&self) -> Option<f64> {
        self.bids.keys().rev().next().map(|k| k.0)
    }
    fn best_ask(&self) -> Option<f64> {
        self.asks.keys().next().map(|k| k.0)
    }
    fn top_n(&self, is_bid: bool, n: usize) -> (Vec<f64>, Vec<f64>) {
        let mut px = Vec::new();
        let mut qt = Vec::new();
        if is_bid {
            for (p, q) in self.bids.iter().rev().take(n) {
                px.push(p.0);
                qt.push(*q);
            }
        } else {
            for (p, q) in self.asks.iter().take(n) {
                px.push(p.0);
                qt.push(*q);
            }
        }
        (px, qt)
    }
}

// ----- ClickHouse rows and queries -----
#[allow(dead_code)]
#[derive(Deserialize, clickhouse::Row)]
struct SnapshotRow {
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

async fn load_latest_snapshot(
    ch: &ChClient,
    venue: &str,
    symbol: &str,
    start_us: u64,
) -> anyhow::Result<SnapshotRow> {
    let q = "SELECT ts, symbol, venue, last_id, bids_px, bids_qty, asks_px, asks_qty, source \
             FROM snapshot_books WHERE symbol = ? AND venue = ? AND ts <= ? \
             ORDER BY ts DESC LIMIT 1";
    let mut cursor = ch
        .query(q)
        .bind(symbol)
        .bind(venue)
        .bind(start_us)
        .fetch::<SnapshotRow>()?;
    if let Some(row) = cursor.next().await? {
        Ok(row)
    } else {
        anyhow::bail!("未找到起始快照")
    }
}

enum EventKind {
    Binance {
        bids_px: Vec<f64>,
        bids_qty: Vec<f64>,
        asks_px: Vec<f64>,
        asks_qty: Vec<f64>,
        u: u64,
        pu: u64,
    },
    Bitget {
        action: String,
        bids_px: Vec<f64>,
        bids_qty: Vec<f64>,
        asks_px: Vec<f64>,
        asks_qty: Vec<f64>,
        bids_px_s: Option<Vec<String>>,
        bids_qty_s: Option<Vec<String>>,
        asks_px_s: Option<Vec<String>>,
        asks_qty_s: Option<Vec<String>>,
        checksum: Option<i64>,
        price_scale: u64,
        qty_scale: u64,
    },
}

struct EventRow {
    event_ts: u64,
    kind: EventKind,
}

// 不再使用 EventStream 包裝，直接以 Cursor 讀取

#[derive(Deserialize, clickhouse::Row)]
struct EventRecord {
    event_ts: u64,
    #[allow(dead_code)]
    ingest_ts: u64,
    #[allow(dead_code)]
    symbol: String,
    // binance-specific
    #[allow(dead_code)]
    #[serde(rename = "U")]
    upper_u: Option<u64>,
    u: Option<u64>,
    pu: Option<u64>,
    bids_px: Vec<f64>,
    bids_qty: Vec<f64>,
    asks_px: Vec<f64>,
    asks_qty: Vec<f64>,
    // bitget-specific
    action: Option<String>,
    checksum: Option<i64>,
    price_scale: Option<u64>,
    qty_scale: Option<u64>,
    bids_px_s: Option<Vec<String>>,
    bids_qty_s: Option<Vec<String>>,
    asks_px_s: Option<Vec<String>>,
    asks_qty_s: Option<Vec<String>>,
}

fn to_event_row(venue: &str, r: EventRecord) -> EventRow {
    if venue.eq_ignore_ascii_case("binance") {
        EventRow {
            event_ts: r.event_ts,
            kind: EventKind::Binance {
                bids_px: r.bids_px,
                bids_qty: r.bids_qty,
                asks_px: r.asks_px,
                asks_qty: r.asks_qty,
                u: r.u.unwrap_or(0),
                pu: r.pu.unwrap_or(0),
            },
        }
    } else {
        EventRow {
            event_ts: r.event_ts,
            kind: EventKind::Bitget {
                action: r.action.unwrap_or_else(|| "update".into()),
                bids_px: r.bids_px,
                bids_qty: r.bids_qty,
                asks_px: r.asks_px,
                asks_qty: r.asks_qty,
                bids_px_s: r.bids_px_s,
                bids_qty_s: r.bids_qty_s,
                asks_px_s: r.asks_px_s,
                asks_qty_s: r.asks_qty_s,
                checksum: r.checksum,
                price_scale: r.price_scale.unwrap_or(1),
                qty_scale: r.qty_scale.unwrap_or(1),
            },
        }
    }
}

// ----- 校驗 & 插入工具 -----
fn compute_bitget_crc(
    bids_px_s: Option<&[String]>,
    bids_qty_s: Option<&[String]>,
    asks_px_s: Option<&[String]>,
    asks_qty_s: Option<&[String]>,
    bids_px: &[f64],
    bids_qty: &[f64],
    asks_px: &[f64],
    asks_qty: &[f64],
    ps: u64,
    qs: u64,
) -> u32 {
    // 若有原始字串，優先使用；否則使用 tick/lot 固定位數格式化
    let use_str =
        bids_px_s.is_some() && bids_qty_s.is_some() && asks_px_s.is_some() && asks_qty_s.is_some();
    let n = if use_str {
        usize::min(
            25,
            usize::min(
                usize::min(bids_px_s.unwrap().len(), bids_qty_s.unwrap().len()),
                usize::min(asks_px_s.unwrap().len(), asks_qty_s.unwrap().len()),
            ),
        )
    } else {
        usize::min(
            25,
            usize::min(
                bids_px.len(),
                usize::min(bids_qty.len(), usize::min(asks_px.len(), asks_qty.len())),
            ),
        )
    };

    let mut parts: Vec<String> = Vec::with_capacity(n * 4);
    if use_str {
        let bpxs = bids_px_s.unwrap();
        let bqts = bids_qty_s.unwrap();
        let apxs = asks_px_s.unwrap();
        let aqts = asks_qty_s.unwrap();
        for i in 0..n {
            parts.push(bpxs[i].clone());
            parts.push(bqts[i].clone());
            parts.push(apxs[i].clone());
            parts.push(aqts[i].clone());
        }
    } else {
        let pd = decimal_places_from_scale(ps);
        let qd = decimal_places_from_scale(qs);
        for i in 0..n {
            parts.push(format!("{:.*}", pd, bids_px[i]));
            parts.push(format!("{:.*}", qd, bids_qty[i]));
            parts.push(format!("{:.*}", pd, asks_px[i]));
            parts.push(format!("{:.*}", qd, asks_qty[i]));
        }
    }
    let s = parts.join(":");
    let mut hasher = Crc32::new();
    hasher.update(s.as_bytes());
    hasher.finalize()
}

fn decimal_places_from_scale(scale: u64) -> usize {
    if scale == 0 {
        return 0;
    }
    let mut s = scale;
    let mut d = 0;
    while s > 1 {
        s /= 10;
        d += 1;
    }
    d
}

#[derive(clickhouse::Row, serde::Serialize)]
struct GapRow {
    ts: u64,
    venue: String,
    symbol: String,
    reason: String,
    detail: String,
}

async fn insert_gap(
    ch: &ChClient,
    venue: &str,
    symbol: &str,
    reason: &str,
    detail: String,
) -> anyhow::Result<()> {
    let row = GapRow {
        ts: now_micros(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        reason: reason.to_string(),
        detail,
    };
    let mut ins = ch.insert::<GapRow>("gap_log")?;
    ins.write(&row).await?;
    ins.end().await?;
    Ok(())
}

#[derive(clickhouse::Row, serde::Serialize)]
struct LobDepthRow {
    timestamp: DateTime<Utc>,
    symbol: String,
    exchange: String,
    sequence: u64,
    is_snapshot: u8,
    bid_prices: Vec<f64>,
    bid_quantities: Vec<f64>,
    ask_prices: Vec<f64>,
    ask_quantities: Vec<f64>,
    bid_levels: u16,
    ask_levels: u16,
    spread: f64,
    mid_price: f64,
    data_source: String,
}

async fn insert_lob_depth(ch: &ChClient, row: LobDepthRow) -> anyhow::Result<()> {
    let mut ins = ch.insert::<LobDepthRow>("lob_depth")?;
    ins.write(&row).await?;
    ins.end().await?;
    Ok(())
}

fn now_micros() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

fn to_dt64(ts_ms: u64) -> DateTime<Utc> {
    // event_ts 是毫秒，轉為 DateTime64(6) 的秒+納秒
    let secs = (ts_ms / 1000) as i64;
    let nanos = ((ts_ms % 1000) * 1_000_000) as u32;
    DateTime::<Utc>::from_timestamp(secs, nanos).unwrap_or_else(|| DateTime::<Utc>::UNIX_EPOCH)
}
