use super::book::TopBook;
use serde::{Deserialize, Serialize};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSnapshot {
    pub symbol_id: u32,
    pub mid: i64,
    pub spread: i64,
    pub obi1_fp: i64,
    pub obi3_fp: i64,
    pub microgap: i64,
    pub flow_1s: i64,
    pub book_staleness_ns: i64,
    pub ts_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureView {
    pub snapshot: FeatureSnapshot,
    pub book_update_id: u64,
}

#[inline(always)]
pub fn imbalance_fp(bid_qty: i64, ask_qty: i64) -> i64 {
    let total = bid_qty + ask_qty;
    if total == 0 {
        return 0;
    }
    ((bid_qty - ask_qty) * 1_000_000) / total
}

#[inline(always)]
pub fn microprice(bid_px: i64, bid_qty: i64, ask_px: i64, ask_qty: i64) -> i64 {
    let bid_qty = bid_qty as i128;
    let ask_qty = ask_qty as i128;
    let total = bid_qty + ask_qty;
    if total == 0 {
        return (bid_px + ask_px) / 2;
    }
    ((ask_px as i128 * bid_qty + bid_px as i128 * ask_qty) / total) as i64
}

pub fn compute_features<const N: usize>(
    symbol_id: u32,
    book: &TopBook<N>,
    flow_1s: i64,
    ts_ns: i64,
) -> Option<FeatureView> {
    let best_bid = book.best_bid()?;
    let best_ask = book.best_ask()?;

    let mid = (best_bid.price + best_ask.price) / 2;
    let spread = best_ask.price - best_bid.price;
    let micro = microprice(best_bid.price, best_bid.qty, best_ask.price, best_ask.qty);
    let obi1_fp = imbalance_fp(best_bid.qty, best_ask.qty);
    let obi3_fp = top_n_imbalance::<N, 3>(book);

    Some(FeatureView {
        snapshot: FeatureSnapshot {
            symbol_id,
            mid,
            spread,
            obi1_fp,
            obi3_fp,
            microgap: micro - mid,
            flow_1s,
            book_staleness_ns: ts_ns.saturating_sub(book.last_update_ns).max(0),
            ts_ns,
        },
        book_update_id: book.last_update_id,
    })
}

pub fn top_n_imbalance<const N: usize, const K: usize>(book: &TopBook<N>) -> i64 {
    let depth = K.min(N);
    let mut bid_qty = 0_i64;
    let mut ask_qty = 0_i64;

    for idx in 0..depth {
        if idx < book.bid_len {
            bid_qty += book.bids[idx].qty;
        }
        if idx < book.ask_len {
            ask_qty += book.asks[idx].qty;
        }
    }

    imbalance_fp(bid_qty, ask_qty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_obi_and_microprice() {
        assert_eq!(imbalance_fp(150, 50), 500_000);
        assert_eq!(microprice(100, 150, 102, 50), 101);
    }

    #[test]
    fn emits_feature_snapshot_from_top_book() {
        let mut book = TopBook::<3>::new();
        book.apply_update(&[(100, 150)], &[(102, 50)], 10, 1_000);

        let view = compute_features(1, &book, 0, 2_000).unwrap();

        assert_eq!(view.book_update_id, 10);
        assert_eq!(view.snapshot.mid, 101);
        assert_eq!(view.snapshot.spread, 2);
        assert_eq!(view.snapshot.obi1_fp, 500_000);
        assert_eq!(view.snapshot.microgap, 0);
        assert_eq!(view.snapshot.book_staleness_ns, 1_000);
    }

    #[test]
    fn staleness_never_goes_negative() {
        let mut book = TopBook::<3>::new();
        book.apply_update(&[(100, 150)], &[(102, 50)], 10, 2_000);

        let view = compute_features(1, &book, 0, 1_000).unwrap();

        assert_eq!(view.snapshot.book_staleness_ns, 0);
    }
}
