use alpha_domain::{
    verify_runtime_attribution_event, AttributionKind, AttributionMode, AttributionOutcome,
    SignedRuntimeAttributionEvent,
};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

const MAX_FEEDBACK_LINE_BYTES: usize = 1024 * 1024;
const MAX_SELECTED_EVIDENCE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedRuntimeLatencyEvidence {
    pub account_id: String,
    pub signed_events: Vec<u8>,
    pub first_observed_at: DateTime<Utc>,
    pub last_observed_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
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
    market: &str,
    symbol: &str,
    account_id: &str,
    available_before: DateTime<Utc>,
) -> Result<VerifiedRuntimeLatencyEvidence> {
    let expected_feedback_sha256 = expected_sha256(feedback_log_sha256, "feedback log")?;
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
        || !matches!(market, "spot" | "usdm")
        || symbol.trim().is_empty()
        || account_id.trim().is_empty()
    {
        bail!("runtime feedback deployment, market, symbol, and account are required");
    }
    if market == "usdm" {
        bail!("USD-M runtime latency evidence is unavailable without a derivatives execution path");
    }

    let mut observations = Vec::new();
    let mut signed_events = Vec::new();
    let mut event_digests = BTreeMap::new();
    let mut feedback =
        BufReader::new(File::open(feedback_log).context("failed to open runtime feedback log")?);
    let mut feedback_hasher = Sha256::new();
    let mut line = Vec::new();
    let mut line_number = 0_usize;
    loop {
        line.clear();
        let mut limited = (&mut feedback).take((MAX_FEEDBACK_LINE_BYTES + 1) as u64);
        let bytes_read = limited
            .read_until(b'\n', &mut line)
            .context("failed to read runtime feedback log")?;
        if bytes_read == 0 {
            break;
        }
        line_number += 1;
        if bytes_read > MAX_FEEDBACK_LINE_BYTES {
            bail!("runtime feedback line {line_number} exceeds the size limit");
        }
        feedback_hasher.update(&line);
        let record = line.strip_suffix(b"\n").unwrap_or(&line);
        let record = record.strip_suffix(b"\r").unwrap_or(record);
        if record.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let signed: SignedRuntimeAttributionEvent = serde_json::from_slice(record)
            .with_context(|| format!("runtime feedback line {line_number} is invalid JSON"))?;
        let event =
            verify_runtime_attribution_event(&signed, &trusted_keys).with_context(|| {
                format!("runtime feedback line {line_number} failed signature verification")
            })?;
        if event.mode != AttributionMode::LiveSmall
            || event.kind != AttributionKind::Fill
            || event.outcome != AttributionOutcome::Healthy
            || event.deployment_id != deployment_id
            || event.account_id.as_deref() != Some(account_id)
            || event
                .metrics
                .get(&format!("instrument_market_{market}"))
                .copied()
                != Some(1.0)
            || !event.venue.as_deref().is_some_and(|value| {
                value.eq_ignore_ascii_case("binance") || value.eq_ignore_ascii_case("binance_spot")
            })
            || !event
                .symbol
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(symbol))
        {
            continue;
        }
        let available_at_us = metric_u64(&event.metrics, "evidence_available_at_us")?;
        let available_at = DateTime::from_timestamp_micros(
            i64::try_from(available_at_us).context("evidence availability exceeds i64")?,
        )
        .context("runtime feedback evidence availability is invalid")?;
        if event.observed_at > available_at {
            bail!("runtime feedback observation is after its evidence availability");
        }
        if available_at > available_before || event.observed_at > available_before {
            continue;
        }
        let Some(arrival_slippage_bps) = event.metrics.get("arrival_slippage_bps").copied() else {
            continue;
        };
        if !arrival_slippage_bps.is_finite() || arrival_slippage_bps < 0.0 {
            bail!("runtime feedback arrival slippage is invalid");
        }
        let canonical = serde_json::to_vec(&signed)?;
        if let Some(previous) = event_digests.insert(event.event_id.clone(), canonical.clone()) {
            if previous != canonical {
                bail!("runtime feedback contains conflicting duplicate event IDs");
            }
            continue;
        }
        observations.push((
            event.observed_at,
            available_at,
            metric_u64(&event.metrics, "intent_to_private_report_us")?,
            arrival_slippage_bps,
        ));
        let selected_bytes = signed_events
            .len()
            .checked_add(record.len() + 1)
            .context("selected runtime evidence size overflows")?;
        if selected_bytes > MAX_SELECTED_EVIDENCE_BYTES {
            bail!("selected runtime evidence exceeds the size limit");
        }
        signed_events.extend_from_slice(record);
        signed_events.push(b'\n');
    }
    if hex::encode(feedback_hasher.finalize()) != expected_feedback_sha256 {
        bail!("feedback log bytes do not match the trusted digest anchor");
    }
    if observations.is_empty() {
        bail!("no verified pre-snapshot live order lifecycle observations match");
    }
    observations.sort_by_key(|(observed_at, _, _, _)| *observed_at);
    let mut latencies = observations
        .iter()
        .map(|(_, _, latency_us, _)| {
            latency_us
                .checked_mul(1_000)
                .context("runtime feedback latency overflows nanoseconds")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut costs = observations
        .iter()
        .map(|(_, _, _, slippage_bps)| *slippage_bps)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    costs.sort_by(f64::total_cmp);
    Ok(VerifiedRuntimeLatencyEvidence {
        account_id: account_id.to_string(),
        signed_events,
        first_observed_at: observations.first().unwrap().0,
        last_observed_at: observations.last().unwrap().0,
        available_at: observations
            .iter()
            .map(|(_, available_at, _, _)| *available_at)
            .max()
            .unwrap(),
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
    let expected = expected_sha256(expected, label)?;
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {label}"))?;
    if hex::encode(Sha256::digest(&bytes)) != expected {
        bail!("{label} bytes do not match the trusted digest anchor");
    }
    Ok(bytes)
}

fn expected_sha256(expected: &str, label: &str) -> Result<String> {
    let expected = expected.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA-256 is invalid");
    }
    Ok(expected)
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
            strategy_id: Some("strategy-1".to_string()),
            order_id: Some("order-1".to_string()),
            account_id: Some("binance-main".to_string()),
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            metrics: BTreeMap::from([
                ("intent_to_private_report_us".to_string(), 75.0),
                ("arrival_slippage_bps".to_string(), 1.25),
                (
                    "evidence_available_at_us".to_string(),
                    1_783_987_200_000_000.0,
                ),
                ("instrument_market_spot".to_string(), 1.0),
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
            "spot",
            "BTCUSDT",
            "binance-main",
            DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap();

        assert_eq!(evidence.observations, 1);
        assert_eq!(evidence.account_id, "binance-main");
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
            "spot",
            "BTCUSDT",
            "binance-main",
            DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .is_err());
    }

    #[test]
    fn rejects_usdm_without_a_derivatives_execution_path() {
        let (_directory, log, log_sha, keys, keys_sha) = write_fixture(AttributionMode::LiveSmall);
        assert!(verify_runtime_latency_evidence(
            &log,
            &log_sha,
            &keys,
            &keys_sha,
            "deployment-1",
            "usdm",
            "BTCUSDT",
            "binance-main",
            DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .is_err());
    }

    #[test]
    fn duplicate_signed_events_do_not_reweight_costs() {
        let (_directory, log, _log_sha, keys, keys_sha) = write_fixture(AttributionMode::LiveSmall);
        let line = std::fs::read(&log).unwrap();
        let signed: SignedRuntimeAttributionEvent = serde_json::from_slice(&line).unwrap();
        let value = serde_json::to_value(&signed).unwrap();
        let mut alternate = serde_json::to_vec(&value).unwrap();
        alternate.push(b'\n');
        let bytes = [line.as_slice(), alternate.as_slice()].concat();
        std::fs::write(&log, &bytes).unwrap();
        let evidence = verify_runtime_latency_evidence(
            &log,
            &hex::encode(Sha256::digest(&bytes)),
            &keys,
            &keys_sha,
            "deployment-1",
            "spot",
            "BTCUSDT",
            "binance-main",
            DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap();

        assert_eq!(evidence.observations, 1);
    }

    #[test]
    fn rejects_observation_after_claimed_availability() {
        let (_directory, log, _log_sha, keys, keys_sha) = write_fixture(AttributionMode::LiveSmall);
        let signed: SignedRuntimeAttributionEvent =
            serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
        let mut event = signed.event;
        event.observed_at = DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let signed =
            sign_runtime_attribution_event(event, "key-1", &SigningKey::from_bytes(&[7_u8; 32]))
                .unwrap();
        let mut bytes = serde_json::to_vec(&signed).unwrap();
        bytes.push(b'\n');
        std::fs::write(&log, &bytes).unwrap();

        assert!(verify_runtime_latency_evidence(
            &log,
            &hex::encode(Sha256::digest(&bytes)),
            &keys,
            &keys_sha,
            "deployment-1",
            "spot",
            "BTCUSDT",
            "binance-main",
            DateTime::parse_from_rfc3339("2026-07-14T00:00:02Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .is_err());
    }
}
