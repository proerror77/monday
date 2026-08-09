use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use hft_collector::binance_fee_artifact::{
    publish_fee_snapshot, valid_binance_symbol, valid_runtime_account_id, BinanceFeeSnapshot,
    BinanceInstrumentRules, SideFeeBps, FEE_SCHEMA,
};
use integration::signing::{BinanceCredentials, BinanceSigner};
use rust_decimal::Decimal;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf, str::FromStr, time::Duration};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Market {
    Spot,
    Usdm,
}

impl Market {
    fn name(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Spot => "/api/v3/account/commission",
            Self::Usdm => "/fapi/v1/commissionRate",
        }
    }

    fn default_base(self) -> &'static str {
        match self {
            Self::Spot => "https://api.binance.com",
            Self::Usdm => "https://fapi.binance.com",
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "binance-fee-snapshot")]
struct Args {
    #[arg(long, value_enum)]
    market: Market,
    #[arg(long)]
    symbol: String,
    #[arg(long)]
    account_id: String,
    #[arg(long)]
    output_root: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let symbol = args.symbol.trim().to_ascii_uppercase();
    if !valid_binance_symbol(&symbol)
        || !valid_runtime_account_id(&args.account_id)
        || !args.output_root.is_absolute()
    {
        bail!("symbol, account id, and absolute output root are required");
    }
    let credentials = BinanceCredentials::new(
        required_env("HFT_SECRET_BINANCE_API_KEY")?,
        required_env("HFT_SECRET_BINANCE_SECRET")?,
    );
    let account_fingerprint = hex::encode(Sha256::digest(credentials.api_key.as_bytes()));
    let signer = BinanceSigner::new(credentials);
    let base = args.market.default_base();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let instrument_rules = match args.market {
        Market::Spot => {
            let payload: Value = client
                .get(format!("{base}/api/v3/exchangeInfo?symbol={symbol}"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await
                .context("Binance exchangeInfo response is invalid JSON")?;
            Some(parse_spot_rules(&payload, &symbol)?)
        }
        Market::Usdm => None,
    };
    let mut params = HashMap::from([
        ("symbol".to_string(), symbol.clone()),
        ("recvWindow".to_string(), "5000".to_string()),
    ]);
    let query = signer.sign_request(&mut params);
    let requested_at = Utc::now();
    let mut request = client.get(format!("{base}{}?{query}", args.market.endpoint()));
    for (name, value) in signer.generate_headers() {
        request = request.header(name, value);
    }
    let response = request.send().await?.error_for_status()?;
    let payload: Value = response
        .json()
        .await
        .context("Binance fee response is invalid JSON")?;
    validate_response_symbol(&payload, &symbol)?;
    let received_at = Utc::now();
    let (maker_fee_bps, taker_fee_bps, calculation) = parse_fees(args.market, &payload)?;
    let published = publish_fee_snapshot(
        &args.output_root,
        &BinanceFeeSnapshot {
            schema: FEE_SCHEMA.to_string(),
            venue: "binance".to_string(),
            market: args.market.name().to_string(),
            symbol,
            runtime_account_id: args.account_id,
            account_fingerprint,
            maker_fee_bps,
            taker_fee_bps,
            calculation,
            source_endpoint: args.market.endpoint().to_string(),
            instrument_rules,
            rules_source_endpoint: matches!(args.market, Market::Spot)
                .then(|| "/api/v3/exchangeInfo".to_string()),
            requested_at,
            received_at,
        },
    )?;
    serde_json::to_writer_pretty(
        std::io::stdout().lock(),
        &serde_json::json!({
            "data": published.data_path,
            "manifest": published.manifest_path,
            "success": published.success_path,
            "data_sha256": published.data_sha256,
            "manifest_sha256": published.manifest_sha256,
        }),
    )?;
    println!();
    Ok(())
}

fn required_env(name: &str) -> Result<String> {
    let value = std::env::var_os(name)
        .with_context(|| format!("{name} is required"))?
        .into_string()
        .map_err(|_| anyhow::anyhow!("{name} is not valid UTF-8"))?;
    if value.trim().is_empty() {
        bail!("{name} is empty");
    }
    Ok(value)
}

fn validate_response_symbol(payload: &Value, expected: &str) -> Result<()> {
    if payload["symbol"].as_str() != Some(expected) {
        bail!("Binance fee response symbol does not match the request");
    }
    Ok(())
}

fn parse_fees(market: Market, payload: &Value) -> Result<(SideFeeBps, SideFeeBps, String)> {
    match market {
        Market::Spot => Ok((
            SideFeeBps {
                buy: to_bps(spot_rate(payload, "maker", "buyer")?),
                sell: to_bps(spot_rate(payload, "maker", "seller")?),
            },
            SideFeeBps {
                buy: to_bps(spot_rate(payload, "taker", "buyer")?),
                sell: to_bps(spot_rate(payload, "taker", "seller")?),
            },
            "liquidity_plus_side_standard_special_tax_without_asset_discount".to_string(),
        )),
        Market::Usdm => {
            let maker = to_bps(decimal_field(payload, "makerCommissionRate")?);
            let taker = to_bps(decimal_field(payload, "takerCommissionRate")?);
            Ok((
                SideFeeBps {
                    buy: maker.clone(),
                    sell: maker,
                },
                SideFeeBps {
                    buy: taker.clone(),
                    sell: taker,
                },
                "account_commission_rate".to_string(),
            ))
        }
    }
}

fn spot_rate(payload: &Value, liquidity: &str, side: &str) -> Result<Decimal> {
    ["standardCommission", "specialCommission", "taxCommission"]
        .into_iter()
        .try_fold(Decimal::ZERO, |total, group| {
            Ok(total
                + decimal_field(&payload[group], liquidity)?
                + decimal_field(&payload[group], side)?)
        })
}

fn parse_spot_rules(payload: &Value, symbol: &str) -> Result<BinanceInstrumentRules> {
    let entry = payload["symbols"]
        .as_array()
        .and_then(|symbols| {
            symbols
                .iter()
                .find(|entry| entry["symbol"].as_str() == Some(symbol))
        })
        .context("Binance exchangeInfo has no requested symbol")?;
    if entry["status"] != "TRADING" {
        bail!("Binance Spot symbol is not trading");
    }
    let filters = entry["filters"]
        .as_array()
        .context("Binance exchangeInfo symbol has no filters")?;
    let filter = |kind: &str| {
        filters
            .iter()
            .find(|filter| filter["filterType"].as_str() == Some(kind))
            .with_context(|| format!("Binance exchangeInfo is missing {kind}"))
    };
    let price = filter("PRICE_FILTER")?;
    let lot = filter("LOT_SIZE")?;
    let notional = filter("NOTIONAL")?;
    Ok(BinanceInstrumentRules {
        tick_size: decimal_field(price, "tickSize")?.normalize().to_string(),
        step_size: decimal_field(lot, "stepSize")?.normalize().to_string(),
        min_notional: decimal_field(notional, "minNotional")?
            .normalize()
            .to_string(),
    })
}

fn decimal_field(value: &Value, field: &str) -> Result<Decimal> {
    Decimal::from_str(
        value[field]
            .as_str()
            .with_context(|| format!("Binance response is missing {field}"))?,
    )
    .with_context(|| format!("Binance response has invalid {field}"))
}

fn to_bps(rate: Decimal) -> String {
    (rate * Decimal::from(10_000)).normalize().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_spot_all_in_rates_without_assuming_discount_balance() {
        let payload = json!({
            "standardCommission": {"maker":"0.001", "taker":"0.001", "buyer":"0.0003", "seller":"0.0004"},
            "specialCommission": {"maker":"0.0001", "taker":"0.0002", "buyer":"0.00003", "seller":"0.00004"},
            "taxCommission": {"maker":"0.00001", "taker":"0.00002", "buyer":"0.000003", "seller":"0.000004"},
            "discount": {"enabledForAccount":true, "enabledForSymbol":true, "discount":"0.25"}
        });
        let (maker, taker, method) = parse_fees(Market::Spot, &payload).unwrap();
        assert_eq!(maker.buy, "14.43");
        assert_eq!(maker.sell, "15.54");
        assert_eq!(taker.buy, "15.53");
        assert_eq!(taker.sell, "16.64");
        assert!(method.contains("without_asset_discount"));
    }

    #[test]
    fn parses_usdm_account_rates() {
        let payload = json!({"makerCommissionRate":"0.0002", "takerCommissionRate":"0.0005"});
        let (maker, taker, _) = parse_fees(Market::Usdm, &payload).unwrap();
        assert_eq!(
            (maker.buy, maker.sell, taker.buy, taker.sell),
            ("2".into(), "2".into(), "5".into(), "5".into())
        );
    }

    #[test]
    fn parses_spot_trading_rules() {
        let payload = json!({"symbols":[{
            "symbol":"BTCUSDT", "status":"TRADING", "filters":[
                {"filterType":"PRICE_FILTER", "tickSize":"0.01000000"},
                {"filterType":"LOT_SIZE", "stepSize":"0.00001000"},
                {"filterType":"NOTIONAL", "minNotional":"5.00000000"}
            ]
        }]});
        assert_eq!(
            parse_spot_rules(&payload, "BTCUSDT").unwrap(),
            BinanceInstrumentRules {
                tick_size: "0.01".to_string(),
                step_size: "0.00001".to_string(),
                min_notional: "5".to_string(),
            }
        );
    }

    #[test]
    fn signed_requests_cannot_target_an_overridden_host_or_symbol() {
        let args = "binance-fee-snapshot --market spot --symbol BTCUSDT --output-root /tmp/fees --api-base https://example.com";
        assert!(Args::try_parse_from(args.split_whitespace()).is_err());
        assert!(!valid_binance_symbol("BTCUSDT&symbol=ETHUSDT"));
        assert!(validate_response_symbol(&json!({"symbol":"ETHUSDT"}), "BTCUSDT").is_err());
    }

    #[test]
    fn rejects_legacy_min_notional_filter() {
        let payload = json!({"symbols":[{
            "symbol":"BTCUSDT", "status":"TRADING", "filters":[
                {"filterType":"PRICE_FILTER", "tickSize":"0.01"},
                {"filterType":"LOT_SIZE", "stepSize":"0.00001"},
                {"filterType":"MIN_NOTIONAL", "minNotional":"5"}
            ]
        }]});
        assert!(parse_spot_rules(&payload, "BTCUSDT").is_err());
    }
}
