//! Binance Spot USDT Listing Monitor
//!
//! Monitors the Binance exchangeInfo API for new USDT spot pairs and sends Feishu notifications.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{error, info, warn};

use listing_monitor::feishu;

const EXCHANGE_INFO_API: &str = "https://api.binance.com/api/v3/exchangeInfo";
const POLL_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Deserialize)]
struct ExchangeInfo {
    symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Deserialize, Clone)]
struct SymbolInfo {
    symbol: String,
    status: String,
    #[serde(rename = "baseAsset")]
    base_asset: String,
    #[serde(rename = "quoteAsset")]
    quote_asset: String,
    #[serde(rename = "baseAssetPrecision")]
    base_asset_precision: Option<u8>,
    #[serde(rename = "quoteAssetPrecision")]
    quote_asset_precision: Option<u8>,
}

async fn fetch_usdt_symbols() -> Result<Vec<SymbolInfo>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let resp: ExchangeInfo = client
        .get(EXCHANGE_INFO_API)
        .header("User-Agent", "listing-monitor/1.0")
        .send()
        .await?
        .json()
        .await?;

    // Filter for USDT pairs that are TRADING
    let usdt_symbols: Vec<SymbolInfo> = resp
        .symbols
        .into_iter()
        .filter(|s| s.status == "TRADING")
        .filter(|s| s.quote_asset == "USDT")
        .collect();

    Ok(usdt_symbols)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("listing_monitor=info".parse()?)
                .add_directive("listing_monitor_spot=info".parse()?),
        )
        .init();

    info!("Starting Binance Spot USDT Listing Monitor");
    info!("Poll interval: {} seconds", POLL_INTERVAL_SECS);

    let mut known_symbols: HashSet<String> = HashSet::new();
    let mut first_run = true;

    loop {
        match fetch_usdt_symbols().await {
            Ok(symbols) => {
                let current_symbols: HashSet<String> =
                    symbols.iter().map(|s| s.symbol.clone()).collect();

                let new_symbols: Vec<&SymbolInfo> = symbols
                    .iter()
                    .filter(|s| !known_symbols.contains(&s.symbol))
                    .collect();

                if !new_symbols.is_empty() {
                    if first_run {
                        info!(
                            "Initial load: {} USDT spot pairs found",
                            current_symbols.len()
                        );
                    } else {
                        info!("Detected {} new USDT spot pair(s)!", new_symbols.len());

                        // Build notification content
                        let content = new_symbols
                            .iter()
                            .map(|s| {
                                format!(
                                    "**{}**\nBase: {} | Quote: {}\nPrecision: {} / {}",
                                    s.symbol,
                                    s.base_asset,
                                    s.quote_asset,
                                    s.base_asset_precision.unwrap_or(8),
                                    s.quote_asset_precision.unwrap_or(8)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n---\n\n");

                        // Send Feishu notification
                        if let Err(e) = feishu::send_alert(
                            &format!("Binance Spot New Listing ({})", new_symbols.len()),
                            &content,
                        )
                        .await
                        {
                            error!("Failed to send Feishu notification: {}", e);
                        }
                    }
                }

                // Check for delisted symbols
                if !first_run {
                    let delisted: Vec<&String> = known_symbols
                        .iter()
                        .filter(|s| !current_symbols.contains(*s))
                        .collect();

                    if !delisted.is_empty() {
                        warn!("Detected {} delisted symbol(s): {:?}", delisted.len(), delisted);

                        let content = delisted
                            .iter()
                            .map(|s| format!("**{}** - Delisted", s))
                            .collect::<Vec<_>>()
                            .join("\n");

                        if let Err(e) = feishu::send_alert(
                            &format!("Binance Spot Delisted ({})", delisted.len()),
                            &content,
                        )
                        .await
                        {
                            error!("Failed to send Feishu notification: {}", e);
                        }
                    }
                }

                known_symbols = current_symbols;
                first_run = false;
            }
            Err(e) => {
                error!("Failed to fetch exchange info: {}", e);
            }
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}
