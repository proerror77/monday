//! Generate Binance tokenized securities instrument metadata from exchangeInfo.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::time::Duration;

const EXCHANGE_INFO_API: &str = "https://api.binance.com/api/v3/exchangeInfo";

#[derive(Debug, Deserialize)]
struct ExchangeInfo {
    symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Deserialize)]
struct SymbolInfo {
    symbol: String,
    status: String,
    #[serde(rename = "baseAsset")]
    base_asset: String,
    #[serde(rename = "quoteAsset")]
    quote_asset: String,
    filters: Vec<Filter>,
    #[serde(default)]
    permissions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Filter {
    #[serde(rename = "filterType")]
    filter_type: String,
    #[serde(flatten)]
    values: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct GeneratedCatalog {
    generated_at: String,
    source: String,
    instruments: Vec<GeneratedInstrument>,
}

#[derive(Debug, Serialize)]
struct GeneratedInstrument {
    symbol: String,
    venue: String,
    base: String,
    quote: String,
    asset_class: String,
    product_type: String,
    regulatory_profile: String,
    underlying_symbol: String,
    issuer: String,
    tick_size: String,
    lot_size: String,
    min_qty: Option<String>,
    min_notional: Option<String>,
    status: String,
    permissions: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let symbols = parse_symbol_filter(&args);
    let exchange_info = if let Some(path) = parse_arg_value(&args, "--input=") {
        serde_json::from_str(&fs::read_to_string(path)?)?
    } else {
        fetch_exchange_info().await?
    };

    let instruments: Vec<_> = exchange_info
        .symbols
        .into_iter()
        .filter(|symbol| symbol.status == "TRADING")
        .filter(|symbol| symbol.quote_asset == "USDT")
        .filter(|symbol| {
            symbols
                .as_ref()
                .map(|selected| selected.contains(&symbol.symbol))
                .unwrap_or_else(|| looks_like_bstock(&symbol.base_asset))
        })
        .map(to_generated_instrument)
        .collect();

    let catalog = GeneratedCatalog {
        generated_at: chrono::Utc::now().to_rfc3339(),
        source: EXCHANGE_INFO_API.to_string(),
        instruments,
    };

    println!("{}", serde_yaml::to_string(&catalog)?);
    Ok(())
}

fn parse_symbol_filter(args: &[String]) -> Option<BTreeSet<String>> {
    parse_arg_value(args, "--symbols=").map(|value| {
            value
                .split(',')
                .map(|symbol| symbol.trim().to_ascii_uppercase())
                .filter(|symbol| !symbol.is_empty())
                .collect()
    })
}

fn parse_arg_value<'a>(args: &'a [String], prefix: &str) -> Option<&'a str> {
    args.iter().find_map(|arg| arg.strip_prefix(prefix))
}

async fn fetch_exchange_info() -> Result<ExchangeInfo> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    Ok(client
        .get(EXCHANGE_INFO_API)
        .header("User-Agent", "binance-bstock-catalog-sync/1.0")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn looks_like_bstock(base_asset: &str) -> bool {
    // Binance's first bStocks use a trailing B ticker pattern: CRCLB, NVDAB, TSLAB.
    // ponytail: heuristic only; replace with an official asset-class field if Binance exposes one.
    base_asset.len() > 2 && base_asset.ends_with('B')
}

fn to_generated_instrument(symbol: SymbolInfo) -> GeneratedInstrument {
    let tick_size = filter_value(&symbol.filters, "PRICE_FILTER", "tickSize")
        .unwrap_or_else(|| "0.01".to_string());
    let lot_size =
        filter_value(&symbol.filters, "LOT_SIZE", "stepSize").unwrap_or_else(|| "1".to_string());
    let min_qty = filter_value(&symbol.filters, "LOT_SIZE", "minQty");
    let min_notional = filter_value(&symbol.filters, "MIN_NOTIONAL", "minNotional")
        .or_else(|| filter_value(&symbol.filters, "NOTIONAL", "minNotional"));

    GeneratedInstrument {
        underlying_symbol: symbol
            .base_asset
            .strip_suffix('B')
            .unwrap_or(&symbol.base_asset)
            .to_string(),
        issuer: symbol.base_asset.clone(),
        symbol: symbol.symbol,
        venue: "BINANCE_TOKENIZED_SECURITIES".to_string(),
        base: symbol.base_asset,
        quote: symbol.quote_asset,
        asset_class: "TokenizedSecurity".to_string(),
        product_type: "TokenizedSecuritySpot".to_string(),
        regulatory_profile: "AdgmTokenizedSecurity".to_string(),
        tick_size,
        lot_size,
        min_qty,
        min_notional,
        status: symbol.status,
        permissions: symbol.permissions,
    }
}

fn filter_value(filters: &[Filter], filter_type: &str, key: &str) -> Option<String> {
    filters
        .iter()
        .find(|filter| filter.filter_type == filter_type)
        .and_then(|filter| filter.values.get(key))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bstock_heuristic_matches_current_symbol_pattern() {
        assert!(looks_like_bstock("TSLAB"));
        assert!(looks_like_bstock("NVDAB"));
        assert!(!looks_like_bstock("BTC"));
        assert!(!looks_like_bstock("B"));
    }
}
