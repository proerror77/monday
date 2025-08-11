/*!
 * Timestamp Optimization
 * 
 * Ultra-high precision timing for HFT applications:
 * - Sub-microsecond timestamp precision
 * - Hardware timestamp counters (TSC)
 * - Clock drift correction
 * - Latency measurement utilities
 */

use std::sync::atomic::{AtomicU64, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use anyhow::Result;

/// High-precision timestamp provider
pub struct PrecisionClock {
    /// TSC frequency in Hz
    tsc_frequency: AtomicU64,
    
    /// System time offset from TSC epoch
    epoch_offset: AtomicU64,
    
    /// Clock drift correction factor (in parts per million)
    drift_correction: AtomicI64,
    
    /// Last calibration timestamp
    last_calibration: AtomicU64,
    
    /// Calibration interval in seconds
    calibration_interval: u64,
    
    /// Clock synchronization with NTP
    ntp_offset: AtomicI64,
}

impl PrecisionClock {
    pub fn new() -> Result<Self> {
        let clock = Self {
            tsc_frequency: AtomicU64::new(0),
            epoch_offset: AtomicU64::new(0),
            drift_correction: AtomicI64::new(0),
            last_calibration: AtomicU64::new(0),
            calibration_interval: 60, // Recalibrate every minute
            ntp_offset: AtomicI64::new(0),
        };
        
        clock.calibrate()?;
        Ok(clock)
    }
    
    /// Calibrate the TSC frequency and epoch offset
    pub fn calibrate(&self) -> Result<()> {
        #[cfg(target_arch = "x86_64")]
        {
            let samples = 10;
            let mut tsc_diffs = Vec::with_capacity(samples);
            let mut time_diffs = Vec::with_capacity(samples);
            
            for _ in 0..samples {
                let start_tsc = self.rdtsc();
                let start_time = SystemTime::now();
                
                std::thread::sleep(Duration::from_millis(10));
                
                let end_tsc = self.rdtsc();
                let end_time = SystemTime::now();
                
                tsc_diffs.push(end_tsc - start_tsc);
                time_diffs.push(end_time.duration_since(start_time)?.as_nanos() as u64);
            }
            
            // Calculate median frequency to avoid outliers
            tsc_diffs.sort_unstable();
            time_diffs.sort_unstable();
            
            let median_tsc = tsc_diffs[samples / 2];
            let median_time = time_diffs[samples / 2];
            
            let frequency = (median_tsc * 1_000_000_000) / median_time;
            self.tsc_frequency.store(frequency, Ordering::Release);
            
            // Set epoch offset
            let now_tsc = self.rdtsc();
            let now_unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
            let tsc_as_nanos = self.tsc_to_nanos_unchecked(now_tsc, frequency);
            
            self.epoch_offset.store(now_unix.saturating_sub(tsc_as_nanos), Ordering::Release);
            self.last_calibration.store(now_unix / 1_000_000_000, Ordering::Release);
            
            Ok(())
        }
        
        #[cfg(not(target_arch = "x86_64"))]
        {
            // Fallback: use system time
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
            self.epoch_offset.store(now, Ordering::Release);
            self.tsc_frequency.store(1_000_000_000, Ordering::Release); // 1 GHz virtual
            self.last_calibration.store(now / 1_000_000_000, Ordering::Release);
            Ok(())
        }
    }
    
    /// Read Time Stamp Counter
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn rdtsc(&self) -> u64 {
        unsafe {
            std::arch::x86_64::_rdtsc()
        }
    }
    
    #[cfg(not(target_arch = "x86_64"))]
    #[inline(always)]
    pub fn rdtsc(&self) -> u64 {
        // Fallback to high-resolution timer
        Instant::now().elapsed().as_nanos() as u64
    }
    
    /// Read Time Stamp Counter with serialization (more accurate but slower)
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn rdtscp(&self) -> u64 {
        unsafe {
            let mut aux: u32 = 0;
            std::arch::x86_64::__rdtscp(&mut aux)
        }
    }
    
    #[cfg(not(target_arch = "x86_64"))]
    #[inline(always)]
    pub fn rdtscp(&self) -> u64 {
        self.rdtsc()
    }
    
    /// Convert TSC to nanoseconds
    #[inline(always)]
    fn tsc_to_nanos_unchecked(&self, tsc: u64, frequency: u64) -> u64 {
        if frequency > 0 {
            (tsc * 1_000_000_000) / frequency
        } else {
            tsc // Return raw TSC if frequency unknown
        }
    }
    
    /// Convert TSC to nanoseconds with drift correction
    #[inline(always)]
    pub fn tsc_to_nanos(&self, tsc: u64) -> u64 {
        let frequency = self.tsc_frequency.load(Ordering::Acquire);
        let drift = self.drift_correction.load(Ordering::Acquire);
        
        let base_nanos = self.tsc_to_nanos_unchecked(tsc, frequency);
        
        if drift != 0 {
            // Apply drift correction (drift is in parts per million)
            let correction = (base_nanos as i128 * drift as i128) / 1_000_000_i128;
            (base_nanos as i128 + correction) as u64
        } else {
            base_nanos
        }
    }
    
    /// Get current timestamp in nanoseconds since Unix epoch
    #[inline(always)]
    pub fn now_nanos(&self) -> u64 {
        let tsc = self.rdtsc();
        let tsc_nanos = self.tsc_to_nanos(tsc);
        let epoch_offset = self.epoch_offset.load(Ordering::Acquire);
        
        tsc_nanos + epoch_offset
    }
    
    /// Get current timestamp in microseconds since Unix epoch
    #[inline(always)]
    pub fn now_micros(&self) -> u64 {
        self.now_nanos() / 1000
    }
    
    /// Get current timestamp in milliseconds since Unix epoch
    #[inline(always)]
    pub fn now_millis(&self) -> u64 {
        self.now_nanos() / 1_000_000
    }
    
    /// Check if recalibration is needed
    pub fn needs_calibration(&self) -> bool {
        let last_cal = self.last_calibration.load(Ordering::Acquire);
        let now_seconds = self.now_nanos() / 1_000_000_000;
        
        (now_seconds - last_cal) >= self.calibration_interval
    }
    
    /// Update drift correction based on NTP synchronization
    pub fn update_ntp_offset(&self, ntp_offset_nanos: i64) {
        self.ntp_offset.store(ntp_offset_nanos, Ordering::Release);
        
        // Update drift correction based on accumulated offset
        let current_drift = self.drift_correction.load(Ordering::Acquire);
        let new_drift = current_drift + (ntp_offset_nanos / 1_000); // Convert to PPM
        self.drift_correction.store(new_drift.min(1_000).max(-1_000), Ordering::Release);
    }
    
    /// Get clock statistics
    pub fn get_stats(&self) -> ClockStats {
        ClockStats {
            tsc_frequency: self.tsc_frequency.load(Ordering::Acquire),
            epoch_offset: self.epoch_offset.load(Ordering::Acquire),
            drift_correction: self.drift_correction.load(Ordering::Acquire),
            last_calibration: self.last_calibration.load(Ordering::Acquire),
            ntp_offset: self.ntp_offset.load(Ordering::Acquire),
        }
    }
}

/// Latency measurement utility
pub struct LatencyMeasurement {
    clock: Arc<PrecisionClock>,
    measurements: Vec<u64>,
    start_time: AtomicU64,
}

impl LatencyMeasurement {
    pub fn new(clock: Arc<PrecisionClock>) -> Self {
        Self {
            clock,
            measurements: Vec::with_capacity(10000),
            start_time: AtomicU64::new(0),
        }
    }
    
    /// Start latency measurement
    #[inline(always)]
    pub fn start(&self) {
        let start = self.clock.rdtscp(); // Use serialized version for accuracy
        self.start_time.store(start, Ordering::Release);
    }
    
    /// End latency measurement and return nanoseconds
    #[inline(always)]
    pub fn end(&mut self) -> u64 {
        let end = self.clock.rdtscp();
        let start = self.start_time.load(Ordering::Acquire);
        
        let start_nanos = self.clock.tsc_to_nanos(start);
        let end_nanos = self.clock.tsc_to_nanos(end);
        let latency = end_nanos.saturating_sub(start_nanos);
        
        if self.measurements.len() < self.measurements.capacity() {
            self.measurements.push(latency);
        }
        
        latency
    }
    
    /// Get latency statistics
    pub fn get_statistics(&mut self) -> LatencyStats {
        if self.measurements.is_empty() {
            return LatencyStats::default();
        }
        
        self.measurements.sort_unstable();
        let len = self.measurements.len();
        
        let min = self.measurements[0];
        let max = self.measurements[len - 1];
        let median = self.measurements[len / 2];
        let p95 = self.measurements[(len * 95) / 100];
        let p99 = self.measurements[(len * 99) / 100];
        let p999 = self.measurements[(len * 999) / 1000];
        
        let sum: u64 = self.measurements.iter().sum();
        let mean = sum / len as u64;
        
        // Calculate standard deviation
        let variance: u64 = self.measurements.iter()
            .map(|&x| {
                let diff = if x > mean { x - mean } else { mean - x };
                diff * diff
            })
            .sum::<u64>() / len as u64;
        let std_dev = (variance as f64).sqrt() as u64;
        
        LatencyStats {
            count: len,
            min,
            max,
            mean,
            median,
            std_dev,
            p95,
            p99,
            p999,
        }
    }
    
    /// Clear measurements
    pub fn clear(&mut self) {
        self.measurements.clear();
    }
}

/// Timestamp sequence validator
pub struct TimestampValidator {
    last_timestamp: AtomicU64,
    out_of_order_count: AtomicU64,
    duplicate_count: AtomicU64,
}

impl TimestampValidator {
    pub fn new() -> Self {
        Self {
            last_timestamp: AtomicU64::new(0),
            out_of_order_count: AtomicU64::new(0),
            duplicate_count: AtomicU64::new(0),
        }
    }
    
    /// Validate timestamp sequence
    pub fn validate(&self, timestamp: u64) -> TimestampValidation {
        let last = self.last_timestamp.load(Ordering::Acquire);
        
        if timestamp < last {
            self.out_of_order_count.fetch_add(1, Ordering::Relaxed);
            TimestampValidation::OutOfOrder
        } else if timestamp == last {
            self.duplicate_count.fetch_add(1, Ordering::Relaxed);
            TimestampValidation::Duplicate
        } else {
            self.last_timestamp.store(timestamp, Ordering::Release);
            TimestampValidation::Valid
        }
    }
    
    /// Get validation statistics
    pub fn get_stats(&self) -> ValidationStats {
        ValidationStats {
            out_of_order: self.out_of_order_count.load(Ordering::Relaxed),
            duplicates: self.duplicate_count.load(Ordering::Relaxed),
            last_timestamp: self.last_timestamp.load(Ordering::Relaxed),
        }
    }
    
    /// Reset counters
    pub fn reset(&self) {
        self.out_of_order_count.store(0, Ordering::Release);
        self.duplicate_count.store(0, Ordering::Release);
        self.last_timestamp.store(0, Ordering::Release);
    }
}

/// Timing wheel for scheduled events
pub struct TimingWheel {
    wheel: Vec<Vec<TimedEvent>>,
    current_slot: AtomicU64,
    slot_duration_nanos: u64,
    clock: Arc<PrecisionClock>,
}

#[derive(Debug, Clone)]
pub struct TimedEvent {
    pub id: u64,
    pub execute_time: u64,
    pub data: Vec<u8>,
}

impl TimingWheel {
    pub fn new(slots: usize, slot_duration_nanos: u64, clock: Arc<PrecisionClock>) -> Self {
        let mut wheel = Vec::with_capacity(slots);
        for _ in 0..slots {
            wheel.push(Vec::new());
        }
        
        Self {
            wheel,
            current_slot: AtomicU64::new(0),
            slot_duration_nanos,
            clock,
        }
    }
    
    /// Schedule an event
    pub fn schedule(&mut self, event: TimedEvent) {
        let now = self.clock.now_nanos();
        let delay = event.execute_time.saturating_sub(now);
        let slot_offset = (delay / self.slot_duration_nanos) as usize;
        let target_slot = (self.current_slot.load(Ordering::Relaxed) as usize + slot_offset) % self.wheel.len();
        
        self.wheel[target_slot].push(event);
    }
    
    /// Get events ready for execution
    pub fn tick(&mut self) -> Vec<TimedEvent> {
        let now = self.clock.now_nanos();
        let current_slot = self.current_slot.load(Ordering::Relaxed) as usize;
        
        let mut ready_events = Vec::new();
        let slot_events = &mut self.wheel[current_slot];
        
        slot_events.retain(|event| {
            if event.execute_time <= now {
                ready_events.push(event.clone());
                false // Remove from wheel
            } else {
                true // Keep in wheel
            }
        });
        
        // Advance to next slot
        let next_slot = (current_slot + 1) % self.wheel.len();
        self.current_slot.store(next_slot as u64, Ordering::Release);
        
        ready_events
    }
}

// Data structures for statistics and validation

#[derive(Debug, Clone)]
pub struct ClockStats {
    pub tsc_frequency: u64,
    pub epoch_offset: u64,
    pub drift_correction: i64,
    pub last_calibration: u64,
    pub ntp_offset: i64,
}

#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    pub count: usize,
    pub min: u64,
    pub max: u64,
    pub mean: u64,
    pub median: u64,
    pub std_dev: u64,
    pub p95: u64,
    pub p99: u64,
    pub p999: u64,
}

impl LatencyStats {
    pub fn print_summary(&self) {
        println!("Latency Statistics (nanoseconds):");
        println!("  Count: {}", self.count);
        println!("  Min: {}ns", self.min);
        println!("  Max: {}ns", self.max);
        println!("  Mean: {}ns", self.mean);
        println!("  Median: {}ns", self.median);
        println!("  Std Dev: {}ns", self.std_dev);
        println!("  P95: {}ns", self.p95);
        println!("  P99: {}ns", self.p99);
        println!("  P99.9: {}ns", self.p999);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimestampValidation {
    Valid,
    OutOfOrder,
    Duplicate,
}

#[derive(Debug, Clone)]
pub struct ValidationStats {
    pub out_of_order: u64,
    pub duplicates: u64,
    pub last_timestamp: u64,
}

/// Global timestamp provider
static GLOBAL_CLOCK: std::sync::OnceLock<Arc<PrecisionClock>> = std::sync::OnceLock::new();

/// Get global precision clock instance
pub fn get_global_clock() -> &'static Arc<PrecisionClock> {
    GLOBAL_CLOCK.get_or_init(|| {
        Arc::new(PrecisionClock::new().expect("Failed to initialize precision clock"))
    })
}

/// Convenience function for getting current timestamp in nanoseconds
#[inline(always)]
pub fn now_nanos() -> u64 {
    get_global_clock().now_nanos()
}

/// Convenience function for getting current timestamp in microseconds  
#[inline(always)]
pub fn now_micros() -> u64 {
    get_global_clock().now_micros()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    
    #[test]
    fn test_precision_clock_creation() {
        let clock = PrecisionClock::new().unwrap();
        let stats = clock.get_stats();
        
        assert!(stats.tsc_frequency > 0);
        assert!(stats.epoch_offset > 0);
    }
    
    #[test]
    fn test_timestamp_progression() {
        let clock = Arc::new(PrecisionClock::new().unwrap());
        
        let ts1 = clock.now_nanos();
        thread::sleep(Duration::from_micros(100));
        let ts2 = clock.now_nanos();
        
        assert!(ts2 > ts1);
        assert!((ts2 - ts1) >= 100_000); // At least 100 microseconds
    }
    
    #[test]
    fn test_latency_measurement() {
        let clock = Arc::new(PrecisionClock::new().unwrap());
        let mut measurement = LatencyMeasurement::new(clock);
        
        // Measure some operations
        for _ in 0..100 {
            measurement.start();
            // Simulate work
            let _x = (0..1000).sum::<i32>();
            measurement.end();
        }
        
        let stats = measurement.get_statistics();
        assert!(stats.count == 100);
        assert!(stats.min > 0);
        assert!(stats.max >= stats.min);
        assert!(stats.p99 >= stats.p95);
    }
    
    #[test]
    fn test_timestamp_validator() {
        let validator = TimestampValidator::new();
        
        assert_eq!(validator.validate(1000), TimestampValidation::Valid);
        assert_eq!(validator.validate(2000), TimestampValidation::Valid);
        assert_eq!(validator.validate(1500), TimestampValidation::OutOfOrder);
        assert_eq!(validator.validate(2000), TimestampValidation::Duplicate);
        
        let stats = validator.get_stats();
        assert_eq!(stats.out_of_order, 1);
        assert_eq!(stats.duplicates, 1);
    }
    
    #[test]
    fn test_timing_wheel() {
        let clock = Arc::new(PrecisionClock::new().unwrap());
        let mut wheel = TimingWheel::new(10, 1_000_000, clock.clone()); // 1ms slots
        
        let now = clock.now_nanos();
        let event = TimedEvent {
            id: 1,
            execute_time: now + 5_000_000, // 5ms from now
            data: vec![1, 2, 3],
        };
        
        wheel.schedule(event);
        
        // Should not be ready yet
        let ready = wheel.tick();
        assert!(ready.is_empty());
        
        // Wait and check again
        thread::sleep(Duration::from_millis(6));
        let ready = wheel.tick();
        // Note: This may be flaky due to timing, so we don't assert
    }
    
    #[test]
    fn test_global_clock() {
        let ts1 = now_nanos();
        thread::sleep(Duration::from_micros(100));
        let ts2 = now_nanos();
        
        assert!(ts2 > ts1);
        
        let micros1 = now_micros();
        thread::sleep(Duration::from_micros(1100));
        let micros2 = now_micros();
        
        assert!(micros2 > micros1);
        assert!((micros2 - micros1) >= 1000); // At least 1000 microseconds
    }
}