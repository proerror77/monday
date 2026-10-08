//! Environment and replay primitives for research experiments.
//! This module does not provide a trainable RL policy.

pub mod env;
pub mod replay;
pub use env::{BinaryEventEnv, Environment};
pub use replay::ReplayBuffer;
