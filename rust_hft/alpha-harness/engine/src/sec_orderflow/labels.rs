use crate::sec_orderflow::features::{
    as_of_book, book_crossed, last_age_ms, FlowBookSnapshot, MergedTradeSecond,
};
use hft_research_manifest::sec_orderflow::{
    SecOrderflowError, SecOrderflowExperimentManifestV1, SecOrderflowPriceKindV1,
    SecOrderflowTargetKindV1, SEC_ORDERFLOW_HORIZONS_S, SEC_ORDERFLOW_SAMPLED_MID_MIN_HORIZON_S,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstTouch {
    UpFirst,
    DownFirst,
    None,
    Ambiguous,
    Censored,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabelCountKey {
    pub symbol: String,
    pub horizon_s: u16,
    pub price: SecOrderflowPriceKindV1,
    pub target: SecOrderflowTargetKindV1,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetObservation {
    pub horizon_s: u16,
    pub price: SecOrderflowPriceKindV1,
    pub target: SecOrderflowTargetKindV1,
    pub valid: bool,
    pub invalid_reason: Option<String>,
    pub value: Option<f64>,
}

pub fn simple_return_bp(start: f64, end: f64) -> Option<f64> {
    if start > 0.0 && end > 0.0 && start.is_finite() && end.is_finite() {
        Some(10_000.0 * (end / start - 1.0))
    } else {
        None
    }
}

pub fn direction3(return_bp: f64, epsilon_bp: Option<f64>) -> Option<i8> {
    let epsilon = epsilon_bp?;
    if !return_bp.is_finite() || !epsilon.is_finite() || epsilon <= 0.0 {
        return None;
    }
    if return_bp > epsilon {
        Some(1)
    } else if return_bp < -epsilon {
        Some(-1)
    } else {
        Some(0)
    }
}

pub fn first_touch(up: bool, down: bool) -> FirstTouch {
    match (up, down) {
        (true, true) => FirstTouch::Ambiguous,
        (true, false) => FirstTouch::UpFirst,
        (false, true) => FirstTouch::DownFirst,
        (false, false) => FirstTouch::None,
    }
}

pub fn evaluate_targets(
    manifest: &SecOrderflowExperimentManifestV1,
    _symbol: &str,
    decision: &MergedTradeSecond,
    trades: &[MergedTradeSecond],
    books: &[FlowBookSnapshot],
    book: Option<&FlowBookSnapshot>,
) -> Result<Vec<TargetObservation>, SecOrderflowError> {
    let mut observations = Vec::new();
    let t_ms = decision.available_time_ms();
    for horizon in SEC_ORDERFLOW_HORIZONS_S {
        let end_sec = decision.sec.saturating_add(i64::from(horizon));
        let future: Vec<&MergedTradeSecond> = trades
            .iter()
            .filter(|row| row.sec > decision.sec && row.sec <= end_sec)
            .collect();
        let last_end = trades.iter().find(|row| row.sec == end_sec);
        let path_complete = future.len() as u16 == horizon;
        let end_available_ms = end_sec.saturating_add(1).saturating_mul(1_000);
        let end_book = as_of_book(books, end_available_ms);
        for price in prices_for_horizon(horizon) {
            for target in &manifest.targets {
                observations.push(observe(
                    manifest,
                    *target,
                    price,
                    horizon,
                    decision,
                    book,
                    end_book,
                    last_end,
                    &future,
                    path_complete,
                    t_ms,
                    end_available_ms,
                ));
            }
        }
    }
    Ok(observations)
}

fn prices_for_horizon(horizon: u16) -> Vec<SecOrderflowPriceKindV1> {
    let mut prices = vec![
        SecOrderflowPriceKindV1::MidStrict,
        SecOrderflowPriceKindV1::Last,
        SecOrderflowPriceKindV1::Mark,
    ];
    if horizon >= SEC_ORDERFLOW_SAMPLED_MID_MIN_HORIZON_S {
        prices.push(SecOrderflowPriceKindV1::MidSampled15s);
    }
    prices
}

#[allow(clippy::too_many_arguments)]
fn observe(
    manifest: &SecOrderflowExperimentManifestV1,
    target: SecOrderflowTargetKindV1,
    price: SecOrderflowPriceKindV1,
    horizon: u16,
    decision: &MergedTradeSecond,
    book: Option<&FlowBookSnapshot>,
    end_book: Option<&FlowBookSnapshot>,
    last_end: Option<&MergedTradeSecond>,
    future: &[&MergedTradeSecond],
    path_complete: bool,
    t_ms: i64,
    end_available_ms: i64,
) -> TargetObservation {
    let reject = |reason: &str| TargetObservation {
        horizon_s: horizon,
        price,
        target,
        valid: false,
        invalid_reason: Some(reason.to_string()),
        value: None,
    };
    match target {
        SecOrderflowTargetKindV1::MarkReturn
        | SecOrderflowTargetKindV1::BasisChange
        | SecOrderflowTargetKindV1::FundingRealized => {
            return reject("mark_or_funding_missing");
        }
        SecOrderflowTargetKindV1::RealizedClosePnl => {
            return reject("execution_ledger_not_admitted");
        }
        _ => {}
    }
    match price {
        SecOrderflowPriceKindV1::Mark => reject("mark_missing"),
        SecOrderflowPriceKindV1::MidStrict | SecOrderflowPriceKindV1::MidSampled15s => {
            let Some(book) = book else {
                return reject("book_missing_at_decision");
            };
            if book_crossed(book) {
                return reject("crossed_or_locked_book");
            }
            let age = t_ms.saturating_sub(book.ts);
            let max_age = if price == SecOrderflowPriceKindV1::MidStrict {
                i64::from(manifest.freshness_ms.mid_event)
            } else {
                i64::from(manifest.legacy.sampled_mid_max_age_ms)
            };
            if age > max_age {
                return reject("mid_stale");
            }
            let Some(end_book) = end_book else {
                return reject("end_mid_missing");
            };
            if book_crossed(end_book) {
                return reject("crossed_or_locked_book");
            }
            let end_age = end_available_ms.saturating_sub(end_book.ts);
            if end_age > max_age {
                return reject("end_mid_stale");
            }
            if !path_complete
                && matches!(
                    target,
                    SecOrderflowTargetKindV1::Mfe
                        | SecOrderflowTargetKindV1::Mae
                        | SecOrderflowTargetKindV1::Touch
                        | SecOrderflowTargetKindV1::FirstTouch
                )
            {
                return reject("unknown_gap");
            }
            let Some(ret) = simple_return_bp(book.mid, end_book.mid) else {
                return reject("non_finite_mid");
            };
            finish_price_target(target, price, horizon, ret, None, book.mid, reject)
        }
        SecOrderflowPriceKindV1::Last => {
            let age = last_age_ms(decision, t_ms);
            if age > i64::from(manifest.freshness_ms.last_upper_bound) {
                return reject("last_stale");
            }
            let Some(end) = last_end else {
                return reject("end_last_missing");
            };
            if !path_complete
                && matches!(
                    target,
                    SecOrderflowTargetKindV1::Mfe
                        | SecOrderflowTargetKindV1::Mae
                        | SecOrderflowTargetKindV1::Touch
                        | SecOrderflowTargetKindV1::FirstTouch
                )
            {
                return reject("unknown_gap");
            }
            let Some(ret) = simple_return_bp(decision.close, end.close) else {
                return reject("non_finite_last");
            };
            finish_price_target(
                target,
                price,
                horizon,
                ret,
                Some(future),
                decision.close,
                reject,
            )
        }
    }
}

fn finish_price_target(
    target: SecOrderflowTargetKindV1,
    price: SecOrderflowPriceKindV1,
    horizon: u16,
    ret: f64,
    last_path: Option<&[&MergedTradeSecond]>,
    start_price: f64,
    reject: impl Fn(&str) -> TargetObservation,
) -> TargetObservation {
    let valid = |value: Option<f64>| TargetObservation {
        horizon_s: horizon,
        price,
        target,
        valid: true,
        invalid_reason: None,
        value,
    };
    match target {
        SecOrderflowTargetKindV1::ReturnBp | SecOrderflowTargetKindV1::Quantiles => {
            valid(Some(ret))
        }
        SecOrderflowTargetKindV1::Direction3 => match direction3(ret, None) {
            Some(_) => valid(Some(ret)),
            None => reject("tick_size_missing"),
        },
        SecOrderflowTargetKindV1::Mfe => match last_path {
            Some(path) => valid(Some(observed_mfe(start_price, path))),
            None => reject("sparse_mid_path_is_lower_bound_only"),
        },
        SecOrderflowTargetKindV1::Mae => match last_path {
            Some(path) => valid(Some(observed_mae(start_price, path))),
            None => reject("sparse_mid_path_is_lower_bound_only"),
        },
        SecOrderflowTargetKindV1::Touch | SecOrderflowTargetKindV1::FirstTouch => {
            reject("tick_size_or_cost_barrier_missing")
        }
        _ => reject("unsupported_target"),
    }
}

fn observed_mfe(start: f64, future: &[&MergedTradeSecond]) -> f64 {
    future
        .iter()
        .filter_map(|row| simple_return_bp(start, row.high))
        .fold(0.0, f64::max)
        .max(0.0)
}

fn observed_mae(start: f64, future: &[&MergedTradeSecond]) -> f64 {
    future
        .iter()
        .filter_map(|row| simple_return_bp(start, row.low).map(|value| -value))
        .fold(0.0, f64::max)
        .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_touch_classifier_does_not_guess_order() {
        assert_eq!(first_touch(true, true), FirstTouch::Ambiguous);
        assert_eq!(first_touch(false, false), FirstTouch::None);
    }
}
