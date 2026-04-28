use super::features::FeatureSnapshot;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalSide {
    Long = 1,
    Short = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub symbol_id: u32,
    pub side: SignalSide,
    pub confidence_fp: i64,
    pub edge_bps: i32,
    pub ts_ns: i64,
    pub expire_ts_ns: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct SignalRules {
    pub min_obi1_fp: i64,
    pub min_obi3_fp: i64,
    pub max_spread: i64,
    pub max_book_staleness_ns: i64,
    pub ttl_ns: i64,
}

impl Default for SignalRules {
    fn default() -> Self {
        Self {
            min_obi1_fp: 250_000,
            min_obi3_fp: 150_000,
            max_spread: i64::MAX,
            max_book_staleness_ns: i64::MAX,
            ttl_ns: 50_000_000,
        }
    }
}

impl SignalRules {
    pub fn evaluate(&self, features: &FeatureSnapshot) -> Option<Signal> {
        if features.spread <= 0 || features.spread > self.max_spread {
            return None;
        }
        if features.book_staleness_ns > self.max_book_staleness_ns {
            return None;
        }

        let side = if features.obi1_fp >= self.min_obi1_fp
            && features.obi3_fp >= self.min_obi3_fp
            && features.microgap > 0
        {
            SignalSide::Long
        } else if features.obi1_fp <= -self.min_obi1_fp
            && features.obi3_fp <= -self.min_obi3_fp
            && features.microgap < 0
        {
            SignalSide::Short
        } else {
            return None;
        };

        let confidence_fp = features.obi1_fp.abs().min(1_000_000);

        Some(Signal {
            symbol_id: features.symbol_id,
            side,
            confidence_fp,
            edge_bps: 0,
            ts_ns: features.ts_ns,
            expire_ts_ns: features.ts_ns + self.ttl_ns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_has_expiry() {
        let features = FeatureSnapshot {
            symbol_id: 1,
            mid: 100,
            spread: 1,
            obi1_fp: 300_000,
            obi3_fp: 200_000,
            microgap: 1,
            flow_1s: 0,
            book_staleness_ns: 0,
            ts_ns: 1_000,
        };
        let rules = SignalRules {
            ttl_ns: 500,
            ..SignalRules::default()
        };

        let signal = rules.evaluate(&features).unwrap();

        assert_eq!(signal.side, SignalSide::Long);
        assert_eq!(signal.ts_ns, 1_000);
        assert_eq!(signal.expire_ts_ns, 1_500);
    }
}
