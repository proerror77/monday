mod artifact;
#[allow(dead_code)] // Used by the stacked semantic-verifier PR; no runtime path consumes it alone.
mod wire;
pub use artifact::*;
