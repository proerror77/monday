use secrecy::SecretString;
use std::path::{Path, PathBuf};

fn select_admin_token(
    admin_token: Option<String>,
    api_admin_token: Option<String>,
    api_key: Option<String>,
) -> Option<SecretString> {
    [admin_token, api_admin_token, api_key]
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
        .map(SecretString::from)
}

#[derive(Debug, Clone)]
pub struct PlatformConfig {
    pub listen_addr: String,
    pub admin_token: Option<SecretString>,
    pub operator_token: Option<SecretString>,
    pub worker_token: Option<SecretString>,
    pub sidecar_token: Option<SecretString>,
    pub auth_cookie_secret: SecretString,
    pub registry_file: PathBuf,
    pub runner_binary: PathBuf,
    pub strategy_config_root: PathBuf,
    pub runtime_root: PathBuf,
    pub status_file: PathBuf,
    pub deployment_status_file: PathBuf,
    pub trading_state_file: PathBuf,
    pub audit_log_file: PathBuf,
    pub agent_runs_file: PathBuf,
    pub proposals_file: PathBuf,
    pub release_sha: Option<String>,
    pub live_approval_file: Option<PathBuf>,
    pub tick_interval_ms: u64,
    pub request_rate_limit_per_minute: u32,
    pub live_reconcile_backoff_base_ms: u64,
    pub live_reconcile_backoff_max_ms: u64,
    pub worker_heartbeat_stale_after_ms: u64,
    pub live_reconcile_stale_after_ms: u64,
    pub venue_stale_after_ms: u64,
    pub circuit_breaker_enabled: bool,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        let runtime_root = PathBuf::from("run/platform");
        Self {
            listen_addr: "127.0.0.1:8081".to_string(),
            admin_token: None,
            operator_token: None,
            worker_token: None,
            sidecar_token: None,
            auth_cookie_secret: generate_cookie_secret(),
            registry_file: PathBuf::from("data/state/deployments.json"),
            runner_binary: PathBuf::from("bin/ploy-runner"),
            strategy_config_root: PathBuf::from("config/strategies"),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            audit_log_file: runtime_root.join("audit-log.jsonl"),
            agent_runs_file: PathBuf::from("run/sidecar/agent-runs.jsonl"),
            proposals_file: runtime_root.join("proposals.json"),
            release_sha: None,
            live_approval_file: None,
            runtime_root,
            tick_interval_ms: 1_000,
            request_rate_limit_per_minute: 240,
            live_reconcile_backoff_base_ms: 1_000,
            live_reconcile_backoff_max_ms: 30_000,
            worker_heartbeat_stale_after_ms: 15_000,
            live_reconcile_stale_after_ms: 15_000,
            venue_stale_after_ms: 15_000,
            circuit_breaker_enabled: false,
        }
    }
}

impl PlatformConfig {
    fn path_uses_default(path: &Path, default: &str) -> bool {
        path == Path::new(default)
    }

    pub fn normalize_derived_paths(&mut self) {
        if Self::path_uses_default(&self.status_file, "run/platform/system-status.json") {
            self.status_file = self.runtime_root.join("system-status.json");
        }
        if Self::path_uses_default(
            &self.deployment_status_file,
            "run/platform/deployments.json",
        ) {
            self.deployment_status_file = self.runtime_root.join("deployments.json");
        }
        if Self::path_uses_default(&self.trading_state_file, "run/platform/trading-state.json") {
            self.trading_state_file = self.runtime_root.join("trading-state.json");
        }
        if Self::path_uses_default(&self.audit_log_file, "run/platform/audit-log.jsonl") {
            self.audit_log_file = self.runtime_root.join("audit-log.jsonl");
        }
        if Self::path_uses_default(&self.proposals_file, "run/platform/proposals.json") {
            self.proposals_file = self.runtime_root.join("proposals.json");
        }
        if Self::path_uses_default(&self.agent_runs_file, "run/sidecar/agent-runs.jsonl") {
            let parent = self
                .runtime_root
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("run"));
            self.agent_runs_file = parent.join("sidecar/agent-runs.jsonl");
        }
    }

    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(value) = std::env::var("PLOY_LISTEN_ADDR") {
            config.listen_addr = value;
        }
        config.admin_token = select_admin_token(
            std::env::var("PLOY_ADMIN_TOKEN").ok(),
            std::env::var("PLOY_API_ADMIN_TOKEN").ok(),
            std::env::var("PLOY_API_KEY").ok(),
        );
        if let Ok(value) = std::env::var("PLOY_OPERATOR_TOKEN") {
            if !value.trim().is_empty() {
                config.operator_token = Some(SecretString::from(value));
            }
        } else if let Ok(value) = std::env::var("PLOY_API_OPERATOR_TOKEN") {
            if !value.trim().is_empty() {
                config.operator_token = Some(SecretString::from(value));
            }
        }
        if let Ok(value) = std::env::var("PLOY_WORKER_TOKEN") {
            if !value.trim().is_empty() {
                config.worker_token = Some(SecretString::from(value));
            }
        }
        if let Ok(value) = std::env::var("PLOY_SIDECAR_AUTH_TOKEN") {
            if !value.trim().is_empty() {
                config.sidecar_token = Some(SecretString::from(value));
            }
        }
        if let Ok(value) = std::env::var("PLOY_API_AUTH_COOKIE_SECRET") {
            if !value.trim().is_empty() {
                config.auth_cookie_secret = SecretString::from(value);
            }
        }
        if let Ok(value) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
            config.registry_file = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_RUNNER_BINARY") {
            config.runner_binary = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_STRATEGY_CONFIG_ROOT") {
            config.strategy_config_root = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_RUNTIME_ROOT") {
            config.runtime_root = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_SYSTEM_STATUS_FILE") {
            config.status_file = PathBuf::from(value);
        } else {
            config.status_file = config.runtime_root.join("system-status.json");
        }
        if let Ok(value) = std::env::var("PLOY_DEPLOYMENT_STATUS_FILE") {
            config.deployment_status_file = PathBuf::from(value);
        } else {
            config.deployment_status_file = config.runtime_root.join("deployments.json");
        }
        if let Ok(value) = std::env::var("PLOY_TRADING_STATE_FILE") {
            config.trading_state_file = PathBuf::from(value);
        } else {
            config.trading_state_file = config.runtime_root.join("trading-state.json");
        }
        if let Ok(value) = std::env::var("PLOY_AUDIT_LOG_FILE") {
            config.audit_log_file = PathBuf::from(value);
        } else {
            config.audit_log_file = config.runtime_root.join("audit-log.jsonl");
        }
        if let Ok(value) = std::env::var("PLOY_AGENT_RUNS_FILE") {
            config.agent_runs_file = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("PLOY_PROPOSALS_FILE") {
            config.proposals_file = PathBuf::from(value);
        } else {
            config.proposals_file = config.runtime_root.join("proposals.json");
        }
        if let Ok(value) = std::env::var("PLOY_RELEASE_SHA") {
            if !value.trim().is_empty() {
                config.release_sha = Some(value);
            }
        }
        if let Ok(value) = std::env::var("PLOY_LIVE_APPROVAL_FILE") {
            if !value.trim().is_empty() {
                config.live_approval_file = Some(PathBuf::from(value));
            }
        }
        if let Ok(value) = std::env::var("PLOY_TICK_INTERVAL_MS") {
            if let Ok(parsed) = value.parse() {
                config.tick_interval_ms = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_REQUEST_RATE_LIMIT_PER_MINUTE") {
            if let Ok(parsed) = value.parse() {
                config.request_rate_limit_per_minute = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS") {
            if let Ok(parsed) = value.parse() {
                config.live_reconcile_backoff_base_ms = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS") {
            if let Ok(parsed) = value.parse() {
                config.live_reconcile_backoff_max_ms = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_WORKER_HEARTBEAT_STALE_AFTER_MS") {
            if let Ok(parsed) = value.parse() {
                config.worker_heartbeat_stale_after_ms = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_LIVE_RECONCILE_STALE_AFTER_MS") {
            if let Ok(parsed) = value.parse() {
                config.live_reconcile_stale_after_ms = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_VENUE_STALE_AFTER_MS") {
            if let Ok(parsed) = value.parse() {
                config.venue_stale_after_ms = parsed;
            }
        }
        if let Ok(value) = std::env::var("PLOY_CIRCUIT_BREAKER_ENABLED") {
            config.circuit_breaker_enabled =
                matches!(value.as_str(), "1" | "true" | "TRUE" | "True");
        }

        config.normalize_derived_paths();
        config
    }
}

fn generate_cookie_secret() -> SecretString {
    use rand::RngCore;

    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    SecretString::from(bytes_to_hex(&bytes))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::PlatformConfig;

    #[test]
    fn default_paths_match_workspace_contract() {
        let config = PlatformConfig::default();
        assert!(config.admin_token.is_none());
        assert!(config.operator_token.is_none());
        assert!(config.worker_token.is_none());
        assert!(config.sidecar_token.is_none());
        assert!(!config.auth_cookie_secret.expose_secret().is_empty());
        assert_eq!(
            config.registry_file.to_string_lossy(),
            "data/state/deployments.json"
        );
        assert_eq!(config.runner_binary.to_string_lossy(), "bin/ploy-runner");
        assert_eq!(
            config.strategy_config_root.to_string_lossy(),
            "config/strategies"
        );
        assert_eq!(config.runtime_root.to_string_lossy(), "run/platform");
        assert_eq!(
            config.status_file.to_string_lossy(),
            "run/platform/system-status.json"
        );
        assert_eq!(
            config.deployment_status_file.to_string_lossy(),
            "run/platform/deployments.json"
        );
        assert_eq!(
            config.trading_state_file.to_string_lossy(),
            "run/platform/trading-state.json"
        );
        assert_eq!(
            config.audit_log_file.to_string_lossy(),
            "run/platform/audit-log.jsonl"
        );
        assert_eq!(
            config.agent_runs_file.to_string_lossy(),
            "run/sidecar/agent-runs.jsonl"
        );
        assert_eq!(
            config.proposals_file.to_string_lossy(),
            "run/platform/proposals.json"
        );
        assert_eq!(config.tick_interval_ms, 1_000);
        assert_eq!(config.request_rate_limit_per_minute, 240);
        assert_eq!(config.live_reconcile_backoff_base_ms, 1_000);
        assert_eq!(config.live_reconcile_backoff_max_ms, 30_000);
        assert_eq!(config.worker_heartbeat_stale_after_ms, 15_000);
        assert_eq!(config.live_reconcile_stale_after_ms, 15_000);
        assert_eq!(config.venue_stale_after_ms, 15_000);
        assert!(!config.circuit_breaker_enabled);
    }

    #[test]
    fn ploy_api_key_is_admin_compatibility_alias() {
        let token = super::select_admin_token(None, None, Some("compat-token".to_string()))
            .expect("PLOY_API_KEY alias");
        assert_eq!(token.expose_secret(), "compat-token");

        let preferred = super::select_admin_token(
            Some("admin-token".to_string()),
            Some("api-admin-token".to_string()),
            Some("compat-token".to_string()),
        )
        .expect("preferred admin token");
        assert_eq!(preferred.expose_secret(), "admin-token");
    }
}
