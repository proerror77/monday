//! Descriptive consumption of verified external historical fill events. These
//! rows have no historical information-availability proof and never enter a
//! Mission, holdout, executable replay, or promotion through this module.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use data::polymarket_history::{verify_history, HistoryQuality, HistoryRecord, HISTORY_SCOPE};
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct HistoricalOutcomeSummary {
    pub condition_id: String,
    pub token_id: String,
    pub outcome: String,
    pub market_slug: String,
    pub observed_fill_rows: u64,
    pub identity_ambiguous_rows: u64,
    pub first_block_time_unix: u64,
    pub last_block_time_unix: u64,
    pub minimum_observed_price: Decimal,
    pub maximum_observed_price: Decimal,
    /// Sums describe exported fill-event rows; they do not certify market volume.
    pub observed_cash_amount: Decimal,
    pub observed_token_amount: Decimal,
    pub observed_volume_weighted_price: Decimal,
}

#[derive(Debug, Serialize)]
pub struct HistoricalTradeExploration {
    pub schema: &'static str,
    pub evidence_scope: &'static str,
    pub manifest_sha256: String,
    pub content_sha256: String,
    pub source_revision: String,
    pub requested_start_unix: u64,
    pub requested_end_unix: u64,
    pub retrieved_at_unix: u64,
    pub quality: HistoryQuality,
    pub outcomes: Vec<HistoricalOutcomeSummary>,
    pub baseline_status: &'static str,
    pub baseline_blockers: Vec<&'static str>,
    pub execution_metrics: &'static str,
    pub live_allowed: bool,
    pub promotion_allowed: bool,
}

fn exact_amount_sum(total: Decimal, amount: Decimal) -> Result<Decimal> {
    let raw_units = |value: Decimal| -> Result<i128> {
        let scale = 6_u32
            .checked_sub(value.scale())
            .context("invalid history amount scale")?;
        value
            .mantissa()
            .checked_mul(10_i128.pow(scale))
            .context("history raw amount overflow")
    };
    let raw = raw_units(total)?
        .checked_add(raw_units(amount)?)
        .context("history aggregate overflow")?;
    if !(0..=(1_i128 << 96) - 1).contains(&raw) {
        anyhow::bail!("history aggregate exceeds exact six-decimal representation");
    }
    Ok(Decimal::from_i128_with_scale(raw, 6))
}

pub fn explore_historical_trades(
    directory: &Path,
    manifest_sha256: &str,
) -> Result<HistoricalTradeExploration> {
    let history = verify_history(directory, manifest_sha256)?;
    let manifest = history.manifest();
    let mut slugs = BTreeMap::new();
    let mut outcomes: BTreeMap<(String, String), HistoricalOutcomeSummary> = BTreeMap::new();
    for record in history.records() {
        match record {
            HistoryRecord::Market(market) => {
                slugs.insert(market.condition_id.clone(), market.slug.clone());
            }
            HistoryRecord::Fill(fill) => {
                let row = outcomes
                    .entry((fill.condition_id.clone(), fill.token_id.clone()))
                    .or_insert_with(|| HistoricalOutcomeSummary {
                        condition_id: fill.condition_id.clone(),
                        token_id: fill.token_id.clone(),
                        outcome: fill.outcome.clone(),
                        market_slug: slugs.get(&fill.condition_id).cloned().unwrap_or_default(),
                        observed_fill_rows: 0,
                        identity_ambiguous_rows: 0,
                        first_block_time_unix: fill.block_time_unix,
                        last_block_time_unix: fill.block_time_unix,
                        minimum_observed_price: fill.price,
                        maximum_observed_price: fill.price,
                        observed_cash_amount: Decimal::ZERO,
                        observed_token_amount: Decimal::ZERO,
                        observed_volume_weighted_price: Decimal::ZERO,
                    });
                row.observed_fill_rows += 1;
                row.identity_ambiguous_rows += u64::from(fill.log_index.is_none());
                row.first_block_time_unix = row.first_block_time_unix.min(fill.block_time_unix);
                row.last_block_time_unix = row.last_block_time_unix.max(fill.block_time_unix);
                row.minimum_observed_price = row.minimum_observed_price.min(fill.price);
                row.maximum_observed_price = row.maximum_observed_price.max(fill.price);
                row.observed_cash_amount =
                    exact_amount_sum(row.observed_cash_amount, fill.cash_amount)?;
                row.observed_token_amount =
                    exact_amount_sum(row.observed_token_amount, fill.token_amount)?;
                row.observed_volume_weighted_price = row
                    .observed_cash_amount
                    .checked_div(row.observed_token_amount)
                    .context("history weighted price overflow")?;
            }
            HistoryRecord::Quarantine(_) => {}
        }
    }
    let mut blockers = vec![
        "external_export_has_no_verified_contract_or_date_completeness",
        "historical_fill_and_market_metadata_availability_unknown",
        "point_in_time_cex_signal_evidence_not_supplied",
        "official_event_window_and_settlement_evidence_not_supplied",
        "authenticated_chronological_partitions_and_frozen_hypothesis_required",
        "executable_quotes_and_cost_model_required_for_execution_metrics",
    ];
    if manifest.quality.accepted_fill_rows == 0 {
        blockers.push("no_accepted_fill_rows");
    }
    if manifest.quality.ambiguous_identity_rows > 0 {
        blockers.push("fill_identity_ambiguous_without_log_index");
    }
    if manifest.quality.quarantined_rows > 0 {
        blockers.push("quarantined_input_rows_require_disposition");
    }
    Ok(HistoricalTradeExploration {
        schema: "monday.prediction.historical_trade_exploration.v1",
        evidence_scope: HISTORY_SCOPE,
        manifest_sha256: history.manifest_sha256().to_owned(),
        content_sha256: manifest.content_sha256.clone(),
        source_revision: manifest.source.revision.clone(),
        requested_start_unix: manifest.source.requested_start_unix,
        requested_end_unix: manifest.source.requested_end_unix,
        retrieved_at_unix: manifest.source.retrieved_at_unix,
        quality: manifest.quality.clone(),
        outcomes: outcomes.into_values().collect(),
        baseline_status: "blocked_missing_research_evidence",
        baseline_blockers: blockers,
        execution_metrics: "unavailable: no executable-price, spread, queue, fill-probability, net-return or Sharpe evidence",
        live_allowed: false,
        promotion_allowed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::polymarket_history::*;
    use std::fs;

    #[test]
    fn historical_trade_consumer_rejects_aggregate_precision_loss() {
        let maximum = Decimal::from_parts(u32::MAX, u32::MAX, u32::MAX, false, 6);
        assert!(exact_amount_sum(maximum, Decimal::new(1, 6)).is_err());
        assert_eq!(
            exact_amount_sum(Decimal::new(1, 6), Decimal::new(2, 6)).unwrap(),
            Decimal::new(3, 6)
        );
    }

    #[test]
    fn historical_trade_consumer_explores_rows_without_promoting_to_research_or_execution() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let condition_id = format!("0x{}", "a".repeat(64));
        let market = HistoryMarket {
            condition_id: condition_id.clone(),
            slug: "btc-updown-5m-synthetic".into(),
            outcomes: BTreeMap::from([("123".into(), "Up".into()), ("456".into(), "Down".into())]),
        };
        let make_fill = |source_row, cash, time| {
            HistoryRecord::Fill(HistoryFill {
                source_row,
                condition_id: condition_id.clone(),
                token_id: "123".into(),
                outcome: "Up".into(),
                block_time_unix: time,
                historical_available_at_unix: None,
                transaction_hash: format!("0x{}", "b".repeat(64)),
                log_index: None,
                maker_direction: MakerDirection::Buy,
                cash_amount: Decimal::new(cash, 0),
                token_amount: Decimal::TEN,
                price: Decimal::new(cash, 1),
            })
        };
        let records = [
            HistoryRecord::Market(market),
            make_fill(1, 4, 1_789_000_100),
            make_fill(2, 6, 1_789_000_010),
        ];
        let mut content = Vec::new();
        let mut quality = HistoryQuality::default();
        for record in records {
            quality.observe(&record);
            content.extend(serde_json::to_vec(&record).unwrap());
            content.push(b'\n');
        }
        let source = HistorySourceFile {
            path: root.join("external.csv").display().to_string(),
            sha256: "c".repeat(64),
            bytes: 100,
        };
        let manifest = HistoryManifest {
            schema: HISTORY_SCHEMA.into(),
            evidence_scope: HISTORY_SCOPE.into(),
            source: HistorySource {
                schema: POLY_DATA_SCHEMA.into(),
                revision: POLY_DATA_REVISION.into(),
                chain_id: 137,
                exchange_contract: V2_EXCHANGE.into(),
                markets: source.clone(),
                fills: source,
                retrieved_at_unix: 1_789_001_000,
                imported_at_unix: 1_789_002_000,
                requested_start_unix: 1_789_000_000,
                requested_end_unix: 1_789_000_300,
                max_input_bytes: 1024,
                max_input_rows: 100,
            },
            content_sha256: history_sha256(&content),
            content_bytes: content.len() as u64,
            quality,
            l2_available: false,
            executable_quotes_available: false,
            settlement_available: false,
            trade_completion_certified: false,
            coverage_certified: false,
            research_promotion_allowed: false,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(root.join(HISTORY_DATA_FILE), &content).unwrap();
        fs::write(root.join(HISTORY_MANIFEST_FILE), &manifest_bytes).unwrap();
        fs::write(
            root.join(HISTORY_SUCCESS_FILE),
            format!("{}\n", manifest.content_sha256),
        )
        .unwrap();
        let anchor = history_sha256(&manifest_bytes);
        let result = explore_historical_trades(&root, &anchor).unwrap();
        assert_eq!(result.outcomes.len(), 1);
        let outcome = &result.outcomes[0];
        assert_eq!(outcome.observed_fill_rows, 2);
        assert_eq!(outcome.identity_ambiguous_rows, 2);
        assert_eq!(outcome.observed_volume_weighted_price, Decimal::new(5, 1));
        assert_eq!(outcome.first_block_time_unix, 1_789_000_010);
        assert_eq!(outcome.last_block_time_unix, 1_789_000_100);
        assert!(!result.live_allowed && !result.promotion_allowed);
        assert_eq!(result.baseline_status, "blocked_missing_research_evidence");
        assert!(result
            .baseline_blockers
            .contains(&"fill_identity_ambiguous_without_log_index"));
        assert!(explore_historical_trades(&root, &"0".repeat(64)).is_err());
    }
}
