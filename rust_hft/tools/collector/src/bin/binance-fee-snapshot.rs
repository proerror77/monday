use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use hft_collector::binance_fee_artifact::{
    publish_fee_snapshot, BinanceFeeSnapshot, BinanceInstrumentRules, FEE_SCHEMA,
};
use integration::signing::{BinanceCredentials, BinanceSigner};
use rust_decimal::Decimal;
use serde_json::Value;
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
    output_root: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let symbol = args.symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() || !args.output_root.is_absolute() {
        bail!("symbol and absolute output root are required");
    }
    let credentials = BinanceCredentials::new(
        required_env("HFT_SECRET_BINANCE_API_KEY")?,
        required_env("HFT_SECRET_BINANCE_SECRET")?,
    );
    let signer = BinanceSigner::new(credentials);
    let mut params = HashMap::from([
        ("symbol".to_string(), symbol.clone()),
        ("recvWindow".to_string(), "5000".to_string()),
    ]);
    let query = signer.sign_request(&mut params);
    let base = args.market.default_base();
    let requested_at = Utc::now();
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
    let value = std::env::var(name).with_context(|| format!("{name} is required"))?;
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

fn parse_fees(market: Market, payload: &Value) -> Result<(String, String, String)> {
    match market {
        Market::Spot => {
            let maker = spot_rate(payload, "maker")?;
            let taker = spot_rate(payload, "taker")?;
            Ok((
                to_bps(maker),
                to_bps(taker),
                "standard_plus_special_plus_tax_without_asset_discount".to_string(),
            ))
        }
        Market::Usdm => Ok((
            to_bps(decimal_field(payload, "makerCommissionRate")?),
            to_bps(decimal_field(payload, "takerCommissionRate")?),
            "account_commission_rate".to_string(),
        )),
    }
}

fn spot_rate(payload: &Value, side: &str) -> Result<Decimal> {
    ["standardCommission", "specialCommission", "taxCommission"]
        .into_iter()
        .try_fold(Decimal::ZERO, |total, group| {
            Ok(total + decimal_field(&payload[group], side)?)
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
    let notional = filter("NOTIONAL").or_else(|_| filter("MIN_NOTIONAL"))?;
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
            .with_context(|| format!("Binance fee response is missing {field}"))?,
    )
    .with_context(|| format!("Binance fee response has invalid {field}"))
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
            "standardCommission": {"maker":"0.001", "taker":"0.001"},
            "specialCommission": {"maker":"0.0001", "taker":"0.0002"},
            "taxCommission": {"maker":"0.00001", "taker":"0.00002"},
            "discount": {"enabledForAccount":true, "enabledForSymbol":true, "discount":"0.25"}
        });
        let (maker, taker, method) = parse_fees(Market::Spot, &payload).unwrap();
        assert_eq!(maker, "11.1");
        assert_eq!(taker, "12.2");
        assert!(method.contains("without_asset_discount"));
    }

    #[test]
    fn parses_usdm_account_rates() {
        let payload = json!({"makerCommissionRate":"0.0002", "takerCommissionRate":"0.0005"});
        let (maker, taker, _) = parse_fees(Market::Usdm, &payload).unwrap();
        assert_eq!(maker, "2");
        assert_eq!(taker, "5");
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
        assert!(Args::try_parse_from([
            "binance-fee-snapshot",
            "--market",
            "spot",
            "--symbol",
            "BTCUSDT",
            "--output-root",
            "/tmp/fees",
            "--api-base",
            "https://example.com",
        ])
        .is_err());
        assert!(validate_response_symbol(&json!({"symbol":"ETHUSDT"}), "BTCUSDT").is_err());
    }
}
