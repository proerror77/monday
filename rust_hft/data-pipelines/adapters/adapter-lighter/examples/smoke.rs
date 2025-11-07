use adapter_lighter_data::LighterMarketStream;
use futures::StreamExt;
use hft_core::Symbol;
use ports::MarketStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SYMBOLS=BTCUSDT,ETHUSDT
    let symbols_env = std::env::var("SYMBOLS").unwrap_or_else(|_| "BTCUSDT".to_string());
    let symbols: Vec<Symbol> = symbols_env
        .split(',')
        .map(|s| Symbol::from(s.trim().to_string()))
        .collect();

    println!("Lighter smoke test - symbols: {:?}", symbols);
    if let Ok(rest) = std::env::var("LIGHTER_REST") {
        println!("REST endpoint: {}", rest);
    }
    if let Ok(ws) = std::env::var("LIGHTER_WS_PUBLIC") {
        println!("WS endpoint: {}", ws);
    }

    let mut stream = LighterMarketStream::new();
    stream.connect().await?;
    let mut s = stream.subscribe(symbols).await?;

    let mut count = 0u32;
    let max_events = std::env::var("MAX_EVENTS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(5);
    let timeout_ms = std::env::var("TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(15000);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let t = tokio::time::timeout_at(deadline, s.next()).await;
        match t {
            Ok(Some(Ok(ev))) => {
                match &ev {
                    ports::MarketEvent::Snapshot(sn) => {
                        println!(
                            "SNAPSHOT {} bids:{} asks:{} ts:{}",
                            sn.symbol.as_str(),
                            sn.bids.len(),
                            sn.asks.len(),
                            sn.timestamp
                        );
                    }
                    ports::MarketEvent::Update(upd) => {
                        println!(
                            "UPDATE {} Δbids:{} Δasks:{} ts:{}",
                            upd.symbol.as_str(),
                            upd.bids.len(),
                            upd.asks.len(),
                            upd.timestamp
                        );
                    }
                    ports::MarketEvent::Trade(tr) => {
                        println!(
                            "TRADE {} {}@{} ts:{}",
                            tr.symbol.as_str(),
                            tr.quantity,
                            tr.price,
                            tr.timestamp
                        );
                    }
                    ports::MarketEvent::Disconnect { reason } => {
                        eprintln!("DISCONNECT: {}", reason);
                        break;
                    }
                    _ => {}
                }
                count += 1;
                if count >= max_events {
                    break;
                }
            }
            Ok(Some(Err(e))) => {
                eprintln!("ERROR: {}", e);
                break;
            }
            Ok(None) => {
                eprintln!("Stream ended");
                break;
            }
            Err(_) => {
                eprintln!("Timeout waiting for events ({} ms)", timeout_ms);
                break;
            }
        }
    }
    Ok(())
}
