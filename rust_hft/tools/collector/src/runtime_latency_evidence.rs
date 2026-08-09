use alpha_domain::{
    verify_runtime_attribution_event, AttributionKind, AttributionMode, AttributionOutcome,
    SignedRuntimeAttributionEvent,
};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedRuntimeLatencyEvidence {
    pub signed_events: Vec<u8>,
    pub first_observed_at: DateTime<Utc>,
    pub last_observed_at: DateTime<Utc>,
    pub observations: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub p50_cost_bps: String,
    pub p95_cost_bps: String,
    pub p99_cost_bps: String,
}

pub fn verify_runtime_latency_evidence(
    feedback_log: &Path,
    feedback_log_sha256: &str,
    trusted_keys_path: &Path,
    trusted_keys_sha256: &str,
    deployment_id: &str,
    symbol: &str,
    account_fingerprint: &str,
    available_before: DateTime<Utc>,
) -> Result<VerifiedRuntimeLatencyEvidence> {
    let feedback_bytes = read_sha256_anchored(feedback_log, feedback_log_sha256, "feedback log")?;
    let key_bytes = read_sha256_anchored(
        trusted_keys_path,
        trusted_keys_sha256,
        "feedback trusted keys",
    )?;
    let encoded: BTreeMap<String, String> = serde_json::from_slice(&key_bytes)
        .context("runtime feedback trusted keys are invalid JSON")?;
    let trusted_keys = encoded
        .into_iter()
        .map(|(key_id, value)| {
            let bytes = hex::decode(value)
                .with_context(|| format!("runtime feedback key {key_id} is not hex"))?;
            let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                anyhow::anyhow!("runtime feedback key {key_id} must contain exactly 32 bytes")
            })?;
            Ok((
                key_id.clone(),
                VerifyingKey::from_bytes(&bytes)
                    .with_context(|| format!("runtime feedback key {key_id} is invalid"))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    if deployment_id.trim().is_empty()
        || symbol.trim().is_empty()
        || account_fingerprint.len() != 64
        || !account_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("runtime feedback deployment, symbol, and account fingerprint are required");
    }

    let mut observations = Vec::new();
    let mut signed_events = Vec::new();
    for (index, line) in feedback_bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let signed: SignedRuntimeAttributionEvent = serde_json::from_slice(line)
            .with_context(|| format!("runtime feedback line {} is invalid JSON", index + 1))?;
        let event =
            verify_runtime_attribution_event(&signed, &trusted_keys).with_context(|| {
                format!(
                    "runtime feedback line {} failed signature verification",
                    index + 1
                )
            })?;
        if event.mode != AttributionMode::LiveSmall
            || event.kind != AttributionKind::Fill
            || event.outcome != AttributionOutcome::Healthy
            || event.deployment_id != deployment_id
            || event.account_id.as_deref() != Some(account_fingerprint)
            || event.venue.as_deref() != Some("binance")
            || !event
                .symbol
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(symbol))
            || event.observed_at > available_before
        {
            continue;
        }
        observations.push((
            event.observed_at,
            metric_u64(&event.metrics, "intent_to_private_report_us")?,
            metric_nonnegative(&event.metrics, "realized_slippage_bps")?,
        ));
        signed_events.extend_from_slice(line);
        signed_events.push(b'\n');
    }
    if observations.is_empty() {
        bail!("no verified pre-snapshot live order lifecycle observations match");
    }
    observations.sort_by_key(|(observed_at, _, _)| *observed_at);
    let mut latencies = observations
        .iter()
        .map(|(_, latency_us, _)| latency_us.saturating_mul(1_000))
        .collect::<Vec<_>>();
    let mut costs = observations
        .iter()
        .map(|(_, _, slippage_bps)| *slippage_bps)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    costs.sort_by(f64::total_cmp);
    Ok(VerifiedRuntimeLatencyEvidence {
        signed_events,
        first_observed_at: observations.first().unwrap().0,
        last_observed_at: observations.last().unwrap().0,
        observations: observations.len() as u64,
        p50_ns: percentile(&latencies, 50),
        p95_ns: percentile(&latencies, 95),
        p99_ns: percentile(&latencies, 99),
        p50_cost_bps: percentile(&costs, 50).to_string(),
        p95_cost_bps: percentile(&costs, 95).to_string(),
        p99_cost_bps: percentile(&costs, 99).to_string(),
    })
}

fn read_sha256_anchored(path: &Path, expected: &str, label: &str) -> Result<Vec<u8>> {
    let expected = expected.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA-256 is invalid");
    }
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {label}"))?;
    if hex::encode(Sha256::digest(&bytes)) != expected {
        bail!("{label} bytes do not match the trusted digest anchor");
    }
    Ok(bytes)
}

fn metric_u64(metrics: &BTreeMap<String, f64>, name: &str) -> Result<u64> {
    let value = metric_nonnegative(metrics, name)?;
    if value.fract() != 0.0 || value > u64::MAX as f64 {
        bail!("runtime feedback metric {name} is not an unsigned integer");
    }
    Ok(value as u64)
}

fn metric_nonnegative(metrics: &BTreeMap<String, f64>, name: &str) -> Result<f64> {
    let value = *metrics
        .get(name)
        .with_context(|| format!("runtime feedback fill is missing {name}"))?;
    if !value.is_finite() || value < 0.0 {
        bail!("runtime feedback metric {name} is invalid");
    }
    Ok(value)
}

fn percentile<T: Copy>(sorted: &[T], percentile: usize) -> T {
    sorted[((sorted.len() - 1) * percentile).div_ceil(100)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{
        sign_runtime_attribution_event, RuntimeAttributionEvent, SignedRuntimeAttributionEvent,
    };
    use ed25519_dalek::SigningKey;

    fn write_fixture(
        mode: AttributionMode,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        String,
        std::path::PathBuf,
        String,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let key_path = directory.path().join("keys.json");
        std::fs::write(
            &key_path,
            serde_json::to_vec(&BTreeMap::from([(
                "key-1".to_string(),
                hex::encode(signing_key.verifying_key().as_bytes()),
            )]))
            .unwrap(),
        )
        .unwrap();
        let event = RuntimeAttributionEvent {
            event_id: "fill-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::Fill,
            strategy_id: None,
            order_id: Some("order-1".to_string()),
            account_id: Some("a".repeat(64)),
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            metrics: BTreeMap::from([
                ("intent_to_private_report_us".to_string(), 75.0),
                ("realized_slippage_bps".to_string(), 1.25),
            ]),
            reason: None,
            observed_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let signed: SignedRuntimeAttributionEvent =
            sign_runtime_attribution_event(event, "key-1", &signing_key).unwrap();
        let log_path = directory.path().join("feedback.jsonl");
        let mut bytes = serde_json::to_vec(&signed).unwrap();
        bytes.push(b'\n');
        std::fs::write(&log_path, &bytes).unwrap();
        let log_sha = hex::encode(Sha256::digest(&bytes));
        let key_sha = hex::encode(Sha256::digest(std::fs::read(&key_path).unwrap()));
        (directory, log_path, log_sha, key_path, key_sha)
    }

    #[test]
    fn accepts_only_signed_live_lifecycle_costs() {
        let (_directory, log, log_sha, keys, keys_sha) = write_fixture(AttributionMode::LiveSmall);
        let evidence = verify_runtime_latency_evidence(
            &log,
            &log_sha,
            &keys,
            &keys_sha,
            "deployment-1",
            "BTCUSDT",
            &"a".repeat(64),
            DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap();

        assert_eq!(evidence.observations, 1);
        assert_eq!(evidence.p99_ns, 75_000);
        assert_eq!(evidence.p95_cost_bps, "1.25");
    }

    #[test]
    fn rejects_shadow_costs_as_not_real() {
        let (_directory, log, log_sha, keys, keys_sha) = write_fixture(AttributionMode::Shadow);
        assert!(verify_runtime_latency_evidence(
            &log,
            &log_sha,
            &keys,
            &keys_sha,
            "deployment-1",
            "BTCUSDT",
            &"a".repeat(64),
            DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .is_err());
    }
}
