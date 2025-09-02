use clap::{Parser, Subcommand};
use anyhow::Result;
use tracing::info;
use clickhouse::Client as ChClient;
use ordered_float::OrderedFloat;
use std::collections::BTreeMap;

#[derive(Parser, Debug)]
#[command(author, version, about = "Dataset tooling: grid/export", long_about = None)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Grid {
        #[arg(long, default_value = "binance")] venue: String,
        #[arg(long)] symbol: String,
        #[arg(long)] start_us: u64,
        #[arg(long)] end_us: u64,
        #[arg(long, default_value_t = 100)] step_ms: u64,
        #[arg(long, default_value_t = 20)] k: usize,
        #[arg(long, default_value = "http://localhost:8123")] ch_url: String,
        #[arg(long, default_value = "hft")] database: String,
    },
    Export {
        #[arg(long, default_value = "binance")] venue: String,
        #[arg(long)] symbol: String,
        #[arg(long)] start_us: u64,
        #[arg(long)] end_us: u64,
        #[arg(long, default_value_t = 100)] step_ms: u32,
        #[arg(long, default_value_t = 20)] k: usize,
        #[arg(long, default_value_t = 100)] L: usize,
        #[arg(long, default_value_t = 1000)] H_ms: u32,
        #[arg(long, default_value_t = 1.0)] tau_ticks: f64,
        #[arg(long, default_value = "http://localhost:8123")] ch_url: String,
        #[arg(long, default_value = "hft")] database: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();
    match args.cmd {
        Cmd::Grid { venue, symbol, start_us, end_us, step_ms, k, ch_url, database } => {
            run_grid(&venue, &symbol, start_us, end_us, step_ms, k, &ch_url, &database).await?
        }
        Cmd::Export { venue, symbol, start_us, end_us, step_ms, k, L, H_ms, tau_ticks, ch_url, database } => {
            run_export(&venue, &symbol, start_us, end_us, step_ms, k, L, H_ms, tau_ticks, &ch_url, &database).await?
        }
    }
    Ok(())
}

#[derive(Default)]
struct Book { bids: BTreeMap<OrderedFloat<f64>, f64>, asks: BTreeMap<OrderedFloat<f64>, f64> }

impl Book {
    fn apply(&mut self, bpx: &[f64], bqt: &[f64], apx: &[f64], aqt: &[f64]) {
        for (i, p) in bpx.iter().enumerate() {
            let q = *bqt.get(i).unwrap_or(&0.0);
            let key = OrderedFloat(*p);
            if q == 0.0 { self.bids.remove(&key); } else { self.bids.insert(key, q); }
        }
        for (i, p) in apx.iter().enumerate() {
            let q = *aqt.get(i).unwrap_or(&0.0);
            let key = OrderedFloat(*p);
            if q == 0.0 { self.asks.remove(&key); } else { self.asks.insert(key, q); }
        }
    }
    fn topk(&self, k: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut bpx = Vec::new(); let mut bqt = Vec::new();
        let mut apx = Vec::new(); let mut aqt = Vec::new();
        for (p,q) in self.bids.iter().rev().take(k) { bpx.push(p.0); bqt.push(*q); }
        for (p,q) in self.asks.iter().take(k) { apx.push(p.0); aqt.push(*q); }
        (bpx,bqt,apx,aqt)
    }
    fn mid(&self) -> Option<f64> {
        let bb = self.bids.iter().rev().next().map(|(p,_)| p.0)?;
        let ba = self.asks.iter().next().map(|(p,_)| p.0)?;
        Some((bb+ba)/2.0)
    }
}

#[derive(clickhouse::Row, serde::Serialize, serde::Deserialize)]
struct GridRow { ts: u64, symbol: String, venue: String, k: u16, bid_px: Vec<f64>, bid_qty: Vec<f64>, ask_px: Vec<f64>, ask_qty: Vec<f64> }

async fn run_grid(venue: &str, symbol: &str, start_us: u64, end_us: u64, step_ms: u64, k: usize, ch_url: &str, database: &str) -> Result<()> {
    let ch = ChClient::default().with_url(ch_url).with_database(database);
    let (table, order) = if venue.eq_ignore_ascii_case("binance") {
        ("raw_depth_binance", "ORDER BY event_ts ASC, u ASC")
    } else { ("raw_books_bitget", "ORDER BY event_ts ASC, seq ASC") };
    let start_ms = start_us/1000; let end_ms = end_us/1000;

    let mut book = Book::default();
    let mut grid_ts = start_us;
    let step_us = step_ms * 1000;
    let mut rows: Vec<GridRow> = Vec::new();

    if venue.eq_ignore_ascii_case("binance") {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct BinanceRow { event_ts: u64, bids_px: Vec<f64>, bids_qty: Vec<f64>, asks_px: Vec<f64>, asks_qty: Vec<f64>, u: Option<u64>, pu: Option<u64> }
        let q = format!("SELECT event_ts, bids_px, bids_qty, asks_px, asks_qty, u, pu FROM {} WHERE symbol = ? AND event_ts >= ? AND event_ts <= ? {}", table, order);
        let mut cur = ch.query(&q).bind(symbol).bind(start_ms).bind(end_ms).fetch::<BinanceRow>()?;
        while let Some(rr) = cur.next().await? {
            let event_ts = rr.event_ts;
            let ev_us = event_ts.saturating_mul(1000);
            while grid_ts <= ev_us && grid_ts <= end_us {
                let (bpx,bqt,apx,aqt) = book.topk(k);
                rows.push(GridRow { ts: grid_ts, symbol: symbol.to_string(), venue: venue.to_string(), k: k as u16, bid_px: bpx, bid_qty: bqt, ask_px: apx, ask_qty: aqt });
                grid_ts = grid_ts.saturating_add(step_us);
                if rows.len() >= 1000 { flush_grid(&ch, &mut rows).await?; }
            }
            book.apply(&rr.bids_px, &rr.bids_qty, &rr.asks_px, &rr.asks_qty);
        }
    } else {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct BitgetRow { event_ts: u64, action: Option<String>, bids_px: Vec<f64>, bids_qty: Vec<f64>, asks_px: Vec<f64>, asks_qty: Vec<f64> }
        let q = format!("SELECT event_ts, action, bids_px, bids_qty, asks_px, asks_qty FROM {} WHERE symbol = ? AND event_ts >= ? AND event_ts <= ? {}", table, order);
        let mut cur = ch.query(&q).bind(symbol).bind(start_ms).bind(end_ms).fetch::<BitgetRow>()?;
        while let Some(rr) = cur.next().await? {
            let event_ts = rr.event_ts;
            let ev_us = event_ts.saturating_mul(1000);
            while grid_ts <= ev_us && grid_ts <= end_us {
                let (bpx,bqt,apx,aqt) = book.topk(k);
                rows.push(GridRow { ts: grid_ts, symbol: symbol.to_string(), venue: venue.to_string(), k: k as u16, bid_px: bpx, bid_qty: bqt, ask_px: apx, ask_qty: aqt });
                grid_ts = grid_ts.saturating_add(step_us);
                if rows.len() >= 1000 { flush_grid(&ch, &mut rows).await?; }
            }
            // snapshot/update 都直接套用
            book.apply(&rr.bids_px, &rr.bids_qty, &rr.asks_px, &rr.asks_qty);
        }
    }
    // 收尾：直到 end_us
    while grid_ts <= end_us {
        let (bpx,bqt,apx,aqt) = book.topk(k);
        rows.push(GridRow { ts: grid_ts, symbol: symbol.to_string(), venue: venue.to_string(), k: k as u16, bid_px: bpx, bid_qty: bqt, ask_px: apx, ask_qty: aqt });
        grid_ts = grid_ts.saturating_add(step_us);
        if rows.len() >= 1000 { flush_grid(&ch, &mut rows).await?; }
    }
    if !rows.is_empty() { flush_grid(&ch, &mut rows).await?; }
    info!("grid 完成: {} {}", venue, symbol);
    Ok(())
}

async fn flush_grid(ch: &ChClient, rows: &mut Vec<GridRow>) -> Result<()> {
    if rows.is_empty() { return Ok(()); }
    let mut ins = ch.insert::<GridRow>("lob_grid")?;
    for r in rows.iter() { ins.write(r).await?; }
    ins.end().await?;
    rows.clear();
    Ok(())
}

#[derive(clickhouse::Row, serde::Serialize)]
struct SampleRow {
    ts: u64, symbol: String, venue: String, step_ms: u32, k: u16, L: u16, H_ms: u32, tau_ticks: f64,
    bid_px_rel: Vec<f32>, bid_qty_log: Vec<f32>, ask_px_rel: Vec<f32>, ask_qty_log: Vec<f32>, update_flag: Vec<u8>,
    label: i8, mid0: f64, midH: f64
}

async fn run_export(venue: &str, symbol: &str, start_us: u64, end_us: u64, step_ms: u32, k: usize, L: usize, H_ms: u32, tau_ticks: f64, ch_url: &str, database: &str) -> Result<()> {
    let ch = ChClient::default().with_url(ch_url).with_database(database);
    // 讀取 lob_grid
    #[derive(clickhouse::Row, serde::Deserialize)]
    struct GridReadRow { ts: u64, bid_px: Vec<f64>, bid_qty: Vec<f64>, ask_px: Vec<f64>, ask_qty: Vec<f64> }
    let q = "SELECT ts, bid_px, bid_qty, ask_px, ask_qty FROM lob_grid WHERE symbol = ? AND venue = ? AND ts >= ? AND ts <= ? ORDER BY ts ASC";
    let mut cur = ch.query(q).bind(symbol).bind(venue).bind(start_us).bind(end_us).fetch::<GridReadRow>()?;
    let mut ts: Vec<u64> = Vec::new();
    let mut best_mid: Vec<f64> = Vec::new();
    let mut bids_px: Vec<Vec<f64>> = Vec::new();
    let mut bids_qty: Vec<Vec<f64>> = Vec::new();
    let mut asks_px: Vec<Vec<f64>> = Vec::new();
    let mut asks_qty: Vec<Vec<f64>> = Vec::new();
    while let Some(rr) = cur.next().await? {
        let t: u64 = rr.ts; ts.push(t);
        let bpx = rr.bid_px; let bqt = rr.bid_qty;
        let apx = rr.ask_px; let aqt = rr.ask_qty;
        let bb = bpx.iter().copied().max_by(|a,b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let ba = apx.iter().copied().min_by(|a,b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = match (bb, ba) { (Some(b), Some(a)) => (a+b)/2.0, _ => f64::NAN };
        best_mid.push(mid);
        bids_px.push(truncate_pad(&bpx, k)); bids_qty.push(truncate_pad(&bqt, k));
        asks_px.push(truncate_pad(&apx, k)); asks_qty.push(truncate_pad(&aqt, k));
    }
    let step_us = step_ms as u64 * 1000;
    let h_steps = (H_ms as u64 + step_ms as u64 - 1) / step_ms as u64; // 四捨五入
    let mut rows: Vec<SampleRow> = Vec::new();
    let tick = estimate_tick(&bids_px, &asks_px).unwrap_or(0.01);

    let mut i = 0usize;
    while i + L + (h_steps as usize) < ts.len() {
        let mut mid0 = best_mid[i+L-1];
        if !mid0.is_finite() { i+=1; continue; }
        let midH = best_mid[i+L-1+(h_steps as usize)];
        if !midH.is_finite() { i+=1; continue; }
        let tau = tau_ticks * tick;
        let label = if midH > mid0 + tau { 1 } else if midH < mid0 - tau { -1 } else { 0 };

        let mut bid_px_rel = Vec::with_capacity(L*k);
        let mut bid_qty_log = Vec::with_capacity(L*k);
        let mut ask_px_rel = Vec::with_capacity(L*k);
        let mut ask_qty_log = Vec::with_capacity(L*k);
        let mut update_flag = Vec::with_capacity(L);
        for t in 0..L {
            let idx = i + t;
            let mid = best_mid[idx];
            let (bpx, bqt) = (&bids_px[idx], &bids_qty[idx]);
            let (apx, aqt) = (&asks_px[idx], &asks_qty[idx]);
            bid_px_rel.extend(bpx.iter().map(|p| (*p - mid) as f32));
            ask_px_rel.extend(apx.iter().map(|p| (*p - mid) as f32));
            bid_qty_log.extend(bqt.iter().map(|q| ((1.0+*q).ln()) as f32));
            ask_qty_log.extend(aqt.iter().map(|q| ((1.0+*q).ln()) as f32));
            let upd = if t==0 {1u8} else { // 粗略：若與前一格不同則 1
                let pidx = idx-1;
                if bids_px[idx] != bids_px[pidx] || asks_px[idx] != asks_px[pidx] {1} else {0}
            };
            update_flag.push(upd);
        }
        rows.push(SampleRow{ ts: ts[i], symbol: symbol.to_string(), venue: venue.to_string(), step_ms, k: k as u16, L: L as u16, H_ms, tau_ticks,
            bid_px_rel, bid_qty_log, ask_px_rel, ask_qty_log, update_flag, label, mid0, midH });
        if rows.len() >= 200 { flush_samples(&ch, &mut rows).await?; }
        i += 1;
    }
    if !rows.is_empty() { flush_samples(&ch, &mut rows).await?; }
    info!("export 完成: {} {}", venue, symbol);
    Ok(())
}

fn truncate_pad(v: &Vec<f64>, k: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(k);
    for i in 0..k { out.push(*v.get(i).unwrap_or(&0.0)); }
    out
}

fn estimate_tick(bpx: &Vec<Vec<f64>>, apx: &Vec<Vec<f64>>) -> Option<f64> {
    // 估算最常見的價差步長
    let mut last = None; let mut diffs = Vec::new();
    for i in 0..bpx.len() {
        let bb = bpx[i].get(0).copied().unwrap_or(0.0);
        let ba = apx[i].get(0).copied().unwrap_or(0.0);
        if bb>0.0 && ba>0.0 { last = Some((bb, ba)); break; }
    }
    for i in 1..bpx.len() {
        let bb = bpx[i].get(0).copied().unwrap_or(0.0);
        let ba = apx[i].get(0).copied().unwrap_or(0.0);
        if bb>0.0 && ba>0.0 { if let Some((pbb,pba)) = last { diffs.push((bb-pbb).abs()); diffs.push((ba-pba).abs()); last=Some((bb,ba)); } }
    }
    diffs.sort_by(|a,b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    diffs.iter().find(|d| **d>0.0).copied()
}

async fn flush_samples(ch: &ChClient, rows: &mut Vec<SampleRow>) -> Result<()> {
    if rows.is_empty() { return Ok(()); }
    let mut ins = ch.insert::<SampleRow>("dataset_samples")?;
    for r in rows.iter() { ins.write(r).await?; }
    ins.end().await?;
    rows.clear();
    Ok(())
}
