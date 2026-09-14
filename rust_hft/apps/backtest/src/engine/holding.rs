//! Summarize actual IOC episodes while producing or independently reading the
//! existing trace. No extra replay, model evaluation or trace decoding pass.
use super::{HorizonHoldingPolicyV1, TargetPositionReplayTraceEvent};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HorizonExecutionSummaryV1 {
    pub planned_signal_horizon_micros: u64,
    pub entered_episodes: usize,
    pub closed_episodes: usize,
    pub incomplete_exit_orders: usize,
    pub delayed_exit_decisions: usize,
    pub min_actual_holding_micros: Option<u64>,
    pub max_actual_holding_micros: Option<u64>,
}

pub struct HorizonExecutionAccumulator {
    summary: HorizonExecutionSummaryV1,
    open: Option<(i64, i64)>,
    inventory: f64,
}

impl HorizonExecutionAccumulator {
    pub fn new(policy: &HorizonHoldingPolicyV1) -> Result<Self> {
        Ok(Self {
            summary: HorizonExecutionSummaryV1 {
                planned_signal_horizon_micros: policy
                    .duration_micros()
                    .map_err(anyhow::Error::msg)?,
                ..Default::default()
            },
            open: None,
            inventory: 0.0,
        })
    }

    pub fn observe(&mut self, event: &TargetPositionReplayTraceEvent) -> Result<()> {
        if !event.inventory_after.is_finite()
            || event.arrival_timestamp_us < event.decision_timestamp_us
        {
            bail!("invalid holding execution trace");
        }
        let flat = event.inventory_after.abs() <= f64::EPSILON;
        if let Some((decision, arrival)) = self.open {
            let due = decision
                .checked_add(i64::try_from(self.summary.planned_signal_horizon_micros)?)
                .context("holding execution deadline overflow")?;
            if event.requested_quantity > f64::EPSILON {
                if event.decision_timestamp_us < due {
                    bail!("holding execution changed quantity before its deadline");
                }
                if !flat
                    && (event.inventory_after.signum() != self.inventory.signum()
                        || event.inventory_after.abs() > self.inventory.abs() + 1e-8)
                {
                    bail!("holding execution increased or reversed an open episode");
                }
                if event.decision_timestamp_us > due {
                    self.summary.delayed_exit_decisions += 1;
                }
                if !flat {
                    self.summary.incomplete_exit_orders += 1;
                }
            }
            if flat {
                let duration = u64::try_from(
                    event
                        .arrival_timestamp_us
                        .checked_sub(arrival)
                        .context("actual holding duration overflow")?,
                )?;
                self.summary.closed_episodes += 1;
                self.summary.min_actual_holding_micros = Some(
                    self.summary
                        .min_actual_holding_micros
                        .map_or(duration, |old| old.min(duration)),
                );
                self.summary.max_actual_holding_micros = Some(
                    self.summary
                        .max_actual_holding_micros
                        .map_or(duration, |old| old.max(duration)),
                );
                self.open = None;
            }
        } else if !flat {
            if event.filled_quantity <= f64::EPSILON || event.target_position.abs() <= f64::EPSILON
            {
                bail!("holding episode has no filled entry");
            }
            self.summary.entered_episodes += 1;
            self.open = Some((event.decision_timestamp_us, event.arrival_timestamp_us));
        }
        self.inventory = event.inventory_after;
        Ok(())
    }

    pub fn finish(self) -> Result<HorizonExecutionSummaryV1> {
        if self.open.is_some() {
            bail!("holding execution ended with an open episode");
        }
        Ok(self.summary)
    }
}
