//! Pure-Rust model compatibility for the deterministic runtime.
//!
//! New model training lives in the two separate Rust research lanes. This crate
//! keeps only verified ONNX loading through Tract; it has no synthetic or mock
//! model fallback.

mod onnx_lob_strategy;

pub use onnx_lob_strategy::{
    OnnxLobStrategy, OnnxLobStrategyConfig, OnnxLobStrategyValidationError,
};
