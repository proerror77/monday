use crate::factors::FactorObservation;
use crate::signal::traits::SignalSource;
use std::path::Path;

/// Supervised ML model: fits on labelled observations, produces signals.
pub trait StrategyModel: SignalSource {
    fn fit(&mut self, obs: &[FactorObservation], labels: &[bool]);
    /// Returns (factor_name, importance_score) pairs, sorted descending.
    fn feature_importance(&self) -> Vec<(String, f64)>;
    fn save(&self, path: &Path) -> anyhow::Result<()>;
    fn load(path: &Path) -> anyhow::Result<Self>
    where
        Self: Sized;
}

/// RL transition: one step of experience.
#[derive(Debug, Clone)]
pub struct Transition {
    pub state: Vec<f64>,
    pub action: u8, // 0=Hold, 1=Buy, 2=Sell
    pub reward: f64,
    pub next_state: Vec<f64>,
    pub done: bool,
}

/// RL agent: acts in an environment and learns from transitions.
pub trait RlAgent: SignalSource {
    fn act(&self, state: &[f64], epsilon: f64) -> u8;
    fn update(&mut self, transition: &Transition);
    fn save(&self, path: &Path) -> anyhow::Result<()>;
    fn load(path: &Path) -> anyhow::Result<Self>
    where
        Self: Sized;
}
