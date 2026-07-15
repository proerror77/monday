//! Signal attribution tool for PM5D reversal research.
//!
//! Usage:
//!   cargo run --release -p ploy-strategy-bundles --example signal_attribution -- \
//!     --db-url postgresql://user:pass@host/ploy \
//!     --start-date 2026-04-01 \
//!     --end-date 2026-04-10 \
//!     --symbols BTCUSDT,DOGEUSDT \
//!     --output /tmp/attribution.csv

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::traits::MarketUpdate;
use rust_decimal::prelude::ToPrimitive;
use sqlx::postgres::PgPoolOptions;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Write;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start timestamp"))
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59).expect("valid end timestamp"))
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
        .with_timezone(&Utc)
}

struct DriftBuffer {
    entries: VecDeque<(DateTime<Utc>, f64)>,
    window_secs: f64,
}

impl DriftBuffer {
    fn new(window_secs: f64) -> Self {
        Self {
            entries: VecDeque::new(),
            window_secs,
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        self.entries.push_back((ts, price.ln()));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().expect("front exists").0;
            let elapsed = (ts - oldest).num_milliseconds() as f64 / 1000.0;
            if elapsed > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn drift_speed(&self) -> f64 {
        if self.entries.len() < 2 {
            return 0.0;
        }
        let (t0, p0) = self.entries.front().expect("front exists");
        let (t1, p1) = self.entries.back().expect("back exists");
        let dt = ((*t1 - *t0).num_milliseconds() as f64 / 1000.0).max(0.001);
        (p1 - p0) / dt
    }
}

#[derive(Default)]
struct EventState {
    event_id: String,
    symbol: String,
    end_time: Option<DateTime<Utc>>,
    price_to_beat: Option<f64>,
    resolved_up_won: Option<bool>,
    up_token: String,
    down_token: String,
}

#[derive(Default)]
struct LobState {
    obi: f64,
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });
    let symbols_csv =
        flag_value(&args, "--symbols").unwrap_or_else(|| "BTCUSDT,DOGEUSDT".to_string());
    let output_path =
        flag_value(&args, "--output").unwrap_or_else(|| "/tmp/attribution.csv".to_string());
    let symbols: Vec<String> = symbols_csv
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    eprintln!(
        "loading attribution window {start} -> {end} for {:?}",
        symbols
    );

    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let updates = load_from_database_with_options(
        &pool,
        &symbols,
        start,
        end,
        &HistoricalLoadOptions {
            require_official_settlement: true,
            ..Default::default()
        },
    )
    .await
    .expect("historical load failed");

    eprintln!("loaded {} updates", updates.len());

    let mut out = File::create(&output_path).expect("failed to create output file");
    writeln!(
        out,
        "event_id,symbol,price_to_beat,tick_ts,time_remaining_secs,spot_price,distance_to_beat_pct,drift_speed_30s,drift_speed_10s,drift_direction_flipped,drift_flip_age_secs,obi,spread_bps,lob_bid_depth_near,lob_ask_depth_near,pm_up_ask,pm_down_ask,pm_ask_lag_secs,outcome_up_won"
    )
    .expect("failed to write csv header");

    let mut buf_30s: HashMap<String, DriftBuffer> = HashMap::new();
    let mut buf_10s: HashMap<String, DriftBuffer> = HashMap::new();
    let mut prev_drift_30s: HashMap<String, f64> = HashMap::new();
    let mut drift_flip_ts: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();
    let mut events: HashMap<String, EventState> = HashMap::new();
    let mut quotes: HashMap<String, (DateTime<Utc>, f64)> = HashMap::new();
    let mut lob: HashMap<String, LobState> = HashMap::new();
    let mut rows_written = 0usize;

    for update in &updates {
        match update {
            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                price_to_beat,
                resolved_up_won,
                ..
            } => {
                events.insert(
                    event_id.to_string(),
                    EventState {
                        event_id: event_id.to_string(),
                        symbol: symbol.to_string(),
                        end_time: Some(*end_time),
                        price_to_beat: price_to_beat.and_then(|price| price.to_f64()),
                        resolved_up_won: *resolved_up_won,
                        up_token: up_token.to_string(),
                        down_token: down_token.to_string(),
                    },
                );
            }
            MarketUpdate::EventExpired {
                event_id,
                resolved_up_won,
                ..
            } => {
                if let Some(event) = events.get_mut(event_id.as_ref()) {
                    event.resolved_up_won = resolved_up_won.or(event.resolved_up_won);
                }
            }
            MarketUpdate::Quote {
                token_id, ask, ts, ..
            } => {
                if let Some(ask_price) = ask.and_then(|value| value.to_f64()) {
                    quotes.insert(token_id.to_string(), (*ts, ask_price));
                }
            }
            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ..
            } => {
                let state = lob.entry(symbol.to_string()).or_default();
                state.obi = *obi;
                state.spread_bps = *spread_bps;
            }
            MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ..
            } => {
                lob.insert(
                    symbol.to_string(),
                    LobState {
                        obi: *obi,
                        spread_bps: *spread_bps,
                        bid_depth_near: *bid_depth_near,
                        ask_depth_near: *ask_depth_near,
                    },
                );
            }
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                let symbol = symbol.as_ref();
                let Some(spot_price) = price.to_f64() else {
                    continue;
                };

                buf_30s
                    .entry(symbol.to_string())
                    .or_insert_with(|| DriftBuffer::new(30.0))
                    .push(*ts, spot_price);
                buf_10s
                    .entry(symbol.to_string())
                    .or_insert_with(|| DriftBuffer::new(10.0))
                    .push(*ts, spot_price);

                let drift_30 = buf_30s.get(symbol).expect("buffer exists").drift_speed();
                let drift_10 = buf_10s.get(symbol).expect("buffer exists").drift_speed();
                let previous = prev_drift_30s.get(symbol).copied().unwrap_or(0.0);
                let flipped =
                    previous != 0.0 && drift_30 != 0.0 && previous.signum() != drift_30.signum();
                if flipped {
                    drift_flip_ts.insert(symbol.to_string(), Some(*ts));
                }
                prev_drift_30s.insert(symbol.to_string(), drift_30);

                for event in events.values() {
                    if event.symbol != symbol {
                        continue;
                    }
                    let Some(end_time) = event.end_time else {
                        continue;
                    };
                    let Some(price_to_beat) = event.price_to_beat else {
                        continue;
                    };
                    let time_remaining_secs = (end_time - *ts).num_seconds();
                    if time_remaining_secs < 0 {
                        continue;
                    }

                    let flip_age_secs = drift_flip_ts
                        .get(symbol)
                        .and_then(|ts_opt| *ts_opt)
                        .map(|flip_ts| (*ts - flip_ts).num_seconds() as f64)
                        .unwrap_or(-1.0);
                    let direction_flipped = flip_age_secs >= 0.0 && flip_age_secs < 60.0;
                    let distance_to_beat_pct = (spot_price - price_to_beat) / price_to_beat;

                    let (pm_up_ask, pm_ask_lag_secs) = quotes
                        .get(&event.up_token)
                        .map(|(quote_ts, ask)| (*ask, (*ts - *quote_ts).num_seconds() as f64))
                        .unwrap_or((-1.0, -1.0));
                    let (pm_down_ask, _) = quotes
                        .get(&event.down_token)
                        .map(|(quote_ts, ask)| (*ask, (*ts - *quote_ts).num_seconds() as f64))
                        .unwrap_or((-1.0, -1.0));
                    let default_lob = LobState::default();
                    let lob_state = lob.get(symbol).unwrap_or(&default_lob);
                    let outcome = event
                        .resolved_up_won
                        .map(|won| if won { "1" } else { "0" })
                        .unwrap_or("");

                    writeln!(
                        out,
                        "{},{},{:.6},{},{},{:.6},{:.6},{:.8},{:.8},{},{:.1},{:.8},{},{:.4},{:.4},{:.4},{:.4},{:.1},{}",
                        event.event_id,
                        symbol,
                        price_to_beat,
                        ts.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                        time_remaining_secs,
                        spot_price,
                        distance_to_beat_pct,
                        drift_30,
                        drift_10,
                        if direction_flipped { 1 } else { 0 },
                        flip_age_secs,
                        lob_state.obi,
                        lob_state.spread_bps,
                        lob_state.bid_depth_near,
                        lob_state.ask_depth_near,
                        pm_up_ask,
                        pm_down_ask,
                        pm_ask_lag_secs,
                        outcome,
                    )
                    .expect("failed to write csv row");
                    rows_written += 1;
                }
            }
            _ => {}
        }
    }

    eprintln!("wrote {} rows to {}", rows_written, output_path);
}
