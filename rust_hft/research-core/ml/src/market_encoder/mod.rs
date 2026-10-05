//! Two-stage market representation learning. No order, dispatch or holdout authority.
mod artifacts;
mod network;
mod training;
pub use artifacts::{MarketEncoderCheckpoint, MarketTaskModel};
pub use training::{adapt_market_encoder, pretrain_market_encoder};
#[cfg(test)]
mod tests;
