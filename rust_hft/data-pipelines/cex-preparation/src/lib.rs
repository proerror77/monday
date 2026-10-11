//! Bounded CEX acquisition and immutable preparation. No research execution or scheduling.
mod cli;
mod inventory;
mod prepare;
pub use cli::run;
