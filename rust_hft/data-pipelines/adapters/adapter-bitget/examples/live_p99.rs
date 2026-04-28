use data_adapter_bitget::{
    parse_bitget_orderbook_snapshot, parse_bitget_trade_event, parse_bitget_ws_envelope,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const DEFAULT_WS_URL: &str = "wss://ws.bitget.com/v2/ws/public";

#[derive(Default)]
struct LatencyStats {
    envelope_ns: Vec<u64>,
    event_ns: Vec<u64>,
    total_ns: Vec<u64>,
    inter_arrival_ns: Vec<u64>,
    books: u64,
    trades: u64,
    ignored: u64,
}

impl LatencyStats {
    fn record(
        &mut self,
        envelope_ns: u64,
        event_ns: u64,
        total_ns: u64,
        inter_arrival_ns: Option<u64>,
        channel: &str,
    ) {
        self.envelope_ns.push(envelope_ns);
        self.event_ns.push(event_ns);
        self.total_ns.push(total_ns);
        if let Some(value) = inter_arrival_ns {
            self.inter_arrival_ns.push(value);
        }

        if channel.starts_with("books") {
            self.books += 1;
        } else if channel == "trade" || channel == "trades" {
            self.trades += 1;
        }
    }

    fn print(&mut self) {
        println!(
            "live stats: samples={} books={} trades={} ignored={}",
            self.total_ns.len(),
            self.books,
            self.trades,
            self.ignored
        );
        print_percentiles("live envelope_parse", &mut self.envelope_ns);
        print_percentiles("live event_convert", &mut self.event_ns);
        print_percentiles("live total_local", &mut self.total_ns);
        print_percentiles("live inter_arrival", &mut self.inter_arrival_ns);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls_023::crypto::ring::default_provider().install_default();

    let args = Args::from_env();
    let subscribe = json!({
        "op": "subscribe",
        "args": [
            {
                "instType": args.inst_type,
                "channel": args.depth_channel,
                "instId": args.symbol,
            },
            {
                "instType": args.inst_type,
                "channel": "trade",
                "instId": args.symbol,
            }
        ]
    });

    println!(
        "connecting Bitget live: url={} instType={} symbol={} depth_channel={} max_messages={} max_runtime_secs={}",
        args.ws_url,
        args.inst_type,
        args.symbol,
        args.depth_channel,
        args.max_messages,
        args.max_runtime_secs
    );

    let (mut ws, _) = connect_async(&args.ws_url).await?;
    ws.send(Message::Text(subscribe.to_string().into())).await?;
    ws.send(Message::Text("ping".into())).await?;

    let started = Instant::now();
    let mut last_message_at: Option<Instant> = None;
    let mut last_ping_at = Instant::now();
    let mut stats = LatencyStats::default();

    while stats.total_ns.len() < args.max_messages as usize
        && started.elapsed() < Duration::from_secs(args.max_runtime_secs)
    {
        if last_ping_at.elapsed() >= Duration::from_secs(25) {
            ws.send(Message::Text("ping".into())).await?;
            last_ping_at = Instant::now();
        }

        let message = match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(_) => continue,
        };

        let recv_at = Instant::now();
        let inter_arrival_ns = last_message_at.map(|previous| {
            recv_at
                .saturating_duration_since(previous)
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64
        });
        last_message_at = Some(recv_at);

        let text = match message? {
            Message::Text(text) => text,
            Message::Ping(payload) => {
                ws.send(Message::Pong(payload)).await?;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(_) => break,
            _ => continue,
        };

        if text == "pong" {
            continue;
        }

        let envelope = match parse_bitget_ws_envelope(&text) {
            Ok(envelope) => envelope,
            Err(_) => {
                stats.ignored += 1;
                continue;
            }
        };
        let envelope_done = Instant::now();

        let Some(arg) = envelope.arg else {
            stats.ignored += 1;
            continue;
        };

        let converted = if arg.channel.starts_with("books") || arg.channel == "depth" {
            parse_bitget_orderbook_snapshot(&text).map(|event| event.is_some())
        } else if arg.channel == "trade" || arg.channel == "trades" {
            parse_bitget_trade_event(&text).map(|event| event.is_some())
        } else {
            stats.ignored += 1;
            continue;
        };
        let event_done = Instant::now();

        if !converted.unwrap_or(false) {
            stats.ignored += 1;
            continue;
        }

        stats.record(
            nanos_between(recv_at, envelope_done),
            nanos_between(envelope_done, event_done),
            nanos_between(recv_at, event_done),
            inter_arrival_ns,
            arg.channel,
        );
    }

    stats.print();
    Ok(())
}

struct Args {
    ws_url: String,
    inst_type: String,
    symbol: String,
    depth_channel: String,
    max_messages: u64,
    max_runtime_secs: u64,
}

impl Args {
    fn from_env() -> Self {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self {
            ws_url: DEFAULT_WS_URL.to_string(),
            inst_type: "SPOT".to_string(),
            symbol: "BTCUSDT".to_string(),
            depth_channel: "books1".to_string(),
            max_messages: 500,
            max_runtime_secs: 30,
        };

        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--ws-url" => parsed.ws_url = take_value(&mut args, &flag),
                "--inst-type" => parsed.inst_type = take_value(&mut args, &flag),
                "--symbol" => parsed.symbol = take_value(&mut args, &flag),
                "--depth-channel" => parsed.depth_channel = take_value(&mut args, &flag),
                "--max-messages" => {
                    parsed.max_messages = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --max-messages")
                }
                "--max-runtime-secs" => {
                    parsed.max_runtime_secs = take_value(&mut args, &flag)
                        .parse()
                        .expect("valid --max-runtime-secs")
                }
                "--help" | "-h" => {
                    print_help_and_exit();
                }
                other => panic!("unknown argument: {other}"),
            }
        }

        parsed
    }
}

fn take_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("missing value for {flag}"))
}

fn print_help_and_exit() -> ! {
    println!(
        "Usage: cargo run -p hft-data-adapter-bitget --example live_p99 --release -- \\
  [--symbol BTCUSDT] [--inst-type SPOT] [--depth-channel books1] \\
  [--max-messages 500] [--max-runtime-secs 30]"
    );
    std::process::exit(0);
}

fn nanos_between(start: Instant, end: Instant) -> u64 {
    end.saturating_duration_since(start)
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn print_percentiles(label: &str, samples: &mut [u64]) {
    if samples.is_empty() {
        println!("{label}: no samples");
        return;
    }

    samples.sort_unstable();
    println!(
        "{label}: p50={}ns p95={}ns p99={}ns p999={}ns max={}ns count={}",
        percentile(samples, 0.50),
        percentile(samples, 0.95),
        percentile(samples, 0.99),
        percentile(samples, 0.999),
        samples[samples.len() - 1],
        samples.len()
    );
}

fn percentile(samples: &[u64], q: f64) -> u64 {
    let index = ((samples.len() - 1) as f64 * q).ceil() as usize;
    samples[index.min(samples.len() - 1)]
}
