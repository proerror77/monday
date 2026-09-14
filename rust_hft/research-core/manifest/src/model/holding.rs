//! Shared clock semantics for one non-overlapping horizon position. This state
//! chooses lifecycle actions; it never interprets predictions or changes costs.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HorizonHoldingPolicyV1 {
    pub horizon_millis: u64,
}

impl HorizonHoldingPolicyV1 {
    pub fn duration_micros(&self) -> Result<u64, String> {
        self.horizon_millis
            .checked_mul(1000)
            .filter(|v| *v > 0)
            .ok_or_else(|| "holding horizon must be positive and fit the decision clock".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HorizonPositionAction {
    Flat,
    Enter(f64),
    Hold(f64),
    /// A late close is observable evidence, never a horizon-complete trade.
    Exit {
        due_at_micros: u64,
        late: bool,
    },
}

impl HorizonPositionAction {
    pub fn target(self) -> f64 {
        match self {
            Self::Enter(v) | Self::Hold(v) => v,
            Self::Flat | Self::Exit { .. } => 0.0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HorizonPositionState {
    open: Option<(u64, u64, f64)>,
    last_clock: Option<u64>,
    last_exit: Option<u64>,
}

impl HorizonPositionState {
    pub fn is_holding(&self) -> bool {
        self.open.is_some()
    }

    pub fn advance(
        &mut self,
        policy: &HorizonHoldingPolicyV1,
        now: u64,
        can_enter: bool,
        entry_target: Option<f64>,
    ) -> Result<HorizonPositionAction, String> {
        let duration = policy.duration_micros()?;
        if self.last_clock.is_some_and(|last| now < last)
            || entry_target.is_some_and(|v| !v.is_finite() || v.abs() > 1.0)
        {
            return Err("invalid horizon position clock or entry target".into());
        }
        self.last_clock = Some(now);
        if let Some((_, due, target)) = self.open {
            if now < due {
                return Ok(HorizonPositionAction::Hold(target));
            }
            self.open = None;
            self.last_exit = Some(now);
            return Ok(HorizonPositionAction::Exit {
                due_at_micros: due,
                late: now > due,
            });
        }
        if self.last_exit == Some(now) || !can_enter {
            return Ok(HorizonPositionAction::Flat);
        }
        if let Some(target) = entry_target.filter(|v| v.abs() > f64::EPSILON) {
            let due = now
                .checked_add(duration)
                .ok_or("holding deadline overflowed")?;
            self.open = Some((now, due, target));
            return Ok(HorizonPositionAction::Enter(target));
        }
        Ok(HorizonPositionAction::Flat)
    }

    /// An entry that emitted no executable order must not create a phantom hold.
    pub fn reject_entry(&mut self, now: u64) -> Result<(), String> {
        if self.open.is_none_or(|(opened, _, _)| opened != now) {
            return Err("only the current unexecuted entry can be rejected".into());
        }
        self.open = None;
        self.last_exit = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn holding_ignores_new_signals_and_closes_before_reentry() {
        for seconds in [5, 10, 30] {
            let policy = HorizonHoldingPolicyV1 {
                horizon_millis: seconds * 1000,
            };
            let mut state = HorizonPositionState::default();
            assert_eq!(
                state.advance(&policy, 0, true, Some(0.4)).unwrap(),
                HorizonPositionAction::Enter(0.4)
            );
            for t in 1..seconds {
                assert_eq!(
                    state
                        .advance(&policy, t * 1_000_000, true, Some(-0.9))
                        .unwrap(),
                    HorizonPositionAction::Hold(0.4)
                );
            }
            let close = seconds * 1_000_000;
            assert_eq!(
                state.advance(&policy, close, true, Some(-0.9)).unwrap(),
                HorizonPositionAction::Exit {
                    due_at_micros: close,
                    late: false
                }
            );
            assert_eq!(
                state.advance(&policy, close, true, Some(1.0)).unwrap(),
                HorizonPositionAction::Flat
            );
            assert_eq!(
                state
                    .advance(&policy, close + 1_000_000, true, Some(-0.3))
                    .unwrap(),
                HorizonPositionAction::Enter(-0.3)
            );
        }
    }
    #[test]
    fn rejected_entries_and_late_closes_are_not_silently_accepted() {
        let policy = HorizonHoldingPolicyV1 {
            horizon_millis: 5000,
        };
        let mut state = HorizonPositionState::default();
        assert_eq!(
            state.advance(&policy, 0, false, Some(1.0)).unwrap(),
            HorizonPositionAction::Flat
        );
        state.advance(&policy, 1_000_000, true, Some(1.0)).unwrap();
        state.reject_entry(1_000_000).unwrap();
        assert!(!state.is_holding());
        assert_eq!(
            state.advance(&policy, 1_000_000, true, Some(1.0)).unwrap(),
            HorizonPositionAction::Flat
        );
        state.advance(&policy, 2_000_000, true, Some(1.0)).unwrap();
        assert!(state.advance(&policy, 1, true, None).is_err());
        assert_eq!(
            state.advance(&policy, 8_000_000, true, None).unwrap(),
            HorizonPositionAction::Exit {
                due_at_micros: 7_000_000,
                late: true
            }
        );
    }
}
