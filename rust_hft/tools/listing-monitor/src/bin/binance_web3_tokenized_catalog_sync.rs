//! Generate the complete Binance Web3 Ondo tokenized-securities catalog.
//!
//! This is a reference-asset catalog, not a Spot order universe. Binance's
//! public RWA endpoint exposes contract metadata and reference prices, but no
//! executable order book; callers must not turn these entries into synthetic BBOs.

use anyhow::{bail, Result};
use futures::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

const TOKEN_LIST_API: &str = "https://www.binance.com/bapi/defi/v1/public/wallet-direct/buw/wallet/market/token/rwa/stock/detail/list/ai?type=1";
const DYNAMIC_API: &str = "https://www.binance.com/bapi/defi/v2/public/wallet-direct/buw/wallet/market/token/rwa/dynamic/ai";
const USER_AGENT: &str = "binance-web3/1.1 (monday-catalog-sync)";
const DYNAMIC_REQUEST_CONCURRENCY: usize = 16;

#[derive(Debug, Deserialize)]
struct ApiResponse {
    code: String,
    success: bool,
    data: Vec<TokenInfo>,
}

#[derive(Debug, Deserialize)]
struct DynamicApiResponse {
    code: String,
    success: bool,
    data: DynamicData,
}

#[derive(Debug, Deserialize)]
struct DynamicData {
    #[serde(rename = "tokenInfo")]
    token_info: DynamicTokenInfo,
    #[serde(rename = "statusInfo")]
    status_info: DynamicStatusInfo,
}

#[derive(Debug, Deserialize)]
struct DynamicTokenInfo {
    price: String,
    #[serde(rename = "sharesMultiplier")]
    shares_multiplier: String,
}

#[derive(Debug, Deserialize)]
struct DynamicStatusInfo {
    #[serde(rename = "openState")]
    open_state: Option<bool>,
    #[serde(rename = "marketStatus")]
    market_status: Option<String>,
    #[serde(rename = "reasonCode")]
    reason_code: Option<String>,
    #[serde(rename = "reasonMsg")]
    reason_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenInfo {
    #[serde(rename = "chainId")]
    chain_id: String,
    #[serde(rename = "contractAddress")]
    contract_address: String,
    symbol: String,
    ticker: String,
    #[serde(rename = "type")]
    platform_type: u8,
    #[serde(rename = "assetType", default)]
    asset_type: Option<u8>,
    multiplier: String,
    #[serde(rename = "d")]
    decimals: u8,
}

#[derive(Debug, Deserialize, Serialize)]
struct GeneratedCatalog {
    generated_at: String,
    source: String,
    provider: String,
    reference_only: bool,
    underlying_ticker_count: usize,
    instruments_sha256: String,
    instruments: Vec<GeneratedInstrument>,
}

#[derive(Debug, Deserialize, Serialize)]
struct GeneratedInstrument {
    ticker: String,
    symbol: String,
    venue: String,
    asset_class: String,
    product_type: String,
    regulatory_profile: String,
    chain_id: String,
    contract_address: String,
    platform_type: u8,
    asset_type: String,
    multiplier: String,
    decimals: u8,
    reference_only: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReferenceSnapshot {
    observed_at: String,
    source: String,
    catalog_instruments_sha256: String,
    reference_only: bool,
    instruments_sha256: String,
    instruments: Vec<ReferenceInstrument>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReferenceInstrument {
    ticker: String,
    symbol: String,
    chain_id: String,
    contract_address: String,
    reference_price_usd: String,
    shares_multiplier: String,
    market_open: Option<bool>,
    market_status: Option<String>,
    reason_code: Option<String>,
    reason_message: Option<String>,
    reference_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let tokens = fetch_tokens().await?;
    let catalog = build_catalog(tokens.clone())?;
    if let Some(path) = std::env::args().find_map(|argument| {
        argument
            .strip_prefix("--verify-reference-against=")
            .map(ToOwned::to_owned)
    }) {
        let bytes = std::fs::read(&path)?;
        let pinned_sha256 = std::fs::read_to_string(format!("{path}.sha256"))?;
        listing_monitor::integrity::verify_sha256_hex(&bytes, &pinned_sha256)
            .map_err(anyhow::Error::msg)?;
        let checked_in: ReferenceSnapshot = serde_yaml::from_slice(&bytes)?;
        verify_reference_snapshot(&checked_in)?;
        if checked_in.catalog_instruments_sha256 != catalog.instruments_sha256 {
            bail!(
                "Binance Web3 reference snapshot is linked to a stale catalog; regenerate {path}"
            );
        }
        let max_age_seconds = std::env::args()
            .find_map(|argument| {
                argument
                    .strip_prefix("--max-reference-age-seconds=")
                    .map(str::parse::<i64>)
            })
            .transpose()?
            .unwrap_or(300);
        if max_age_seconds <= 0 {
            bail!("--max-reference-age-seconds must be positive");
        }
        let observed_at = chrono::DateTime::parse_from_rfc3339(&checked_in.observed_at)?
            .with_timezone(&chrono::Utc);
        let age_seconds = chrono::Utc::now()
            .signed_duration_since(observed_at)
            .num_seconds();
        if age_seconds > max_age_seconds {
            bail!(
                "Binance Web3 reference snapshot is stale ({age_seconds}s old; max {max_age_seconds}s); regenerate {path}"
            );
        }
        return Ok(());
    }
    if let Some(path) = std::env::args().find_map(|argument| {
        argument
            .strip_prefix("--verify-against=")
            .map(ToOwned::to_owned)
    }) {
        let bytes = std::fs::read(&path)?;
        let pinned_sha256 = std::fs::read_to_string(format!("{path}.sha256"))?;
        listing_monitor::integrity::verify_sha256_hex(&bytes, &pinned_sha256)
            .map_err(anyhow::Error::msg)?;
        let checked_in: GeneratedCatalog = serde_yaml::from_slice(&bytes)?;
        verify_catalog(&checked_in)?;
        if checked_in.instruments_sha256 != catalog.instruments_sha256 {
            bail!("Binance Web3 RWA catalog is stale; regenerate {path}");
        }
        return Ok(());
    }
    if std::env::args().any(|argument| argument == "--with-reference-status") {
        let snapshot = fetch_reference_snapshot(tokens, &catalog.instruments_sha256).await?;
        println!("{}", serde_yaml::to_string(&snapshot)?);
    } else {
        println!("{}", serde_yaml::to_string(&catalog)?);
    }
    Ok(())
}

async fn fetch_tokens() -> Result<Vec<TokenInfo>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let response: ApiResponse = client
        .get(TOKEN_LIST_API)
        .header("Accept-Encoding", "identity")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if !response.success || response.code != "000000" {
        bail!(
            "Binance Web3 RWA catalog request failed: code={}, success={}",
            response.code,
            response.success
        );
    }
    if response.data.is_empty() {
        bail!("Binance Web3 RWA catalog response was empty");
    }
    Ok(response.data)
}

fn build_catalog(mut tokens: Vec<TokenInfo>) -> Result<GeneratedCatalog> {
    tokens.sort_by(|left, right| {
        (&left.ticker, &left.chain_id, &left.contract_address).cmp(&(
            &right.ticker,
            &right.chain_id,
            &right.contract_address,
        ))
    });
    let underlying_ticker_count = tokens
        .iter()
        .map(|token| token.ticker.as_str())
        .collect::<BTreeSet<_>>()
        .len();

    let mut catalog = GeneratedCatalog {
        generated_at: chrono::Utc::now().to_rfc3339(),
        source: TOKEN_LIST_API.to_string(),
        provider: "Ondo Finance via Binance Web3 public API".to_string(),
        reference_only: true,
        underlying_ticker_count,
        instruments_sha256: String::new(),
        instruments: tokens.into_iter().map(to_generated_instrument).collect(),
    };
    catalog.instruments_sha256 = instruments_digest(&catalog.instruments)?;
    Ok(catalog)
}

fn instruments_digest(instruments: &[GeneratedInstrument]) -> Result<String> {
    let canonical = serde_yaml::to_string(instruments)?;
    Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
}

fn verify_catalog(catalog: &GeneratedCatalog) -> Result<()> {
    let expected = instruments_digest(&catalog.instruments)?;
    if catalog.instruments_sha256 != expected {
        bail!("Binance Web3 catalog instruments_sha256 does not match its contents");
    }
    Ok(())
}

fn preferred_reference_tokens(tokens: Vec<TokenInfo>) -> Vec<TokenInfo> {
    let mut selected = BTreeMap::new();
    for token in tokens {
        match selected.entry(token.ticker.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(token);
            }
            std::collections::btree_map::Entry::Occupied(mut entry)
                if entry.get().chain_id != "56" && token.chain_id == "56" =>
            {
                entry.insert(token);
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    selected.into_values().collect()
}

async fn fetch_reference_snapshot(
    tokens: Vec<TokenInfo>,
    catalog_instruments_sha256: &str,
) -> Result<ReferenceSnapshot> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut instruments: Vec<ReferenceInstrument> =
        stream::iter(preferred_reference_tokens(tokens))
            .map(|token| {
                let client = client.clone();
                async move { fetch_reference_instrument(&client, token).await }
            })
            .buffer_unordered(DYNAMIC_REQUEST_CONCURRENCY)
            .try_collect()
            .await?;
    instruments.sort_by(|left, right| left.ticker.cmp(&right.ticker));

    let mut snapshot = ReferenceSnapshot {
        observed_at: chrono::Utc::now().to_rfc3339(),
        source: DYNAMIC_API.to_string(),
        catalog_instruments_sha256: catalog_instruments_sha256.to_string(),
        reference_only: true,
        instruments_sha256: String::new(),
        instruments,
    };
    snapshot.instruments_sha256 = reference_instruments_digest(&snapshot.instruments)?;
    Ok(snapshot)
}

fn reference_instruments_digest(instruments: &[ReferenceInstrument]) -> Result<String> {
    let canonical = serde_yaml::to_string(instruments)?;
    Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
}

fn verify_reference_snapshot(snapshot: &ReferenceSnapshot) -> Result<()> {
    let expected = reference_instruments_digest(&snapshot.instruments)?;
    if snapshot.instruments_sha256 != expected {
        bail!("Binance Web3 reference snapshot instruments_sha256 does not match its contents");
    }
    Ok(())
}

async fn fetch_reference_instrument(
    client: &reqwest::Client,
    token: TokenInfo,
) -> Result<ReferenceInstrument> {
    let response: DynamicApiResponse = client
        .get(DYNAMIC_API)
        .query(&[
            ("chainId", token.chain_id.as_str()),
            ("contractAddress", token.contract_address.as_str()),
        ])
        .header("Accept-Encoding", "identity")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !response.success || response.code != "000000" {
        bail!(
            "Binance Web3 dynamic request failed for {} on {}: code={}, success={}",
            token.ticker,
            token.chain_id,
            response.code,
            response.success
        );
    }

    Ok(ReferenceInstrument {
        ticker: token.ticker,
        symbol: token.symbol,
        chain_id: token.chain_id,
        contract_address: token.contract_address,
        reference_price_usd: response.data.token_info.price,
        shares_multiplier: response.data.token_info.shares_multiplier,
        market_open: response.data.status_info.open_state,
        market_status: response.data.status_info.market_status,
        reason_code: response.data.status_info.reason_code,
        reason_message: response.data.status_info.reason_message,
        reference_only: true,
    })
}

fn to_generated_instrument(token: TokenInfo) -> GeneratedInstrument {
    GeneratedInstrument {
        ticker: token.ticker,
        symbol: token.symbol,
        venue: "BINANCE_WEB3_TOKENIZED_SECURITIES".to_string(),
        asset_class: "TokenizedSecurity".to_string(),
        product_type: "ReferenceAsset".to_string(),
        regulatory_profile: "RestrictedJurisdiction".to_string(),
        chain_id: token.chain_id,
        contract_address: token.contract_address,
        platform_type: token.platform_type,
        asset_type: asset_type_name(token.asset_type).to_string(),
        multiplier: token.multiplier,
        decimals: token.decimals,
        reference_only: true,
    }
}

fn asset_type_name(asset_type: Option<u8>) -> &'static str {
    match asset_type {
        Some(1) => "Stock",
        Some(3) => "ETF",
        None => "Unknown",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_every_chain_deployment_for_each_underlying_ticker() {
        let catalog = build_catalog(vec![
            TokenInfo {
                chain_id: "56".into(),
                contract_address: "0xbsc".into(),
                symbol: "AAPLon".into(),
                ticker: "AAPL".into(),
                platform_type: 1,
                asset_type: Some(1),
                multiplier: "1.01".into(),
                decimals: 18,
            },
            TokenInfo {
                chain_id: "1".into(),
                contract_address: "0xeth".into(),
                symbol: "AAPLon".into(),
                ticker: "AAPL".into(),
                platform_type: 1,
                asset_type: Some(1),
                multiplier: "1.01".into(),
                decimals: 18,
            },
            TokenInfo {
                chain_id: "CT_501".into(),
                contract_address: "solana-address".into(),
                symbol: "AAPLon".into(),
                ticker: "AAPL".into(),
                platform_type: 1,
                asset_type: Some(1),
                multiplier: "1.01".into(),
                decimals: 18,
            },
        ])
        .unwrap();

        assert_eq!(catalog.underlying_ticker_count, 1);
        assert_eq!(catalog.instruments.len(), 3);
        assert_eq!(
            catalog
                .instruments
                .iter()
                .map(|instrument| instrument.chain_id.as_str())
                .collect::<Vec<_>>(),
            vec!["1", "56", "CT_501"]
        );
        assert!(catalog.instruments.iter().all(|instrument| {
            instrument.venue == "BINANCE_WEB3_TOKENIZED_SECURITIES"
                && instrument.asset_class == "TokenizedSecurity"
                && instrument.product_type == "ReferenceAsset"
                && instrument.reference_only
        }));
        verify_catalog(&catalog).unwrap();
    }

    #[test]
    fn retains_new_listings_when_binance_omits_the_asset_type() {
        let catalog = build_catalog(vec![TokenInfo {
            chain_id: "56".into(),
            contract_address: "0xmissing-type".into(),
            symbol: "SKHYon".into(),
            ticker: "SKHY".into(),
            platform_type: 1,
            asset_type: None,
            multiplier: "1".into(),
            decimals: 18,
        }])
        .unwrap();

        assert_eq!(catalog.instruments[0].asset_type, "Unknown");
    }

    #[test]
    fn chooses_bsc_for_dynamic_reference_price_when_multiple_chains_exist() {
        let selected = preferred_reference_tokens(vec![
            TokenInfo {
                chain_id: "1".into(),
                contract_address: "0xeth".into(),
                symbol: "AAPLon".into(),
                ticker: "AAPL".into(),
                platform_type: 1,
                asset_type: Some(1),
                multiplier: "1".into(),
                decimals: 18,
            },
            TokenInfo {
                chain_id: "56".into(),
                contract_address: "0xbsc".into(),
                symbol: "AAPLon".into(),
                ticker: "AAPL".into(),
                platform_type: 1,
                asset_type: Some(1),
                multiplier: "1".into(),
                decimals: 18,
            },
        ]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chain_id, "56");
    }

    #[test]
    fn checked_in_catalog_is_the_full_reference_universe_not_a_spot_subset() {
        let catalog_bytes = include_bytes!(
            "../../../../config/generated/binance_web3_ondo_tokenized_reference_assets.yaml"
        );
        listing_monitor::integrity::verify_sha256_hex(
            catalog_bytes,
            include_str!(
                "../../../../config/generated/binance_web3_ondo_tokenized_reference_assets.yaml.sha256"
            ),
        )
        .expect("checked-in Binance Web3 catalog raw bytes must match its SHA-256 sidecar");
        let catalog: GeneratedCatalog = serde_yaml::from_slice(catalog_bytes)
            .expect("checked-in Binance Web3 catalog must be valid YAML");

        assert!(catalog.reference_only);
        verify_catalog(&catalog)
            .expect("checked-in Binance Web3 catalog digest must match contents");
        assert_eq!(
            catalog.underlying_ticker_count,
            catalog
                .instruments
                .iter()
                .map(|instrument| instrument.ticker.as_str())
                .collect::<BTreeSet<_>>()
                .len()
        );
        assert_eq!(
            catalog
                .instruments
                .iter()
                .map(|instrument| (&instrument.chain_id, &instrument.contract_address))
                .collect::<BTreeSet<_>>()
                .len(),
            catalog.instruments.len()
        );
        assert!(catalog.instruments.iter().all(|instrument| {
            instrument.venue == "BINANCE_WEB3_TOKENIZED_SECURITIES"
                && instrument.asset_class == "TokenizedSecurity"
                && instrument.product_type == "ReferenceAsset"
                && instrument.regulatory_profile == "RestrictedJurisdiction"
                && instrument.reference_only
        }));
    }

    #[test]
    fn checked_in_reference_snapshot_matches_the_catalog_and_is_not_a_bbo_feed() {
        let catalog_bytes = include_bytes!(
            "../../../../config/generated/binance_web3_ondo_tokenized_reference_assets.yaml"
        );
        listing_monitor::integrity::verify_sha256_hex(
            catalog_bytes,
            include_str!(
                "../../../../config/generated/binance_web3_ondo_tokenized_reference_assets.yaml.sha256"
            ),
        )
        .expect("checked-in Binance Web3 catalog raw bytes must match its SHA-256 sidecar");
        let catalog: GeneratedCatalog = serde_yaml::from_slice(catalog_bytes)
            .expect("checked-in Binance Web3 catalog must be valid YAML");
        let snapshot_bytes = include_bytes!(
            "../../../../config/generated/binance_web3_ondo_reference_snapshot.yaml"
        );
        listing_monitor::integrity::verify_sha256_hex(
            snapshot_bytes,
            include_str!(
                "../../../../config/generated/binance_web3_ondo_reference_snapshot.yaml.sha256"
            ),
        )
        .expect("checked-in Binance Web3 snapshot raw bytes must match its SHA-256 sidecar");
        let snapshot: ReferenceSnapshot = serde_yaml::from_slice(snapshot_bytes)
            .expect("checked-in Binance Web3 reference snapshot must be valid YAML");

        verify_catalog(&catalog).unwrap();
        verify_reference_snapshot(&snapshot)
            .expect("checked-in Binance Web3 reference snapshot digest must match contents");
        assert!(snapshot.reference_only);
        assert_eq!(
            snapshot.catalog_instruments_sha256,
            catalog.instruments_sha256
        );
        assert_eq!(
            snapshot
                .instruments
                .iter()
                .map(|instrument| instrument.ticker.as_str())
                .collect::<BTreeSet<_>>(),
            catalog
                .instruments
                .iter()
                .map(|instrument| instrument.ticker.as_str())
                .collect::<BTreeSet<_>>()
        );
        assert!(snapshot.instruments.iter().all(|instrument| {
            !instrument.reference_price_usd.is_empty()
                && !instrument.shares_multiplier.is_empty()
                && instrument.reference_only
        }));
    }
}
