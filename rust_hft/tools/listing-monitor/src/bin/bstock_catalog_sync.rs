//! Generate Binance tokenized securities instrument metadata from exchangeInfo.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

#[derive(Debug, Deserialize, Serialize)]
struct GeneratedCatalog {
    generated_at: String,
    source: String,
    instruments_sha256: String,
    instruments: Vec<GeneratedInstrument>,
}

#[derive(Debug, Deserialize, Serialize)]
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
    let symbols = parse_symbol_filter(&args).ok_or_else(|| {
        anyhow::anyhow!(
            "bStock catalog sync requires --symbols=SYMBOL,... because exchangeInfo has no reliable asset-class field"
        )
    })?;
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
        .filter(|symbol| symbols.contains(&symbol.symbol))
        .map(to_generated_instrument)
        .collect();

    let mut catalog = GeneratedCatalog {
        generated_at: chrono::Utc::now().to_rfc3339(),
        source: EXCHANGE_INFO_API.to_string(),
        instruments_sha256: String::new(),
        instruments,
    };
    catalog.instruments_sha256 = instruments_digest(&catalog.instruments)?;

    if let Some(path) = parse_arg_value(&args, "--verify-against=") {
        let bytes = fs::read(path)?;
        let pinned_sha256 = fs::read_to_string(format!("{path}.sha256"))?;
        listing_monitor::integrity::verify_sha256_hex(&bytes, &pinned_sha256)
            .map_err(anyhow::Error::msg)?;
        let checked_in: GeneratedCatalog = serde_yaml::from_slice(&bytes)?;
        verify_catalog(&checked_in)?;
        if checked_in.instruments_sha256 != catalog.instruments_sha256 {
            anyhow::bail!("bStock catalog is stale; regenerate {path}");
        }
        return Ok(());
    }

    println!("{}", serde_yaml::to_string(&catalog)?);
    Ok(())
}

fn instruments_digest(instruments: &[GeneratedInstrument]) -> Result<String> {
    let canonical = serde_yaml::to_string(instruments)?;
    Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
}

fn verify_catalog(catalog: &GeneratedCatalog) -> Result<()> {
    let expected = instruments_digest(&catalog.instruments)?;
    if catalog.instruments_sha256 != expected {
        anyhow::bail!("bStock catalog instruments_sha256 does not match its contents");
    }
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
    fn checked_in_catalog_covers_the_quotes_only_bstock_universe() {
        let bytes = include_bytes!("../../../../config/generated/binance_bstocks_instruments.yaml");
        listing_monitor::integrity::verify_sha256_hex(
            bytes,
            include_str!("../../../../config/generated/binance_bstocks_instruments.yaml.sha256"),
        )
        .expect("checked-in bStock catalog raw bytes must match its SHA-256 sidecar");
        let catalog: GeneratedCatalog =
            serde_yaml::from_slice(bytes).expect("checked-in bStock catalog must be valid YAML");
        verify_catalog(&catalog).expect("checked-in bStock catalog digest must match contents");

        let symbols: BTreeSet<_> = catalog
            .instruments
            .iter()
            .map(|instrument| instrument.symbol.as_str())
            .collect();
        let quotes_only_config: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../../../config/dev/binance_bstocks_quotes_only.yaml"
        ))
        .expect("checked-in bStock quotes-only config must be valid YAML");
        let configured_symbols: BTreeSet<_> = quotes_only_config["venues"][0]["symbol_catalog"]
            .as_sequence()
            .expect("bStock quotes-only config must declare a symbol catalog")
            .iter()
            .map(|symbol| {
                symbol
                    .as_str()
                    .expect("bStock symbol must be a string")
                    .split_once('@')
                    .expect("bStock symbol must include its venue")
                    .0
            })
            .collect();
        assert_eq!(symbols, configured_symbols);
        assert!(catalog.instruments.iter().all(|instrument| {
            instrument.venue == "BINANCE_TOKENIZED_SECURITIES"
                && instrument.asset_class == "TokenizedSecurity"
                && instrument.product_type == "TokenizedSecuritySpot"
                && instrument.regulatory_profile == "AdgmTokenizedSecurity"
                && instrument.status == "TRADING"
        }));
    }
}
