pub const TOP5_DEPTH: usize = 5;

const TOP5_LINEAR_WEIGHTS: [f64; TOP5_DEPTH] = [5.0, 4.0, 3.0, 2.0, 1.0];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Top5QuantityFeatures {
    pub bid_depth: f64,
    pub ask_depth: f64,
    pub book_imbalance: f64,
    pub weighted_book_imbalance: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Top5BookFeatures {
    pub bid_depth: f64,
    pub ask_depth: f64,
    pub book_imbalance: f64,
    pub weighted_book_imbalance: f64,
    pub near_depth_concentration_skew: f64,
    pub vwap_center_deviation_bps: f64,
}

pub fn top5_book_features(
    bid_levels: impl Iterator<Item = (f64, f64)>,
    ask_levels: impl Iterator<Item = (f64, f64)>,
) -> Option<Top5BookFeatures> {
    let (bid_depth, weighted_bid_depth, bid_near_depth, bid_vwap, best_bid) =
        depth_stats(bid_levels)?;
    let (ask_depth, weighted_ask_depth, ask_near_depth, ask_vwap, best_ask) =
        depth_stats(ask_levels)?;
    let total_depth = bid_depth + ask_depth;
    let weighted_total_depth = weighted_bid_depth + weighted_ask_depth;
    let mid_price = (best_bid + best_ask) * 0.5;
    if !total_depth.is_finite()
        || !weighted_total_depth.is_finite()
        || !mid_price.is_finite()
        || total_depth <= 0.0
        || weighted_total_depth <= 0.0
        || mid_price <= 0.0
    {
        return None;
    }
    Some(Top5BookFeatures {
        bid_depth,
        ask_depth,
        book_imbalance: (bid_depth - ask_depth) / total_depth,
        weighted_book_imbalance: (weighted_bid_depth - weighted_ask_depth) / weighted_total_depth,
        near_depth_concentration_skew: bid_near_depth / bid_depth - ask_near_depth / ask_depth,
        vwap_center_deviation_bps: 10_000.0 * ((bid_vwap + ask_vwap) * 0.5 - mid_price) / mid_price,
    })
}

pub fn top5_quantity_features(
    bid_quantities: impl Iterator<Item = f64>,
    ask_quantities: impl Iterator<Item = f64>,
) -> Option<Top5QuantityFeatures> {
    let (bid_depth, weighted_bid_depth, _, _, _) = depth_stats(
        bid_quantities
            .zip(std::iter::repeat(1.0))
            .map(|(quantity, price)| (price, quantity)),
    )?;
    let (ask_depth, weighted_ask_depth, _, _, _) = depth_stats(
        ask_quantities
            .zip(std::iter::repeat(1.0))
            .map(|(quantity, price)| (price, quantity)),
    )?;
    let total_depth = bid_depth + ask_depth;
    let weighted_total_depth = weighted_bid_depth + weighted_ask_depth;
    if !total_depth.is_finite()
        || !weighted_total_depth.is_finite()
        || total_depth <= 0.0
        || weighted_total_depth <= 0.0
    {
        return None;
    }
    Some(Top5QuantityFeatures {
        bid_depth,
        ask_depth,
        book_imbalance: (bid_depth - ask_depth) / total_depth,
        weighted_book_imbalance: (weighted_bid_depth - weighted_ask_depth) / weighted_total_depth,
    })
}

fn depth_stats(mut levels: impl Iterator<Item = (f64, f64)>) -> Option<(f64, f64, f64, f64, f64)> {
    let mut depth = 0.0;
    let mut weighted = 0.0;
    let mut near_depth = 0.0;
    let mut notional = 0.0;
    let mut best_price = None;
    for (index, weight) in TOP5_LINEAR_WEIGHTS.into_iter().enumerate() {
        let (price, quantity) = levels.next()?;
        if !price.is_finite() || !quantity.is_finite() || price <= 0.0 || quantity <= 0.0 {
            return None;
        }
        if best_price.is_none() {
            best_price = Some(price);
        }
        depth += quantity;
        weighted += weight * quantity;
        notional += price * quantity;
        if index < 2 {
            near_depth += quantity;
        }
    }
    let vwap = notional / depth;
    (depth.is_finite() && weighted.is_finite() && near_depth.is_finite() && vwap.is_finite())
        .then_some((depth, weighted, near_depth, vwap, best_price?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_exact_top5_book_math_and_ignores_the_sixth_level() {
        let features = top5_book_features(
            [
                (100.0, 10.0),
                (99.0, 8.0),
                (98.0, 6.0),
                (97.0, 4.0),
                (96.0, 2.0),
                (95.0, 1_000.0),
            ]
            .into_iter(),
            [
                (101.0, 2.0),
                (102.0, 4.0),
                (103.0, 6.0),
                (104.0, 8.0),
                (105.0, 10.0),
                (106.0, 1_000.0),
            ]
            .into_iter(),
        )
        .unwrap();

        assert_eq!(features.bid_depth, 30.0);
        assert_eq!(features.ask_depth, 30.0);
        assert_eq!(features.book_imbalance, 0.0);
        assert!((features.weighted_book_imbalance - 2.0 / 9.0).abs() < f64::EPSILON);
        assert!((features.near_depth_concentration_skew - 0.4).abs() < f64::EPSILON);
        assert!((features.vwap_center_deviation_bps - 66.33499170812604).abs() < 1e-12);
        assert!(
            top5_book_features([(1.0, 1.0); 4].into_iter(), [(1.0, 1.0); 5].into_iter(),).is_none()
        );
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(top5_book_features(
                [
                    (1.0, 1.0),
                    (1.0, 1.0),
                    (1.0, invalid),
                    (1.0, 1.0),
                    (1.0, 1.0),
                ]
                .into_iter(),
                [(1.0, 1.0); 5].into_iter(),
            )
            .is_none());
        }
    }

    #[test]
    fn quantity_only_helper_stays_quantity_only() {
        let features = top5_quantity_features(
            [10.0, 8.0, 6.0, 4.0, 2.0, 1_000.0].into_iter(),
            [2.0, 4.0, 6.0, 8.0, 10.0, 1_000.0].into_iter(),
        )
        .unwrap();

        assert_eq!(features.bid_depth, 30.0);
        assert_eq!(features.ask_depth, 30.0);
        assert_eq!(features.book_imbalance, 0.0);
        assert!((features.weighted_book_imbalance - 2.0 / 9.0).abs() < f64::EPSILON);
    }
}
