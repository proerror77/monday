//! WebSocket helper utilities for adapters.
//!
//! Provides common reconnection logic and backoff strategies.
//! For full WebSocket client functionality, use `integration::ws::ReconnectingWsClient`.

use std::time::Duration;

/// Configuration for WebSocket reconnection behavior.
#[derive(Clone, Debug)]
pub struct ReconnectConfig {
    /// Maximum number of reconnection attempts before giving up.
    pub max_attempts: u32,
    /// Base delay for exponential backoff in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum delay cap in milliseconds.
    pub max_delay_ms: u64,
    /// Multiplier for exponential backoff (typically 2).
    pub backoff_multiplier: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay_ms: 500,
            max_delay_ms: 32_000, // Cap at ~32 seconds
            backoff_multiplier: 2,
        }
    }
}

impl ReconnectConfig {
    /// Create a new config with custom max attempts and base delay.
    pub fn new(max_attempts: u32, base_delay_ms: u64) -> Self {
        Self {
            max_attempts,
            base_delay_ms,
            ..Default::default()
        }
    }

    /// Calculate the delay for a given attempt number (1-indexed).
    ///
    /// Uses exponential backoff: `base_delay * multiplier^(attempt-1)`
    /// with a configurable cap to prevent excessive delays.
    ///
    /// # Example
    /// ```
    /// use adapters_common::ws_helpers::ReconnectConfig;
    ///
    /// let config = ReconnectConfig::default();
    /// assert_eq!(config.calculate_delay(1).as_millis(), 500);  // 500ms
    /// assert_eq!(config.calculate_delay(2).as_millis(), 1000); // 1s
    /// assert_eq!(config.calculate_delay(3).as_millis(), 2000); // 2s
    /// ```
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        let attempt = attempt.max(1); // Ensure at least 1
        let exponent = (attempt - 1).min(6); // Cap exponent to prevent overflow
        let delay_ms = self
            .base_delay_ms
            .saturating_mul(self.backoff_multiplier.pow(exponent) as u64);
        let capped_delay = delay_ms.min(self.max_delay_ms);
        Duration::from_millis(capped_delay)
    }

    /// Check if more attempts are allowed.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt <= self.max_attempts
    }
}

/// Simple backoff calculator for adapters that manage their own reconnection.
///
/// This is a stateless utility function that matches the common pattern
/// used across multiple adapters: `BASE_DELAY_MS * (1 << (attempts - 1).min(6))`
#[inline]
pub fn calculate_exponential_backoff(attempt: u32, base_delay_ms: u64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6);
    let delay_ms = base_delay_ms.saturating_mul(1u64 << exponent);
    Duration::from_millis(delay_ms)
}

/// Standard reconnection constants used across adapters.
pub mod constants {
    /// Default maximum reconnection attempts.
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;
    /// Default base delay in milliseconds.
    pub const DEFAULT_BASE_DELAY_MS: u64 = 500;
    /// Default fixed reconnect interval in milliseconds (for non-exponential backoff).
    pub const DEFAULT_RECONNECT_INTERVAL_MS: u64 = 5000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        // Test standard exponential backoff pattern
        assert_eq!(calculate_exponential_backoff(1, 500), Duration::from_millis(500));
        assert_eq!(calculate_exponential_backoff(2, 500), Duration::from_millis(1000));
        assert_eq!(calculate_exponential_backoff(3, 500), Duration::from_millis(2000));
        assert_eq!(calculate_exponential_backoff(4, 500), Duration::from_millis(4000));
        assert_eq!(calculate_exponential_backoff(5, 500), Duration::from_millis(8000));
        assert_eq!(calculate_exponential_backoff(6, 500), Duration::from_millis(16000));
        // Should cap at 2^6 = 64x base
        assert_eq!(calculate_exponential_backoff(7, 500), Duration::from_millis(32000));
        assert_eq!(calculate_exponential_backoff(10, 500), Duration::from_millis(32000));
    }

    #[test]
    fn test_reconnect_config() {
        let config = ReconnectConfig::default();

        assert!(config.should_retry(1));
        assert!(config.should_retry(5));
        assert!(!config.should_retry(6));

        assert_eq!(config.calculate_delay(1), Duration::from_millis(500));
        assert_eq!(config.calculate_delay(2), Duration::from_millis(1000));
    }

    #[test]
    fn test_zero_attempt() {
        // Edge case: attempt 0 should be treated as attempt 1
        let config = ReconnectConfig::default();
        assert_eq!(config.calculate_delay(0), Duration::from_millis(500));
    }
}
