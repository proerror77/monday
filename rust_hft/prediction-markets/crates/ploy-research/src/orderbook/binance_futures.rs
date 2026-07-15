use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct LocalBook {
    pub symbol: String,
    pub last_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone)]
pub struct BookSnapshot {
    pub symbol: String,
    pub last_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone)]
pub struct DepthDiff {
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalBookError {
    InvalidLevel,
    FirstDiffOutOfSequence {
        snapshot_last_update_id: u64,
        first_update_id: u64,
        final_update_id: u64,
    },
    DiffGap {
        expected_first_update_id: u64,
        first_update_id: u64,
    },
}

impl LocalBook {
    pub fn from_snapshot(snapshot: BookSnapshot) -> Result<Self, LocalBookError> {
        let mut book = Self {
            symbol: snapshot.symbol,
            last_update_id: snapshot.last_update_id,
            bids: snapshot.bids,
            asks: snapshot.asks,
        };
        sort_and_validate(&mut book.bids, Side::Bid)?;
        sort_and_validate(&mut book.asks, Side::Ask)?;
        Ok(book)
    }

    pub fn apply_diff(&mut self, diff: DepthDiff) -> Result<(), LocalBookError> {
        if diff.first_update_id > diff.final_update_id {
            return Err(LocalBookError::DiffGap {
                expected_first_update_id: self.last_update_id + 1,
                first_update_id: diff.first_update_id,
            });
        }

        let expected = self.last_update_id + 1;
        if diff.first_update_id <= self.last_update_id {
            if !(diff.first_update_id <= expected && expected <= diff.final_update_id) {
                return Err(LocalBookError::FirstDiffOutOfSequence {
                    snapshot_last_update_id: self.last_update_id,
                    first_update_id: diff.first_update_id,
                    final_update_id: diff.final_update_id,
                });
            }
        } else if diff.first_update_id != expected {
            return Err(LocalBookError::DiffGap {
                expected_first_update_id: expected,
                first_update_id: diff.first_update_id,
            });
        }

        apply_levels(&mut self.bids, diff.bids, Side::Bid)?;
        apply_levels(&mut self.asks, diff.asks, Side::Ask)?;
        self.last_update_id = diff.final_update_id;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum Side {
    Bid,
    Ask,
}

fn apply_levels(
    current: &mut Vec<BookLevel>,
    updates: Vec<BookLevel>,
    side: Side,
) -> Result<(), LocalBookError> {
    for update in updates {
        validate_level(&update)?;
        if let Some(existing) = current.iter_mut().find(|level| level.price == update.price) {
            if update.size == 0.0 {
                current.retain(|level| level.price != update.price);
            } else {
                existing.size = update.size;
            }
        } else if update.size > 0.0 {
            current.push(update);
        }
    }
    sort_and_validate(current, side)
}

fn sort_and_validate(levels: &mut [BookLevel], side: Side) -> Result<(), LocalBookError> {
    for level in levels.iter() {
        validate_level(level)?;
    }
    match side {
        Side::Bid => levels.sort_by(|a, b| descending_price(a.price, b.price)),
        Side::Ask => levels.sort_by(|a, b| ascending_price(a.price, b.price)),
    }
    Ok(())
}

fn validate_level(level: &BookLevel) -> Result<(), LocalBookError> {
    if !level.price.is_finite() || level.price <= 0.0 || !level.size.is_finite() || level.size < 0.0
    {
        return Err(LocalBookError::InvalidLevel);
    }
    Ok(())
}

fn ascending_price(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn descending_price(left: f64, right: f64) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: f64, size: f64) -> BookLevel {
        BookLevel { price, size }
    }

    fn snapshot() -> BookSnapshot {
        BookSnapshot {
            symbol: "BTCUSDT".to_string(),
            last_update_id: 100,
            bids: vec![level(99.0, 2.0), level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0), level(102.0, 2.0)],
        }
    }

    #[test]
    fn first_diff_bridges_snapshot_last_update_id() {
        let mut book = LocalBook::from_snapshot(snapshot()).unwrap();
        book.apply_diff(DepthDiff {
            first_update_id: 98,
            final_update_id: 101,
            bids: vec![level(100.0, 3.0), level(98.0, 1.0)],
            asks: vec![level(101.0, 0.0), level(100.5, 4.0)],
        })
        .unwrap();

        assert_eq!(book.last_update_id, 101);
        assert_eq!(
            book.bids,
            vec![level(100.0, 3.0), level(99.0, 2.0), level(98.0, 1.0)]
        );
        assert_eq!(book.asks, vec![level(100.5, 4.0), level(102.0, 2.0)]);
    }

    #[test]
    fn later_diff_must_continue_previous_final_update_id() {
        let mut book = LocalBook::from_snapshot(snapshot()).unwrap();
        book.apply_diff(DepthDiff {
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![level(100.0, 3.0)],
            asks: vec![],
        })
        .unwrap();
        book.apply_diff(DepthDiff {
            first_update_id: 102,
            final_update_id: 102,
            bids: vec![level(99.0, 0.0)],
            asks: vec![level(101.5, 2.0)],
        })
        .unwrap();

        assert_eq!(book.last_update_id, 102);
        assert_eq!(book.bids, vec![level(100.0, 3.0)]);
        assert_eq!(
            book.asks,
            vec![level(101.0, 1.0), level(101.5, 2.0), level(102.0, 2.0)]
        );
    }

    #[test]
    fn out_of_order_diff_returns_typed_error() {
        let mut book = LocalBook::from_snapshot(snapshot()).unwrap();
        let err = book
            .apply_diff(DepthDiff {
                first_update_id: 103,
                final_update_id: 104,
                bids: vec![],
                asks: vec![],
            })
            .unwrap_err();

        assert_eq!(
            err,
            LocalBookError::DiffGap {
                expected_first_update_id: 101,
                first_update_id: 103,
            }
        );
    }
}
