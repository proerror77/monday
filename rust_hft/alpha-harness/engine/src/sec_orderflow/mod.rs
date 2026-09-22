//! Deterministic second-level orderflow transforms. No order or risk stack.

pub mod costs;
pub mod features;
pub mod labels;
pub mod splits;

use alpha_domain::sec_orderflow::{
    SecOrderflowAuditIdentitiesV1, SecOrderflowAuditReportV1, SecOrderflowEligibilityV1,
    SecOrderflowFileDigestV1, SecOrderflowInputStatusV1, SecOrderflowTargetCountV1,
    SEC_ORDERFLOW_AUDIT_REPORT_SCHEMA_V1,
};
use features::{
    as_of_book, merge_trade_fragments, FlowBookSnapshot, FlowTradeFragment, MergedTradeSecond,
};
use hft_research_manifest::sec_orderflow::{
    SecOrderflowError, SecOrderflowExperimentManifestV1, SecOrderflowPriceKindV1,
};
use labels::{evaluate_targets, LabelCountKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use splits::plumbing_split;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedSymbolTape {
    pub symbol: String,
    pub trades: Vec<MergedTradeSecond>,
    pub books: Vec<FlowBookSnapshot>,
    pub source_files: Vec<SecOrderflowFileDigestV1>,
    pub parse_errors: u64,
    pub isolated_cross_file_seconds: u64,
}

pub fn merge_symbol_trades(
    symbol: &str,
    fragments: Vec<FlowTradeFragment>,
) -> Result<(Vec<MergedTradeSecond>, u64), SecOrderflowError> {
    let mut by_key: BTreeMap<(String, i64), Vec<FlowTradeFragment>> = BTreeMap::new();
    for fragment in fragments {
        if fragment.symbol.as_deref().unwrap_or(symbol) != symbol {
            return Err(SecOrderflowError::Invalid(
                "symbol mismatch in trade fragment",
            ));
        }
        by_key
            .entry((fragment.source.path.clone(), fragment.sec))
            .or_default()
            .push(fragment);
    }
    let mut isolated = 0;
    let mut seconds: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for (path, sec) in by_key.keys() {
        seconds.entry(*sec).or_default().push(path.clone());
    }
    for files in seconds.values() {
        let unique = files.iter().collect::<BTreeSet<_>>().len();
        if unique > 1 {
            isolated += 1;
        }
    }
    let mut merged = Vec::new();
    for ((_, _), mut group) in by_key {
        group.sort_by_key(|fragment| fragment.source.line);
        merged.push(merge_trade_fragments(&group)?);
    }
    merged.sort_by_key(|row| row.sec);
    Ok((merged, isolated))
}

pub fn audit_imported(
    manifest: &SecOrderflowExperimentManifestV1,
    tapes: &[ImportedSymbolTape],
    input_status: SecOrderflowInputStatusV1,
) -> Result<SecOrderflowAuditReportV1, SecOrderflowError> {
    manifest.validate()?;
    let config_sha256 = manifest.sha256()?;
    let mut source_files = Vec::new();
    for tape in tapes {
        source_files.extend(tape.source_files.iter().cloned());
    }
    source_files.sort_by(|left, right| left.path.cmp(&right.path));
    let input_list_sha256 = if source_files.is_empty() {
        None
    } else {
        Some(input_list_digest(&source_files)?)
    };

    let mut g0_reasons = Vec::new();
    if tapes.iter().any(|tape| tape.parse_errors > 0) {
        g0_reasons.push("json_parse_errors");
    }
    if tapes
        .iter()
        .any(|tape| tape.isolated_cross_file_seconds > 0)
    {
        g0_reasons.push("cross_file_same_second_isolated");
    }
    g0_reasons.push("legacy_availability_quality_unknown");
    let g0_reason = g0_reasons.join(",");

    let mut counts: BTreeMap<LabelCountKey, SecOrderflowTargetCountV1> = BTreeMap::new();
    for tape in tapes {
        if !manifest.symbols.contains(&tape.symbol) {
            return Err(SecOrderflowError::Invalid(
                "imported symbol is not in the manifest",
            ));
        }
        let books = sorted_books(&tape.books);
        for trade in &tape.trades {
            let decision_ms = trade.available_time_ms();
            let book = as_of_book(&books, decision_ms);
            if book.is_some_and(|snapshot| snapshot.ts >= decision_ms) {
                return Err(SecOrderflowError::Invalid("forward book join is forbidden"));
            }
            let observations =
                evaluate_targets(manifest, &tape.symbol, trade, &tape.trades, &books, book)?;
            for observation in observations {
                let key = LabelCountKey {
                    symbol: tape.symbol.clone(),
                    horizon_s: observation.horizon_s,
                    price: observation.price,
                    target: observation.target,
                };
                let entry =
                    counts
                        .entry(key.clone())
                        .or_insert_with(|| SecOrderflowTargetCountV1 {
                            symbol: key.symbol.clone(),
                            horizon_s: key.horizon_s,
                            price: price_name(key.price),
                            target: key.target,
                            valid: 0,
                            invalid: 0,
                            rejection_reasons: BTreeMap::new(),
                        });
                if observation.valid {
                    entry.valid += 1;
                } else {
                    entry.invalid += 1;
                    let reason = observation
                        .invalid_reason
                        .unwrap_or_else(|| "invalid".to_string());
                    *entry.rejection_reasons.entry(reason).or_insert(0) += 1;
                }
            }
        }
    }

    let mut target_counts = counts.into_values().collect::<Vec<_>>();
    target_counts.sort_by(|left, right| {
        (
            left.symbol.as_str(),
            left.horizon_s,
            left.price.as_str(),
            left.target,
        )
            .cmp(&(
                right.symbol.as_str(),
                right.horizon_s,
                right.price.as_str(),
                right.target,
            ))
    });

    let decision_times = tapes
        .iter()
        .flat_map(|tape| tape.trades.iter().map(MergedTradeSecond::available_time_ms))
        .collect::<Vec<_>>();
    let split = plumbing_split(
        &decision_times,
        i64::from(hft_research_manifest::sec_orderflow::SEC_ORDERFLOW_FULL_DEPENDENCY_GAP_FLOOR_S),
    )?;

    let eligibility = SecOrderflowEligibilityV1::from_audit(
        manifest,
        input_status,
        &g0_reason,
        split.calendar_days,
    )?;

    let mut notes = vec![
        "diagnostic_only".to_string(),
        "no_jobs_dispatched".to_string(),
        "no_training".to_string(),
        "legacy_flow_not_governed_dataset".to_string(),
        "s1_s5_have_no_live_authority".to_string(),
    ];
    if target_counts.is_empty() {
        notes.push("target_counts_empty".to_string());
    }

    Ok(SecOrderflowAuditReportV1 {
        schema_version: SEC_ORDERFLOW_AUDIT_REPORT_SCHEMA_V1.to_string(),
        diagnostic: true,
        mode: "audit_only".to_string(),
        input_status,
        identities: SecOrderflowAuditIdentitiesV1 {
            config_sha256,
            input_list_sha256,
            source_files,
        },
        eligibility,
        target_counts,
        applied_split: split.label,
        notes,
    })
}

fn price_name(price: SecOrderflowPriceKindV1) -> String {
    match price {
        SecOrderflowPriceKindV1::MidStrict => "mid_strict".to_string(),
        SecOrderflowPriceKindV1::Last => "last".to_string(),
        SecOrderflowPriceKindV1::Mark => "mark".to_string(),
        SecOrderflowPriceKindV1::MidSampled15s => "mid_sampled_15s".to_string(),
    }
}

fn sorted_books(books: &[FlowBookSnapshot]) -> Vec<FlowBookSnapshot> {
    let mut books = books.to_vec();
    books.sort_by_key(|book| book.ts);
    books
}

fn input_list_digest(files: &[SecOrderflowFileDigestV1]) -> Result<String, SecOrderflowError> {
    let bytes = serde_json::to_vec(files)
        .map_err(|_| SecOrderflowError::Invalid("input list must serialize"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sec_orderflow::costs::{net_bp, round_trip_cost_bp, CashflowFill};
    use crate::sec_orderflow::features::TradeSourceRef;
    use crate::sec_orderflow::labels::{direction3, first_touch, simple_return_bp, FirstTouch};
    use alpha_domain::sec_orderflow::SecOrderflowAuditReportV1;

    #[allow(clippy::too_many_arguments)]
    fn fragment(
        sec: i64,
        line: u64,
        o: f64,
        h: f64,
        l: f64,
        c: f64,
        buy_to: f64,
        sell_to: f64,
    ) -> FlowTradeFragment {
        FlowTradeFragment {
            ts: sec * 1_000,
            sec,
            symbol: Some("MARSCOINUSDT".to_string()),
            o,
            h,
            l,
            c,
            buy_vol: buy_to / o,
            sell_vol: sell_to / o,
            buy_to,
            sell_to,
            buy_n: 1,
            sell_n: 1,
            trades_n: 2,
            trades_to: buy_to + sell_to,
            vwap: c,
            extra: BTreeMap::new(),
            source: TradeSourceRef {
                path: "trades.jsonl".to_string(),
                line,
            },
        }
    }

    #[test]
    fn same_second_fragments_merge_without_dropping_volume() {
        let merged = merge_trade_fragments(&[
            fragment(10, 1, 100.0, 101.0, 99.0, 100.5, 10.0, 5.0),
            fragment(10, 2, 100.5, 102.0, 98.0, 101.0, 7.0, 1.0),
        ])
        .unwrap();
        assert_eq!(merged.sec, 10);
        assert_eq!(merged.open, 100.0);
        assert_eq!(merged.high, 102.0);
        assert_eq!(merged.low, 98.0);
        assert_eq!(merged.close, 101.0);
        assert_eq!(merged.buy_to, 17.0);
        assert_eq!(merged.sell_to, 6.0);
        assert_eq!(merged.signed_notional, 11.0);
        assert!(merged.historical_cvd_discarded);
    }

    #[test]
    fn first_book_after_decision_is_missing() {
        let books = [FlowBookSnapshot {
            ts: 11_500,
            symbol: "MARSCOINUSDT".to_string(),
            mid: 100.0,
            bid1: 99.9,
            ask1: 100.1,
            spread_bp: 20.0,
            bid_notional5: 50.0,
            ask_notional5: 50.0,
            source_imb5: 1.0,
            source_imb20: 1.0,
            bids: vec![[99.9, 1.0]],
            asks: vec![[100.1, 1.0]],
            extra: BTreeMap::new(),
            source: TradeSourceRef {
                path: "book.jsonl".to_string(),
                line: 1,
            },
        }];
        assert!(as_of_book(&books, 11_000).is_none());
        assert!(as_of_book(&books, 12_000).is_some());
    }

    #[test]
    fn same_second_dual_touch_is_ambiguous_and_adverse_first() {
        assert_eq!(first_touch(true, true), FirstTouch::Ambiguous);
        let long = net_bp(&CashflowFill {
            sign: 1,
            qty: 1.0,
            entry: 100.0,
            exit: 99.0,
            taker_bps_per_side: 11.0,
        });
        let short = net_bp(&CashflowFill {
            sign: -1,
            qty: 1.0,
            entry: 100.0,
            exit: 101.0,
            taker_bps_per_side: 11.0,
        });
        assert!(long < 0.0);
        assert!(short < 0.0);
    }

    #[test]
    fn return_and_direction_use_null_not_zero_for_missing_tick() {
        assert!((simple_return_bp(100.0, 101.0).unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(direction3(100.0, None), None);
        assert_eq!(direction3(0.4, Some(1.0)), Some(0));
        assert_eq!(direction3(2.0, Some(1.0)), Some(1));
    }

    #[test]
    fn missing_input_report_stays_ineligible() {
        let manifest = SecOrderflowExperimentManifestV1::canonical_audit();
        let report = SecOrderflowAuditReportV1::unavailable(
            &manifest,
            SecOrderflowInputStatusV1::Unavailable,
        )
        .unwrap();
        assert!(!report.eligibility.research_eligible);
        assert_eq!(report.eligibility.jobs_dispatched, 0);
        assert!(report
            .notes
            .iter()
            .any(|note| note == "mac_collector_paths_are_never_defaulted"));
    }

    #[test]
    fn overlapping_label_sum_is_not_equity() {
        let labels = [10.0_f64, 10.0, 10.0];
        let path_pnl = 10.0;
        assert_ne!(labels.iter().sum::<f64>(), path_pnl);
        assert!((round_trip_cost_bp(11.0, 5.03) - 27.03).abs() < 1e-9);
    }

    #[test]
    fn plumbing_split_does_not_open_holdout() {
        let times = (0..10).map(|index| index * 2_200_000).collect::<Vec<_>>();
        let split = plumbing_split(&times, 2_105).unwrap();
        assert!(!split.holdout_open);
        assert_eq!(split.label, "plumbing_chronological_not_oos");
        assert!(split.purge_gap_s >= 2_105);
    }
}
