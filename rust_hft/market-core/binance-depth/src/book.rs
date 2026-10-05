use std::collections::BTreeMap;

/// Side marker for local Binance book updates.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSide {
    Bid = 1,
    Ask = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Level {
    pub price: i64,
    pub qty: i64,
}

/// Fixed-size top-of-book view for feature computation.
///
/// This is not the full correctness book. It is the compact, copy-friendly
/// surface used by OBI, microprice, spread, and signal rules.
#[derive(Debug, Clone)]
pub struct TopBook<const N: usize> {
    pub bids: [Level; N],
    pub asks: [Level; N],
    pub bid_len: usize,
    pub ask_len: usize,
    pub last_update_id: u64,
    pub last_update_ns: i64,
}

impl<const N: usize> Default for TopBook<N> {
    fn default() -> Self {
        Self {
            bids: [Level::default(); N],
            asks: [Level::default(); N],
            bid_len: 0,
            ask_len: 0,
            last_update_id: 0,
            last_update_ns: 0,
        }
    }
}

impl<const N: usize> TopBook<N> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn best_bid(&self) -> Option<Level> {
        (self.bid_len > 0).then_some(self.bids[0])
    }

    #[inline]
    pub fn best_ask(&self) -> Option<Level> {
        (self.ask_len > 0).then_some(self.asks[0])
    }

    pub fn clear(&mut self) {
        self.bids = [Level::default(); N];
        self.asks = [Level::default(); N];
        self.bid_len = 0;
        self.ask_len = 0;
        self.last_update_id = 0;
        self.last_update_ns = 0;
    }

    pub fn apply_bid(&mut self, price: i64, qty: i64) {
        apply_side::<N, true>(&mut self.bids, &mut self.bid_len, price, qty);
    }

    pub fn apply_ask(&mut self, price: i64, qty: i64) {
        apply_side::<N, false>(&mut self.asks, &mut self.ask_len, price, qty);
    }

    pub fn apply_update(
        &mut self,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
        final_update_id: u64,
        update_ns: i64,
    ) {
        for &(price, qty) in bids {
            self.apply_bid(price, qty);
        }
        for &(price, qty) in asks {
            self.apply_ask(price, qty);
        }
        self.last_update_id = final_update_id;
        self.last_update_ns = update_ns;
    }
}

/// Fixed-point correctness book plus a refreshed TopN feature view.
///
/// This is intentionally a correctness surface first. It uses fixed-point
/// integer prices/quantities and keeps the full known depth so deletes/updates
/// outside the current TopN are not lost. Feature code reads only `top`.
#[derive(Debug, Clone)]
pub struct CorrectnessBook<const N: usize> {
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
    top: TopBook<N>,
}

impl<const N: usize> Default for CorrectnessBook<N> {
    fn default() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            top: TopBook::new(),
        }
    }
}

impl<const N: usize> CorrectnessBook<N> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn top(&self) -> &TopBook<N> {
        &self.top
    }

    pub fn apply_snapshot(
        &mut self,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
        final_update_id: u64,
        update_ns: i64,
    ) {
        self.bids.clear();
        self.asks.clear();
        apply_levels(&mut self.bids, bids);
        apply_levels(&mut self.asks, asks);
        self.refresh_top(final_update_id, update_ns);
    }

    pub fn apply_diff(
        &mut self,
        bids: &[(i64, i64)],
        asks: &[(i64, i64)],
        final_update_id: u64,
        update_ns: i64,
    ) {
        apply_levels(&mut self.bids, bids);
        apply_levels(&mut self.asks, asks);
        self.refresh_top(final_update_id, update_ns);
    }

    fn refresh_top(&mut self, final_update_id: u64, update_ns: i64) {
        self.top.clear();

        for (idx, (&price, &qty)) in self.bids.iter().rev().take(N).enumerate() {
            self.top.bids[idx] = Level { price, qty };
            self.top.bid_len += 1;
        }
        for (idx, (&price, &qty)) in self.asks.iter().take(N).enumerate() {
            self.top.asks[idx] = Level { price, qty };
            self.top.ask_len += 1;
        }

        self.top.last_update_id = final_update_id;
        self.top.last_update_ns = update_ns;
    }
}

fn apply_levels(levels: &mut BTreeMap<i64, i64>, updates: &[(i64, i64)]) {
    for &(price, qty) in updates {
        if qty <= 0 {
            levels.remove(&price);
        } else {
            levels.insert(price, qty);
        }
    }
}

fn apply_side<const N: usize, const DESC: bool>(
    levels: &mut [Level; N],
    len: &mut usize,
    price: i64,
    qty: i64,
) {
    if N == 0 {
        return;
    }

    for idx in 0..*len {
        if levels[idx].price == price {
            if qty <= 0 {
                remove_at(levels, len, idx);
            } else {
                levels[idx].qty = qty;
            }
            return;
        }
    }

    if qty <= 0 {
        return;
    }

    let mut insert_at = *len;
    for (idx, level) in levels.iter().take(*len).enumerate() {
        let before = if DESC {
            price > level.price
        } else {
            price < level.price
        };
        if before {
            insert_at = idx;
            break;
        }
    }

    if *len == N && insert_at == N {
        return;
    }

    if *len < N {
        *len += 1;
    }

    for idx in (insert_at + 1..*len).rev() {
        levels[idx] = levels[idx - 1];
    }
    levels[insert_at] = Level { price, qty };
}

fn remove_at<const N: usize>(levels: &mut [Level; N], len: &mut usize, idx: usize) {
    if idx >= *len {
        return;
    }

    for pos in idx..(*len - 1) {
        levels[pos] = levels[pos + 1];
    }

    *len -= 1;
    if *len < N {
        levels[*len] = Level::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_bid_and_ask_order() {
        let mut book = TopBook::<3>::new();

        book.apply_bid(100, 10);
        book.apply_bid(102, 12);
        book.apply_bid(101, 11);
        book.apply_bid(99, 9);

        assert_eq!(book.bid_len, 3);
        assert_eq!(book.bids.map(|level| level.price), [102, 101, 100]);

        book.apply_ask(105, 5);
        book.apply_ask(103, 3);
        book.apply_ask(104, 4);
        book.apply_ask(106, 6);

        assert_eq!(book.ask_len, 3);
        assert_eq!(book.asks.map(|level| level.price), [103, 104, 105]);
    }

    #[test]
    fn deletes_and_updates_existing_levels() {
        let mut book = TopBook::<3>::new();

        book.apply_bid(100, 10);
        book.apply_bid(101, 11);
        book.apply_bid(100, 15);
        book.apply_bid(101, 0);

        assert_eq!(book.bid_len, 1);
        assert_eq!(
            book.best_bid(),
            Some(Level {
                price: 100,
                qty: 15
            })
        );
    }

    #[test]
    fn correctness_book_keeps_outside_topn_levels() {
        let mut book = CorrectnessBook::<2>::new();

        book.apply_snapshot(&[(100, 10), (99, 9), (98, 8)], &[(101, 1)], 10, 1);

        assert_eq!(book.top().bid_len, 2);
        assert_eq!(book.top().bids.map(|level| level.price), [100, 99]);

        book.apply_diff(&[(100, 0)], &[], 11, 2);

        assert_eq!(book.top().bid_len, 2);
        assert_eq!(book.top().bids.map(|level| level.price), [99, 98]);
        assert_eq!(book.top().last_update_id, 11);
    }
}
