//! Fixed entry quantity, observed quotes, one entry and one exit per episode.
//! P&L is normalized by the same fixed strategy notional as native event replay.
use super::*;
use hft_research_manifest::model::{
    HorizonHoldingPolicyV1, HorizonPositionAction, HorizonPositionState,
};

type LedgerResult = (Vec<PositionReturnPoint>, usize, f64, Option<f64>);

pub(super) fn evaluate(
    rows: &[ResearchRow],
    targets: &[f64],
    range: std::ops::Range<usize>,
    costs: &EvaluationCostsV1,
    holding: &HorizonHoldingPolicyV1,
) -> Result<LedgerResult, String> {
    let duration = holding.duration_micros()?;
    let clock = |row: &ResearchRow| {
        u64::try_from(row.available_time.timestamp_micros())
            .map_err(|_| "invalid horizon ledger clock".to_string())
    };
    if rows[range.clone()]
        .windows(2)
        .any(|pair| pair[0].series_id != pair[1].series_id)
    {
        return Err(
            "holding ledger cannot anticipate or cross an unexpected series boundary".into(),
        );
    }
    let mut points = Vec::new();
    let mut trade_count = 0;
    let mut turnover = 0.0;
    let capacity = costs.capacity_enabled().then(|| {
        (
            format!("bid_depth_top{}", costs.capacity_depth_levels),
            format!("ask_depth_top{}", costs.capacity_depth_levels),
        )
    });
    let mut max_fraction = capacity.as_ref().map(|_| 0.0);
    // Quote crossing is charged explicitly in cash units below. The remaining
    // frozen fee/rebate/latency/slippage rates apply to actual fill notional.
    let mut fill_costs = costs.clone();
    fill_costs.cross_spread = false;
    for relative in contiguous_series_ranges(&rows[range.clone()]) {
        let series = range.start + relative.start..range.start + relative.end;
        let last_clock = clock(&rows[series.end - 1])?;
        let mut state = HorizonPositionState::default();
        let mut quantity = 0.0_f64;
        let mut previous_mid = None;
        for index in series {
            let row = &rows[index];
            let now = clock(row)?;
            let mid = row
                .features
                .get("mid_price")
                .copied()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("holding ledger requires observed mid prices")?;
            let spread = row
                .features
                .get("spread_bps")
                .copied()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or("holding ledger requires observed spreads")?;
            let half_spread = mid * spread / (2.0 * BPS);
            if !half_spread.is_finite() || half_spread >= mid {
                return Err("invalid holding execution quote".into());
            }
            let can_enter = now
                .checked_add(duration)
                .is_some_and(|due| due <= last_clock);
            let action = state.advance(holding, now, can_enter, Some(targets[index]))?;
            if action.target().to_bits() != targets[index].to_bits()
                || matches!(action, HorizonPositionAction::Exit { late: true, .. })
            {
                return Err("target ledger violates the non-overlapping holding horizon".into());
            }
            let gross_return = previous_mid.map_or(0.0, |previous| quantity * (mid - previous));
            let funded_notional = quantity.abs() * mid;
            let next_quantity = match action {
                HorizonPositionAction::Enter(target) => {
                    target / (mid + target.signum() * half_spread)
                }
                HorizonPositionAction::Exit { .. } => 0.0,
                _ => quantity,
            };
            let delta = next_quantity - quantity;
            let mut cost = 0.0;
            if delta != 0.0 {
                let execution_price = mid + delta.signum() * half_spread;
                let traded_notional = delta.abs() * execution_price;
                trade_count += 1;
                turnover += traded_notional;
                cost = transaction_cost(
                    row,
                    traded_notional,
                    delta,
                    &fill_costs,
                    &capacity,
                    &mut max_fraction,
                ) + delta.abs() * half_spread;
            }
            let funding = row.funding_bps.max(0.0) * funded_notional / BPS;
            let net = gross_return - cost - funding;
            if !next_quantity.is_finite() || !net.is_finite() || !turnover.is_finite() {
                return Err("non-finite horizon position ledger".into());
            }
            points.push(PositionReturnPoint {
                row_index: index,
                series_id: row.series_id,
                available_time: row.available_time,
                target_position: targets[index],
                gross_return,
                transaction_cost: cost,
                funding_cost: funding,
                net_return: net,
            });
            quantity = next_quantity;
            previous_mid = Some(mid);
        }
        if state.is_holding() || quantity != 0.0 {
            return Err("holding ledger ended before the required exit".into());
        }
    }
    Ok((points, trade_count, turnover, max_fraction))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows(prices: &[f64]) -> Vec<ResearchRow> {
        prices
            .iter()
            .enumerate()
            .map(|(i, mid)| ResearchRow {
                series_id: 1,
                available_time: chrono::DateTime::from_timestamp(i as i64, 0).unwrap(),
                label_available_time: chrono::DateTime::from_timestamp(i as i64 + 5, 0).unwrap(),
                signal: 0.0,
                features: std::collections::BTreeMap::from([
                    ("mid_price".into(), *mid),
                    ("spread_bps".into(), 2.0),
                ]),
                label: 0.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: true,
                latency_bps: 0.0,
            })
            .collect()
    }
    fn costs() -> EvaluationCostsV1 {
        EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            slippage_bps: 0.0,
            cross_spread: true,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        }
    }
    #[test]
    fn held_quantity_matches_one_quote_round_trip_without_rebalancing_costs() {
        for (target, prices) in [
            (0.5, [100.0, 80.0, 120.0, 95.0, 90.0, 110.0]),
            (-0.4, [100.0, 130.0, 110.0, 95.0, 80.0, 90.0]),
        ] {
            let rows = rows(&prices);
            let mut targets = vec![target; 6];
            targets[5] = 0.0;
            let policy = HorizonHoldingPolicyV1 {
                horizon_millis: 5000,
            };
            let (points, trades, turnover, _) =
                evaluate(&rows, &targets, 0..6, &costs(), &policy).unwrap();
            let entry_price = prices[0] * (1.0 + target.signum() * 0.0001);
            let exit_price = prices[5] * (1.0 - target.signum() * 0.0001);
            let quantity = target / entry_price;
            let expected_turnover = quantity.abs() * (entry_price + exit_price);
            let expected = quantity * (exit_price - entry_price) - expected_turnover * 0.0002;
            assert_eq!(trades, 2);
            assert!((turnover - expected_turnover).abs() < 1e-12);
            assert!(
                (points.iter().map(|point| point.net_return).sum::<f64>() - expected).abs() < 1e-12
            );
            assert!(points[1..5]
                .iter()
                .all(|point| point.transaction_cost == 0.0));
        }
    }
    #[test]
    fn early_exit_resize_and_incomplete_episodes_are_rejected() {
        let rows = rows(&[100.0; 6]);
        let policy = HorizonHoldingPolicyV1 {
            horizon_millis: 5000,
        };
        for targets in [
            vec![0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.5, 0.8, 0.8, 0.8, 0.8, 0.0],
            vec![0.0, 0.5, 0.5, 0.5, 0.5, 0.0],
        ] {
            assert!(evaluate(&rows, &targets, 0..6, &costs(), &policy).is_err());
        }
    }
}
