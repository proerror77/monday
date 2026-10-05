//! Independently observed immutable worker configuration. Caller hashes and
//! `immutable: true` alone cannot construct this proof.
use super::super::decode_base64;
use crate::prediction_dispatch::{kubectl_binary, kubectl_json};
use anyhow::{ensure, Context};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(super) struct VerifiedWorkerConfiguration {
    reference: hft_research_platform::orchestrator::WorkerConfigurationRef,
}
impl VerifiedWorkerConfiguration {
    pub(super) fn name(&self) -> &str {
        &self.reference.secret_name
    }
    pub(super) fn reference(&self) -> &hft_research_platform::orchestrator::WorkerConfigurationRef {
        &self.reference
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactIo {
    schema_version: String,
    artifact_gateway: String,
    artifact_token_file: PathBuf,
    #[serde(default)]
    artifact_tls: TlsPaths,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsPaths {
    ca_file: Option<PathBuf>,
    identity_file: Option<PathBuf>,
}

pub(super) fn readback(
    context: &str,
    namespace: &str,
    name: &str,
    expected_request: &[u8],
    expected_request_sha256: &str,
) -> anyhow::Result<VerifiedWorkerConfiguration> {
    let observed = kubectl_json(
        &kubectl_binary(),
        context,
        namespace,
        ["get", "secret", name, "-o", "json"],
        "independently read fixed Campaign configuration",
    )?;
    verify_readback(
        &observed,
        namespace,
        name,
        expected_request,
        expected_request_sha256,
    )
}

fn config_filename(path: &Path) -> anyhow::Result<String> {
    let name = path
        .strip_prefix("/config")?
        .to_str()
        .context("worker configuration path is not UTF-8")?;
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && !name.contains("..")
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "worker credential must be a direct private /config file"
    );
    Ok(name.into())
}

fn verify_readback(
    observed: &Value,
    namespace: &str,
    name: &str,
    expected_request: &[u8],
    expected_request_sha256: &str,
) -> anyhow::Result<VerifiedWorkerConfiguration> {
    ensure!(
        observed["apiVersion"] == "v1"
            && observed["kind"] == "Secret"
            && observed["type"] == "Opaque"
            && observed["immutable"] == true
            && observed["metadata"]["name"] == name
            && observed["metadata"]["namespace"] == namespace,
        "fixed worker configuration target or immutability changed"
    );
    let uid = observed["metadata"]["uid"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .context("worker configuration readback lacks actual UID")?;
    let data = observed["data"]
        .as_object()
        .context("worker configuration readback lacks bytes")?;
    ensure!(data.len() == 3, "unbounded worker configuration payload");
    let mut decoded = BTreeMap::new();
    let mut total = 0_usize;
    for (key, encoded) in data {
        let encoded = encoded
            .as_str()
            .context("worker configuration entry is not encoded bytes")?;
        ensure!(
            encoded.len() <= 2 * 1024 * 1024,
            "worker configuration entry exceeds byte bound"
        );
        let bytes = decode_base64(encoded)?;
        total = total
            .checked_add(bytes.len())
            .context("worker configuration size overflow")?;
        ensure!(
            total <= 2 * 1024 * 1024,
            "worker configuration exceeds total byte bound"
        );
        decoded.insert(key.as_str(), bytes);
    }
    let request = decoded
        .get("campaign.json")
        .context("worker configuration lacks finalized request")?;
    ensure!(
        request == expected_request
            && hft_research_platform::sha256(request) == expected_request_sha256,
        "worker configuration differs from inspected finalized request"
    );
    let config = decoded
        .get("artifact-io.json")
        .context("worker configuration lacks scoped artifact IO")?;
    ensure!(
        config.len() <= 64 * 1024,
        "artifact IO configuration exceeds bound"
    );
    let config: ArtifactIo = serde_json::from_slice(config)?;
    let endpoint = reqwest::Url::parse(&config.artifact_gateway)?;
    ensure!(
        config.schema_version == "monday.cex_campaign_artifact_io.v1"
            && endpoint.scheme() == "https"
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none()
            && endpoint.path().ends_with('/'),
        "invalid scoped artifact IO consumer configuration"
    );
    ensure!(
        config.artifact_token_file == Path::new("/identity/artifact.token")
            && config.artifact_tls.identity_file.as_deref() == Some(Path::new("/identity/tls.pem")),
        "worker credentials must use the separately controlled Attempt identity"
    );
    let ca_name = config_filename(
        config
            .artifact_tls
            .ca_file
            .as_deref()
            .context("private artifact CA is absent")?,
    )?;
    ensure!(
        ca_name == "ca.pem",
        "private artifact CA must use the fixed static configuration key"
    );
    let keys = BTreeSet::from([
        "campaign.json".to_owned(),
        "artifact-io.json".to_owned(),
        ca_name.clone(),
    ]);
    ensure!(
        keys.len() == 3,
        "private CA aliases scientific configuration"
    );
    let ca = decoded
        .get(ca_name.as_str())
        .context("private artifact CA bytes are absent")?;
    ensure!(
        ca.len() <= 64 * 1024 && !reqwest::Certificate::from_pem_bundle(ca)?.is_empty(),
        "invalid private artifact CA"
    );
    // Tokens and TLS private identity arrive only after the exact task/lease is
    // imported and leased. They are excluded from this signed static identity.
    ensure!(
        keys.iter().map(String::as_str).collect::<BTreeSet<_>>()
            == decoded.keys().copied().collect(),
        "worker configuration exposes unrelated private inputs"
    );
    let encoded = data
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                value
                    .as_str()
                    .context("static configuration entry is not encoded text")?
                    .to_owned(),
            ))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let reference = hft_research_platform::orchestrator::worker_configuration_reference(
        namespace, name, uid, &encoded,
    )?;
    Ok(VerifiedWorkerConfiguration { reference })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_immutable_or_hash_without_actual_configuration_never_proves_a_worker() {
        let forged = serde_json::json!({"apiVersion":"v1","kind":"Secret","type":"Opaque","immutable":true,"metadata":{"name":"worker","namespace":"monday-research"},"data":{"campaign.json":"e30=","artifact-io.json":"e30=","token":"dG9rZW4="}});
        assert!(verify_readback(
            &forged,
            "monday-research",
            "worker",
            b"{}",
            &hft_research_platform::sha256(b"{}")
        )
        .is_err());
        let mut unknown = forged;
        unknown["metadata"]["uid"] = Value::String("actual-uid".into());
        assert!(verify_readback(
            &unknown,
            "monday-research",
            "worker",
            b"{}",
            &hft_research_platform::sha256(b"{}")
        )
        .is_err());
        for path in [
            "/inputs/token",
            "/config/../native-witness.key",
            "/config/private/token",
            "relative-token",
        ] {
            assert!(config_filename(Path::new(path)).is_err());
        }
    }
}
