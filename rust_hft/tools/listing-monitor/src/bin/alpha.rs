//! Binance Alpha Token Listing Monitor
//!
//! Monitors the Binance Alpha API for new token listings and sends Feishu notifications.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{error, info, warn};

use listing_monitor::feishu;

const ALPHA_API: &str =
    "https://www.binance.com/bapi/defi/v1/public/wallet-direct/buw/wallet/cex/alpha/all/token/list";
const POLL_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Deserialize)]
struct AlphaResponse {
    code: String,
    data: Option<Vec<AlphaToken>>,
    message: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct AlphaToken {
    #[serde(rename = "alphaId")]
    alpha_id: Option<String>,
    symbol: Option<String>,
    name: Option<String>,
    #[serde(rename = "chainId")]
    chain_id: Option<String>,
    #[serde(rename = "contractAddress")]
    contract_address: Option<String>,
}

impl AlphaToken {
    fn display_id(&self) -> String {
        self.alpha_id.clone().unwrap_or_else(|| "unknown".to_string())
    }

    fn display_symbol(&self) -> String {
        self.symbol.clone().unwrap_or_else(|| "N/A".to_string())
    }

    fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| "N/A".to_string())
    }

    fn display_chain(&self) -> String {
        self.chain_id.clone().unwrap_or_else(|| "N/A".to_string())
    }
}

async fn fetch_alpha_tokens() -> Result<Vec<AlphaToken>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let resp: AlphaResponse = client
        .get(ALPHA_API)
        .header("User-Agent", "listing-monitor/1.0")
        .send()
        .await?
        .json()
        .await?;

    if resp.code != "000000" {
        warn!(
            "Alpha API returned non-success code: {} - {:?}",
            resp.code, resp.message
        );
    }

    Ok(resp.data.unwrap_or_default())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("listing_monitor=info".parse()?)
                .add_directive("listing_monitor_alpha=info".parse()?),
        )
        .init();

    info!("Starting Binance Alpha Token Monitor");
    info!("Poll interval: {} seconds", POLL_INTERVAL_SECS);

    let mut known_tokens: HashSet<String> = HashSet::new();
    let mut first_run = true;

    loop {
        match fetch_alpha_tokens().await {
            Ok(tokens) => {
                let current_ids: HashSet<String> =
                    tokens.iter().map(|t| t.display_id()).collect();

                let new_tokens: Vec<&AlphaToken> = tokens
                    .iter()
                    .filter(|t| !known_tokens.contains(&t.display_id()))
                    .collect();

                if !new_tokens.is_empty() {
                    if first_run {
                        info!(
                            "Initial load: {} Alpha tokens found",
                            current_ids.len()
                        );
                    } else {
                        info!("Detected {} new Alpha token(s)!", new_tokens.len());

                        // Build notification content
                        let content = new_tokens
                            .iter()
                            .map(|t| {
                                format!(
                                    "**{}** ({})\nChain: {}\nID: {}",
                                    t.display_symbol(),
                                    t.display_name(),
                                    t.display_chain(),
                                    t.display_id()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n---\n\n");

                        // Send Feishu notification
                        if let Err(e) = feishu::send_alert(
                            &format!("Binance Alpha New Listing ({})", new_tokens.len()),
                            &content,
                        )
                        .await
                        {
                            error!("Failed to send Feishu notification: {}", e);
                        }
                    }
                }

                known_tokens = current_ids;
                first_run = false;
            }
            Err(e) => {
                error!("Failed to fetch Alpha tokens: {}", e);
            }
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}
