use serde::Deserialize;
use std::error::Error;
use std::fmt;

/// Borrowed representation of a Binance Spot diff depth stream message.
///
/// It is designed for typed serde parsing and deliberately avoids building an
/// untyped JSON DOM on the market-data path.
#[derive(Debug, Deserialize)]
pub struct BinanceDepthUpdate<'a> {
    #[serde(rename = "e", borrow)]
    pub event_type: Option<&'a str>,
    #[serde(rename = "E")]
    pub event_time_ms: u64,
    #[serde(rename = "s", borrow)]
    pub symbol: &'a str,
    #[serde(rename = "U")]
    pub first_update_id: u64,
    #[serde(rename = "u")]
    pub final_update_id: u64,
    #[serde(rename = "b", borrow)]
    pub bids: Vec<[&'a str; 2]>,
    #[serde(rename = "a", borrow)]
    pub asks: Vec<[&'a str; 2]>,
}

/// Fixed-point depth update ready for the local book path.
#[derive(Debug, Clone)]
pub struct ParsedDepthUpdate {
    pub symbol_id: u32,
    pub exchange_ts_ns: i64,
    pub receive_ts_ns: i64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub bids: Vec<(i64, i64)>,
    pub asks: Vec<(i64, i64)>,
}

#[derive(Debug)]
pub enum ParseDepthError {
    Json(serde_json::Error),
    Price {
        side: &'static str,
        index: usize,
        source: ParseFixedError,
    },
    Qty {
        side: &'static str,
        index: usize,
        source: ParseFixedError,
    },
}

impl fmt::Display for ParseDepthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(err) => write!(f, "failed to parse Binance depth JSON: {err}"),
            Self::Price {
                side,
                index,
                source,
            } => {
                write!(f, "invalid {side} price at index {index}: {source}")
            }
            Self::Qty {
                side,
                index,
                source,
            } => {
                write!(f, "invalid {side} quantity at index {index}: {source}")
            }
        }
    }
}

impl Error for ParseDepthError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(err) => Some(err),
            Self::Price { source, .. } | Self::Qty { source, .. } => Some(source),
        }
    }
}

impl From<serde_json::Error> for ParseDepthError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Error returned by the non-floating fixed-point parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseFixedError {
    Empty,
    InvalidByte,
    TooManyFractionalDigits,
    Overflow,
}

impl fmt::Display for ParseFixedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty fixed-point value"),
            Self::InvalidByte => write!(f, "invalid fixed-point byte"),
            Self::TooManyFractionalDigits => {
                write!(f, "too many fractional digits for scale 1e6")
            }
            Self::Overflow => write!(f, "fixed-point value overflow"),
        }
    }
}

impl Error for ParseFixedError {}

/// Parse a positive decimal string into scale-1e6 fixed point without f64.
///
/// Binance sends price and quantity values as decimal strings. This parser is
/// intended for hot-path normalization after the typed JSON decoder has
/// borrowed those strings from the raw frame.
pub fn parse_fixed_6(input: &str) -> Result<i64, ParseFixedError> {
    if input.is_empty() {
        return Err(ParseFixedError::Empty);
    }

    let mut whole = 0_i64;
    let mut frac = 0_i64;
    let mut frac_digits = 0_u8;
    let mut seen_dot = false;
    let mut saw_digit = false;

    for byte in input.as_bytes() {
        match *byte {
            b'0'..=b'9' => {
                saw_digit = true;
                let digit = (byte - b'0') as i64;
                if seen_dot {
                    if frac_digits >= 6 {
                        if digit == 0 {
                            continue;
                        }
                        return Err(ParseFixedError::TooManyFractionalDigits);
                    }
                    frac = frac
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(ParseFixedError::Overflow)?;
                    frac_digits += 1;
                } else {
                    whole = whole
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(ParseFixedError::Overflow)?;
                }
            }
            b'.' if !seen_dot => {
                seen_dot = true;
            }
            _ => return Err(ParseFixedError::InvalidByte),
        }
    }

    if !saw_digit {
        return Err(ParseFixedError::Empty);
    }

    for _ in frac_digits..6 {
        frac *= 10;
    }

    whole
        .checked_mul(1_000_000)
        .and_then(|value| value.checked_add(frac))
        .ok_or(ParseFixedError::Overflow)
}

/// Decode and normalize one Binance depth JSON frame.
///
/// This is the baseline parser: typed serde, no untyped DOM, no floating-point
/// decimal parsing. SIMD/custom parsing should only replace this after latency
/// evidence shows parsing is the bottleneck.
pub fn parse_depth_update(
    raw: &[u8],
    symbol_id: u32,
    receive_ts_ns: i64,
) -> Result<ParsedDepthUpdate, ParseDepthError> {
    let update: BinanceDepthUpdate<'_> = serde_json::from_slice(raw)?;
    normalize_depth_update(update, symbol_id, receive_ts_ns)
}

pub fn normalize_depth_update(
    update: BinanceDepthUpdate<'_>,
    symbol_id: u32,
    receive_ts_ns: i64,
) -> Result<ParsedDepthUpdate, ParseDepthError> {
    let mut bids = Vec::with_capacity(update.bids.len());
    let mut asks = Vec::with_capacity(update.asks.len());

    for (idx, [price, qty]) in update.bids.into_iter().enumerate() {
        let price = parse_fixed_6(price).map_err(|source| ParseDepthError::Price {
            side: "bid",
            index: idx,
            source,
        })?;
        let qty = parse_fixed_6(qty).map_err(|source| ParseDepthError::Qty {
            side: "bid",
            index: idx,
            source,
        })?;
        bids.push((price, qty));
    }

    for (idx, [price, qty]) in update.asks.into_iter().enumerate() {
        let price = parse_fixed_6(price).map_err(|source| ParseDepthError::Price {
            side: "ask",
            index: idx,
            source,
        })?;
        let qty = parse_fixed_6(qty).map_err(|source| ParseDepthError::Qty {
            side: "ask",
            index: idx,
            source,
        })?;
        asks.push((price, qty));
    }

    Ok(ParsedDepthUpdate {
        symbol_id,
        exchange_ts_ns: (update.event_time_ms as i64) * 1_000_000,
        receive_ts_ns,
        first_update_id: update.first_update_id,
        final_update_id: update.final_update_id,
        bids,
        asks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_6_without_f64() {
        assert_eq!(parse_fixed_6("60000.12").unwrap(), 60_000_120_000);
        assert_eq!(parse_fixed_6("0.001234").unwrap(), 1_234);
        assert_eq!(parse_fixed_6("1").unwrap(), 1_000_000);
        assert_eq!(parse_fixed_6("1.000000").unwrap(), 1_000_000);
        assert_eq!(parse_fixed_6("1.0000000").unwrap(), 1_000_000);
    }

    #[test]
    fn rejects_lossy_fractional_precision() {
        assert_eq!(
            parse_fixed_6("1.0000001"),
            Err(ParseFixedError::TooManyFractionalDigits)
        );
        assert_eq!(parse_fixed_6("-1.0"), Err(ParseFixedError::InvalidByte));
        assert_eq!(parse_fixed_6(""), Err(ParseFixedError::Empty));
    }

    #[test]
    fn parses_depth_update_into_fixed_point_levels() {
        let raw = br#"{
            "e":"depthUpdate",
            "E":1700000000123,
            "s":"BTCUSDT",
            "U":100,
            "u":102,
            "b":[["60000.12","0.001234"]],
            "a":[["60001.00","0.002000"]]
        }"#;

        let update = parse_depth_update(raw, 1, 999).unwrap();

        assert_eq!(update.symbol_id, 1);
        assert_eq!(update.exchange_ts_ns, 1_700_000_000_123_000_000);
        assert_eq!(update.receive_ts_ns, 999);
        assert_eq!(update.first_update_id, 100);
        assert_eq!(update.final_update_id, 102);
        assert_eq!(update.bids, vec![(60_000_120_000, 1_234)]);
        assert_eq!(update.asks, vec![(60_001_000_000, 2_000)]);
    }
}
