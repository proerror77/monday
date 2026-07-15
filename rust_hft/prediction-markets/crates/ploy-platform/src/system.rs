use chrono::{DateTime, Duration, Utc};
use ploy_operator_contracts::{
    ActiveAlert, AlertKind, AlertSeverity, HeartbeatState, HeartbeatStatus, PlatformMetrics,
    SystemControlResponse, SystemStatus,
};
use std::{collections::BTreeMap, fs};

const MAX_ERROR_TIMESTAMPS: usize = 3_600;

#[derive(Debug, Clone, Copy, Default)]
struct HostMetrics {
    cpu_pressure_milli_percent: Option<u32>,
    load_average_1m_milli: Option<u32>,
    process_memory_mb: Option<u64>,
    memory_available_mb: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePhase {
    Starting,
    Running,
    Degraded,
    Recovering,
}

impl RuntimePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Degraded => "degraded",
            Self::Recovering => "recovering",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemService {
    booted_at: DateTime<Utc>,
    phase: RuntimePhase,
    listen_addr: Option<String>,
    last_trade_time: Option<DateTime<Utc>>,
    websocket_connected: bool,
    database_connected: bool,
    error_timestamps: Vec<DateTime<Utc>>,
    live_reconcile_failures: u32,
    next_live_reconcile_at: Option<DateTime<Utc>>,
    last_live_reconcile_error: Option<String>,
    last_live_reconcile_success_at: Option<DateTime<Utc>>,
    sources: BTreeMap<String, SourceStatus>,
}

#[derive(Debug, Clone)]
struct SourceStatus {
    source_kind: String,
    last_seen_at: Option<DateTime<Utc>>,
    stale_after: Duration,
    forced_stale: bool,
    message: Option<String>,
    triggered_at: Option<DateTime<Utc>>,
}

impl Default for SystemService {
    fn default() -> Self {
        Self {
            booted_at: Utc::now(),
            phase: RuntimePhase::Starting,
            listen_addr: None,
            last_trade_time: None,
            websocket_connected: false,
            database_connected: false,
            error_timestamps: Vec::new(),
            live_reconcile_failures: 0,
            next_live_reconcile_at: None,
            last_live_reconcile_error: None,
            last_live_reconcile_success_at: None,
            sources: BTreeMap::new(),
        }
    }
}

impl SystemService {
    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        let (phase, listen_addr) = status
            .split_once('@')
            .map_or((status.as_str(), None), |(phase, listen_addr)| {
                (phase, Some(listen_addr.to_string()))
            });
        self.phase = match phase {
            "running" => RuntimePhase::Running,
            "degraded" => RuntimePhase::Degraded,
            "recovering" => RuntimePhase::Recovering,
            _ => RuntimePhase::Starting,
        };
        self.listen_addr = listen_addr;
    }

    pub fn mark_running(&mut self, listen_addr: &str) {
        self.listen_addr = Some(listen_addr.to_string());
        self.phase = RuntimePhase::Running;
    }

    pub fn mark_recovering(&mut self, listen_addr: &str) {
        self.listen_addr = Some(listen_addr.to_string());
        self.phase = RuntimePhase::Recovering;
    }

    pub fn mark_degraded(&mut self, listen_addr: &str) {
        self.listen_addr = Some(listen_addr.to_string());
        if self.phase != RuntimePhase::Degraded {
            self.prune_error_timestamps(Utc::now());
            self.error_timestamps.push(Utc::now());
        }
        self.phase = RuntimePhase::Degraded;
    }

    pub fn is_degraded(&self) -> bool {
        self.phase == RuntimePhase::Degraded
    }

    pub fn set_websocket_connected(&mut self, connected: bool) {
        self.websocket_connected = connected;
    }

    pub fn set_database_connected(&mut self, connected: bool) {
        self.database_connected = connected;
    }

    pub fn note_trade(&mut self, last_trade_time: Option<DateTime<Utc>>) {
        if let Some(last_trade_time) = last_trade_time {
            self.last_trade_time = Some(
                self.last_trade_time
                    .map(|current| current.max(last_trade_time))
                    .unwrap_or(last_trade_time),
            );
        }
    }

    pub fn note_live_reconcile_failure(
        &mut self,
        failures: u32,
        next_live_reconcile_at: DateTime<Utc>,
        error: String,
    ) {
        self.live_reconcile_failures = failures;
        self.next_live_reconcile_at = Some(next_live_reconcile_at);
        self.last_live_reconcile_error = Some(error);
    }

    pub fn note_live_reconcile_healthy(&mut self) {
        self.live_reconcile_failures = 0;
        self.next_live_reconcile_at = None;
        self.last_live_reconcile_error = None;
        self.last_live_reconcile_success_at = Some(Utc::now());
    }

    pub fn note_source_heartbeat(
        &mut self,
        source_id: impl Into<String>,
        source_kind: impl Into<String>,
        stale_after: Duration,
    ) {
        let now = Utc::now();
        let source_kind = source_kind.into();
        let entry = self
            .sources
            .entry(source_id.into())
            .or_insert_with(|| SourceStatus {
                source_kind: source_kind.clone(),
                last_seen_at: None,
                stale_after,
                forced_stale: false,
                message: None,
                triggered_at: None,
            });
        entry.source_kind = source_kind;
        entry.last_seen_at = Some(now);
        entry.stale_after = stale_after;
        entry.forced_stale = false;
        entry.message = None;
        entry.triggered_at = None;
    }

    pub fn note_source_failure(
        &mut self,
        source_id: impl Into<String>,
        source_kind: impl Into<String>,
        stale_after: Duration,
        message: String,
    ) {
        let now = Utc::now();
        let source_kind = source_kind.into();
        let entry = self
            .sources
            .entry(source_id.into())
            .or_insert_with(|| SourceStatus {
                source_kind: source_kind.clone(),
                last_seen_at: None,
                stale_after,
                forced_stale: false,
                message: None,
                triggered_at: None,
            });
        entry.source_kind = source_kind;
        entry.last_seen_at = Some(now);
        entry.stale_after = stale_after;
        entry.forced_stale = true;
        entry.message = Some(message);
        if entry.triggered_at.is_none() {
            entry.triggered_at = Some(now);
        }
    }

    pub fn clear_source(&mut self, source_id: &str) {
        self.sources.remove(source_id);
    }

    pub fn refresh_source_health(&mut self) -> usize {
        let now = Utc::now();
        let mut stale_count = 0;
        for source in self.sources.values_mut() {
            let is_stale = source.forced_stale
                || source
                    .last_seen_at
                    .map(|last_seen_at| now - last_seen_at > source.stale_after)
                    .unwrap_or(true);
            if is_stale {
                stale_count += 1;
                if source.triggered_at.is_none() {
                    source.triggered_at = Some(now);
                }
            } else {
                source.triggered_at = None;
            }
        }
        stale_count
    }

    pub fn stale_source_count(&self) -> usize {
        self.sources
            .values()
            .filter(|source| source.triggered_at.is_some())
            .count()
    }

    pub fn heartbeat_statuses(&self) -> Vec<HeartbeatStatus> {
        self.sources
            .iter()
            .map(|(source_id, source)| HeartbeatStatus {
                source_id: source_id.clone(),
                source_kind: source.source_kind.clone(),
                state: if source.triggered_at.is_some() {
                    HeartbeatState::Stale
                } else {
                    HeartbeatState::Healthy
                },
                last_seen_at: source.last_seen_at,
                stale_after_seconds: source.stale_after.num_seconds(),
                message: source.message.clone(),
            })
            .collect()
    }

    pub fn active_alerts(&self) -> Vec<ActiveAlert> {
        self.sources
            .iter()
            .filter_map(|(source_id, source)| {
                let triggered_at = source.triggered_at?;
                Some(ActiveAlert {
                    alert_id: format!("{source_id}:stale"),
                    kind: AlertKind::SourceStale,
                    severity: alert_severity(&source.source_kind),
                    source_id: source_id.clone(),
                    message: source
                        .message
                        .clone()
                        .unwrap_or_else(|| format!("{} source is stale", source.source_kind)),
                    triggered_at,
                })
            })
            .collect()
    }

    pub fn source_is_stale(&self, source_id: &str) -> bool {
        self.sources
            .get(source_id)
            .map(|source| source.triggered_at.is_some())
            .unwrap_or(false)
    }

    pub fn source_is_fresh_at(&self, source_id: &str, now: DateTime<Utc>) -> bool {
        self.sources.get(source_id).is_some_and(|source| {
            !source.forced_stale
                && source.last_seen_at.is_some_and(|last_seen_at| {
                    last_seen_at <= now && now - last_seen_at <= source.stale_after
                })
        })
    }

    pub fn metrics(
        &self,
        total_deployments: usize,
        live_deployments: usize,
        degraded_deployments: usize,
    ) -> PlatformMetrics {
        let host_metrics = collect_host_metrics();
        PlatformMetrics {
            total_deployments,
            live_deployments,
            degraded_deployments,
            active_alerts: self.active_alerts().len(),
            stale_sources: self.stale_source_count(),
            live_reconcile_failures: self.live_reconcile_failures,
            host_cpu_pressure_milli_percent: host_metrics.cpu_pressure_milli_percent,
            host_load_average_1m_milli: host_metrics.load_average_1m_milli,
            process_memory_mb: host_metrics.process_memory_mb,
            host_memory_available_mb: host_metrics.memory_available_mb,
            last_trade_time: self.last_trade_time,
            last_live_reconcile_success_at: self.last_live_reconcile_success_at,
            heartbeats: self.heartbeat_statuses(),
        }
    }

    pub fn status(&self) -> SystemStatus {
        let cutoff = Utc::now() - Duration::hours(1);
        SystemStatus {
            status: match &self.listen_addr {
                Some(listen_addr) => format!("{}@{listen_addr}", self.phase.as_str()),
                None => self.phase.as_str().to_string(),
            },
            uptime_seconds: (Utc::now() - self.booted_at).num_seconds(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            strategy: "platform".to_string(),
            last_trade_time: self.last_trade_time,
            websocket_connected: self.websocket_connected,
            database_connected: self.database_connected,
            error_count_1h: self
                .error_timestamps
                .iter()
                .filter(|timestamp| **timestamp >= cutoff)
                .count() as i64,
            live_reconcile_failures: self.live_reconcile_failures,
            next_live_reconcile_at: self.next_live_reconcile_at,
            last_live_reconcile_error: self.last_live_reconcile_error.clone(),
            active_alert_count: self.active_alerts().len(),
            stale_source_count: self.stale_source_count(),
            last_live_reconcile_success_at: self.last_live_reconcile_success_at,
        }
    }

    fn prune_error_timestamps(&mut self, now: DateTime<Utc>) {
        let cutoff = now - Duration::hours(1);
        self.error_timestamps
            .retain(|timestamp| *timestamp >= cutoff);
        if self.error_timestamps.len() > MAX_ERROR_TIMESTAMPS {
            let excess = self.error_timestamps.len() - MAX_ERROR_TIMESTAMPS;
            self.error_timestamps.drain(0..excess);
        }
    }

    pub fn control_response(&self, action: &str) -> SystemControlResponse {
        SystemControlResponse {
            success: true,
            message: format!("system {action} accepted"),
        }
    }
}

fn collect_host_metrics() -> HostMetrics {
    let load_average_1m = read_load_average_1m();
    let cpu_pressure_milli_percent = load_average_1m.and_then(|load| {
        let cpus = std::thread::available_parallelism().ok()?.get() as f64;
        if cpus <= 0.0 {
            return None;
        }
        Some((((load / cpus) * 100_000.0).clamp(0.0, 999_000.0)).round() as u32)
    });

    HostMetrics {
        cpu_pressure_milli_percent,
        load_average_1m_milli: load_average_1m.map(|value| (value * 1000.0).round() as u32),
        process_memory_mb: read_status_kb("/proc/self/status", "VmRSS:").map(kib_to_mb),
        memory_available_mb: read_meminfo_kb("MemAvailable:").map(kib_to_mb),
    }
}

fn read_load_average_1m() -> Option<f64> {
    let content = fs::read_to_string("/proc/loadavg").ok()?;
    content.split_whitespace().next()?.parse::<f64>().ok()
}

fn read_status_kb(path: &str, key: &str) -> Option<u64> {
    let content = fs::read_to_string(path).ok()?;
    parse_kb_line(&content, key)
}

fn read_meminfo_kb(key: &str) -> Option<u64> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    parse_kb_line(&content, key)
}

fn parse_kb_line(content: &str, key: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with(key) {
            return None;
        }
        line[key.len()..]
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })
}

fn kib_to_mb(kib: u64) -> u64 {
    kib / 1024
}

fn alert_severity(source_kind: &str) -> AlertSeverity {
    match source_kind {
        "worker" | "live_reconcile" | "venue" => AlertSeverity::Critical,
        _ => AlertSeverity::Warning,
    }
}

#[cfg(test)]
mod tests {
    use super::SystemService;
    use chrono::{Duration, Utc};

    #[test]
    fn system_service_transitions_through_degraded_and_recovering() {
        let mut service = SystemService::default();
        assert_eq!(service.status().status, "starting");

        service.mark_running("127.0.0.1:8081");
        assert_eq!(service.status().status, "running@127.0.0.1:8081");

        service.mark_degraded("127.0.0.1:8081");
        service.note_live_reconcile_failure(
            2,
            Utc::now() + Duration::seconds(5),
            "gateway offline".to_string(),
        );
        let degraded = service.status();
        assert_eq!(degraded.status, "degraded@127.0.0.1:8081");
        assert_eq!(degraded.error_count_1h, 1);
        assert_eq!(degraded.live_reconcile_failures, 2);
        assert!(degraded.next_live_reconcile_at.is_some());
        assert_eq!(
            degraded.last_live_reconcile_error.as_deref(),
            Some("gateway offline")
        );
        assert!(service.is_degraded());

        service.mark_recovering("127.0.0.1:8081");
        assert_eq!(service.status().status, "recovering@127.0.0.1:8081");

        service.note_live_reconcile_healthy();
        service.mark_running("127.0.0.1:8081");
        assert_eq!(service.status().status, "running@127.0.0.1:8081");
        assert_eq!(service.status().error_count_1h, 1);
        assert_eq!(service.status().live_reconcile_failures, 0);
    }

    #[test]
    fn system_service_tracks_trade_time_and_connectivity() {
        let mut service = SystemService::default();
        let earlier = Utc::now() - Duration::minutes(5);
        let later = Utc::now();

        service.note_trade(Some(earlier));
        service.note_trade(Some(later));
        service.set_database_connected(true);
        service.set_websocket_connected(true);

        let status = service.status();
        assert_eq!(status.last_trade_time, Some(later));
        assert!(status.database_connected);
        assert!(status.websocket_connected);
        assert_eq!(status.live_reconcile_failures, 0);
    }

    #[test]
    fn set_status_restores_phase() {
        let mut service = SystemService::default();
        service.set_status("degraded@127.0.0.1:8081");
        assert_eq!(service.status().status, "degraded@127.0.0.1:8081");

        service.set_status("running");
        assert_eq!(service.status().status, "running");
    }

    #[test]
    fn system_service_surfaces_stale_sources_in_metrics_and_alerts() {
        let mut service = SystemService::default();
        service.mark_running("127.0.0.1:8081");
        service.note_source_failure(
            "venue:polymarket",
            "venue",
            Duration::seconds(15),
            "gateway offline".to_string(),
        );
        service.refresh_source_health();

        let status = service.status();
        assert_eq!(status.active_alert_count, 1);
        assert_eq!(status.stale_source_count, 1);

        let metrics = service.metrics(2, 1, 1);
        assert_eq!(metrics.total_deployments, 2);
        assert_eq!(metrics.stale_sources, 1);
        assert_eq!(metrics.active_alerts, 1);
        assert_eq!(metrics.heartbeats.len(), 1);
        assert_eq!(metrics.heartbeats[0].source_id, "venue:polymarket");

        let alerts = service.active_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].source_id, "venue:polymarket");
    }

    #[test]
    fn absent_venue_source_is_not_fresh() {
        let service = SystemService::default();

        assert!(!service.source_is_fresh_at("venue:polymarket", Utc::now()));
    }

    #[test]
    fn stale_venue_source_is_not_fresh_without_refresh_side_effect() {
        let mut service = SystemService::default();
        service.note_source_heartbeat("venue:polymarket", "venue", Duration::seconds(15));

        assert!(
            !service.source_is_fresh_at("venue:polymarket", Utc::now() + Duration::seconds(16),)
        );
        assert_eq!(service.stale_source_count(), 0);
    }

    #[test]
    fn future_venue_source_is_not_fresh() {
        let now = Utc::now();
        let mut service = SystemService::default();
        service.note_source_heartbeat("venue:polymarket", "venue", Duration::seconds(15));
        service
            .sources
            .get_mut("venue:polymarket")
            .expect("venue source")
            .last_seen_at = Some(now + Duration::seconds(1));

        assert!(!service.source_is_fresh_at("venue:polymarket", now));
    }
}
