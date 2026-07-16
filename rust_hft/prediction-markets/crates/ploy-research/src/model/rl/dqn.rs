//! DQN agent skeleton. Full burn neural network wiring is a follow-up task.
//! This file establishes the interface so the rest of the pipeline compiles.
#![allow(dead_code)]
use crate::factors::FactorObservation;
use crate::model::traits::{RlAgent, Transition};
use crate::signal::traits::{Signal, SignalSource};
use std::path::Path;

pub struct DqnAgent {
    pub epsilon: f64,
    pub state_dim: usize,
    pub action_dim: usize,
}

impl DqnAgent {
    pub fn new(state_dim: usize, action_dim: usize) -> Self {
        Self {
            epsilon: 1.0,
            state_dim,
            action_dim,
        }
    }
}

impl SignalSource for DqnAgent {
    fn signal(&self, _obs: &FactorObservation) -> Signal {
        Signal::Hold
    }
}

impl RlAgent for DqnAgent {
    fn act(&self, _state: &[f64], _epsilon: f64) -> u8 {
        0
    }
    fn update(&mut self, _transition: &Transition) {}
    fn save(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    fn load(_path: &Path) -> anyhow::Result<Self> {
        Ok(Self::new(16, 3))
    }
}
