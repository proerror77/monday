//! Offline, paused persistent-platform assets. Rendering neither connects to a
//! provider/database nor grants authority; all deployment identities are inputs.
use crate::valid_digest;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub postgres_image: String,
    pub clickhouse_image: String,
    pub control_image: String,
    pub session_image: String,
    pub tls_image: String,
    pub init_image: String,
    pub storage_class: String,
    pub broker_claim: String,
    pub postgres_storage: String,
    pub clickhouse_storage: String,
    pub artifact_storage: String,
    pub session_storage: String,
    pub workspace_claim: String,
    pub workspace_source_revision: String,
    pub issuer_storage: String,
    pub session_checkpoint_sha256: Option<String>,
    pub cluster: String,
    pub kube_api_ip: std::net::IpAddr,
    pub tenant: String,
    pub codex_sha256: String,
    pub experiment_sha256: String,
    pub session_policy_sha256: String,
}
fn name(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0] != b'-'
        && value.as_bytes()[value.len() - 1] != b'-'
}
fn pinned_image(value: &str) -> bool {
    value
        .split_once("@sha256:")
        .is_some_and(|(repository, digest)| {
            !repository.is_empty()
                && repository.len() <= 256
                && repository
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._:-".contains(&b))
                && valid_digest(digest)
        })
}
fn storage(value: &str) -> bool {
    value
        .strip_suffix("Gi")
        .and_then(|n| n.parse::<u16>().ok())
        .is_some_and(|n| (1..=1024).contains(&n))
}
fn configmap(name: &str, entries: &[(&str, &str)]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":"monday-research"},
        "data":entries.iter().copied().collect::<BTreeMap<_,_>>()
    }))?)
}
impl Inventory {
    pub fn render(&self) -> Result<BTreeMap<String, String>> {
        ensure!(
            [
                &self.postgres_image,
                &self.clickhouse_image,
                &self.control_image,
                &self.session_image,
                &self.tls_image,
                &self.init_image
            ]
            .iter()
            .all(|v| pinned_image(v)),
            "all runtime images require exact OCI digests"
        );
        ensure!(
            [
                &self.storage_class,
                &self.broker_claim,
                &self.workspace_claim,
                &self.cluster,
                &self.tenant
            ]
            .iter()
            .all(|v| name(v)),
            "invalid deployment identifier"
        );
        ensure!(
            [
                &self.postgres_storage,
                &self.clickhouse_storage,
                &self.artifact_storage,
                &self.session_storage,
                &self.issuer_storage
            ]
            .iter()
            .all(|v| storage(v)),
            "storage must be finite 1..1024 Gi per volume"
        );
        ensure!(
            [
                &self.codex_sha256,
                &self.experiment_sha256,
                &self.session_policy_sha256
            ]
            .iter()
            .all(|v| valid_digest(v)),
            "missing native Session/source identities"
        );
        ensure!(
            !self.kube_api_ip.is_unspecified() && !self.kube_api_ip.is_multicast(),
            "explicit Kubernetes API host required"
        );
        ensure!(
            self.workspace_source_revision.len() == 40
                && self
                    .workspace_source_revision
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "missing reviewed Git workspace source"
        );
        ensure!(
            self.session_checkpoint_sha256
                .as_ref()
                .is_none_or(|digest| valid_digest(digest)),
            "invalid registered recovery checkpoint identity"
        );
        let api_ip = match self.kube_api_ip {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        };
        let cidr = format!(
            "{}/{}",
            self.kube_api_ip,
            if self.kube_api_ip.is_ipv4() { 32 } else { 128 }
        );
        let substitutions = [
            ("__POSTGRES_IMAGE__", &self.postgres_image),
            ("__CLICKHOUSE_IMAGE__", &self.clickhouse_image),
            ("__CONTROL_IMAGE__", &self.control_image),
            ("__SESSION_IMAGE__", &self.session_image),
            ("__TLS_IMAGE__", &self.tls_image),
            ("__INIT_IMAGE__", &self.init_image),
            ("__STORAGE_CLASS__", &self.storage_class),
            ("__BROKER_CLAIM__", &self.broker_claim),
            ("__POSTGRES_STORAGE__", &self.postgres_storage),
            ("__CLICKHOUSE_STORAGE__", &self.clickhouse_storage),
            ("__ARTIFACT_STORAGE__", &self.artifact_storage),
            ("__SESSION_STORAGE__", &self.session_storage),
            ("__WORKSPACE_CLAIM__", &self.workspace_claim),
            ("__ISSUER_STORAGE__", &self.issuer_storage),
            ("__CLUSTER__", &self.cluster),
            ("__KUBE_API_IP__", &api_ip),
            ("__KUBE_API_CIDR__", &cidr),
            ("__TENANT__", &self.tenant),
            ("__CODEX_SHA256__", &self.codex_sha256),
            ("__EXPERIMENT_SHA256__", &self.experiment_sha256),
            ("__SESSION_POLICY_SHA256__", &self.session_policy_sha256),
        ];
        let mut assets = BTreeMap::new();
        for (filename, template) in [
            ("postgres.yaml",include_str!("../../../../deployment/aliyun/research/foundation/postgres.template.yaml")),
            ("clickhouse.yaml",include_str!("../../../../deployment/aliyun/research/foundation/clickhouse.template.yaml")),
            ("control.yaml",include_str!("../../../../deployment/aliyun/research/foundation/control.template.yaml")),
            ("session.yaml",include_str!("../../../../deployment/aliyun/research/foundation/session.template.yaml")),
            ("rbac.yaml",include_str!("../../../../deployment/aliyun/research/foundation/rbac.template.yaml")),
            ("network.yaml",include_str!("../../../../deployment/aliyun/research/foundation/network.template.yaml")),
            ("service.json",include_str!("../../../../deployment/aliyun/research/foundation/config/service.template.json")),
            ("session.json",include_str!("../../../../deployment/aliyun/research/foundation/config/session.template.json")),
        ] {
            let mut rendered = template.to_owned();
            for (key,value) in substitutions { rendered = rendered.replace(key,value); }
            ensure!(!rendered.contains("__"), "unresolved deployment input");
            assets.insert(filename.to_owned(),rendered);
        }
        let install =
            include_str!("../../../../deployment/aliyun/research/foundation/install-tls.sh");
        assets.insert("postgres-configmap.json".into(),configmap("monday-foundation-postgres", &[("install-tls.sh",install),("pg_hba.conf",include_str!("../../../../deployment/aliyun/research/foundation/postgres/pg_hba.conf"))])?);
        assets.insert("clickhouse-configmap.json".into(),configmap("monday-foundation-clickhouse", &[("install-tls.sh",install),("config.xml",include_str!("../../../../deployment/aliyun/research/foundation/clickhouse/config.xml")),("users.xml",include_str!("../../../../deployment/aliyun/research/foundation/clickhouse/users.xml"))])?);
        assets.insert(
            "control-configmap.json".into(),
            configmap(
                "monday-foundation-control",
                &[
                    ("install-tls.sh", install),
                    (
                        "install-state.sh",
                        include_str!(
                            "../../../../deployment/aliyun/research/foundation/install-state.sh"
                        ),
                    ),
                    ("service.json", &assets["service.json"]),
                    (
                        "gateway.json",
                        include_str!(
                            "../../../../deployment/aliyun/research/foundation/config/gateway.json"
                        ),
                    ),
                    (
                        "nginx.conf",
                        include_str!(
                            "../../../../deployment/aliyun/research/foundation/tls/nginx.conf"
                        ),
                    ),
                ],
            )?,
        );
        assets.insert(
            "session-configmap.json".into(),
            configmap(
                "monday-foundation-session",
                &[("session.json", &assets["session.json"])],
            )?,
        );
        assets.insert("source-environment.json".into(), serde_json::to_string_pretty(&serde_json::json!({"schema":"monday.foundation_source_environment.v1", "workspace_claim":self.workspace_claim, "workspace_source_revision":self.workspace_source_revision, "session_image":self.session_image, "codex_sha256":self.codex_sha256, "runtime_readback_stage":"not_performed_by_renderer"}))?);
        if let Some(checkpoint) = &self.session_checkpoint_sha256 {
            assets.insert("session-resume.patch.json".into(), serde_json::to_string_pretty(&serde_json::json!([{"op":"replace","path":"/spec/template/spec/containers/0/args","value":["resume","/config/session.json",checkpoint,"/state/checkpoints/native.json"]}]))?);
        }
        assets.insert("broker-tools.paused.json".into(), "[]\n".into());
        assets.insert("broker-artifacts.paused.json".into(), "[]\n".into());
        Ok(assets)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    #[test]
    fn offline_assets_require_exact_inputs_and_cannot_render_enabled_or_injected_resources(
    ) -> Result<()> {
        let image = format!("fixture/runtime@sha256:{}", "a".repeat(64));
        let mut inventory = Inventory {
            postgres_image: image.clone(),
            clickhouse_image: image.clone(),
            control_image: image.clone(),
            session_image: image.clone(),
            tls_image: image.clone(),
            init_image: image,
            storage_class: "existing-retained".into(),
            broker_claim: "existing-private-broker".into(),
            postgres_storage: "10Gi".into(),
            clickhouse_storage: "100Gi".into(),
            artifact_storage: "20Gi".into(),
            session_storage: "5Gi".into(),
            workspace_claim: "existing-git-workspace".into(),
            workspace_source_revision: "a".repeat(40),
            issuer_storage: "5Gi".into(),
            session_checkpoint_sha256: None,
            cluster: "fixture".into(),
            kube_api_ip: "127.0.0.1".parse()?,
            tenant: "fixture".into(),
            codex_sha256: "b".repeat(64),
            experiment_sha256: "c".repeat(64),
            session_policy_sha256: "d".repeat(64),
        };
        let assets = inventory.render()?;
        for name in [
            "postgres.yaml",
            "clickhouse.yaml",
            "control.yaml",
            "session.yaml",
        ] {
            assert!(assets[name].contains("replicas: 0"));
            assert!(!assets[name].contains("kind: Job"));
        }
        assert_eq!(assets["broker-tools.paused.json"], "[]\n");
        serde_json::from_str::<crate::service::ServiceConfig>(&assets["service.json"])?;
        let service: crate::service::ServiceConfig = serde_json::from_str(&assets["service.json"])?;
        assert!(!service.terminal_retirement.enabled);
        let issuer = service
            .attempt_identity
            .context("missing configured issuer")?;
        assert_eq!(
            issuer.capabilities_file.to_str(),
            Some("/run/broker/private/artifacts.json")
        );
        assert_eq!(
            issuer.state_root.to_str(),
            Some("/state/attempt-identities")
        );
        assert!(assets["control.yaml"].contains("name: issuer-state"));
        assert!(assets["session.yaml"].contains("claimName: existing-git-workspace"));
        let session: serde_json::Value = serde_json::from_str(&assets["session.json"])?;
        assert!(session.get("checkpoint_file").is_none());
        assert!(assets["rbac.yaml"].contains("verbs: [create, get, delete]"));
        inventory.session_checkpoint_sha256 = Some("e".repeat(64));
        let recovered = inventory.render()?;
        let patch: serde_json::Value =
            serde_json::from_str(&recovered["session-resume.patch.json"])?;
        assert_eq!(
            patch[0]["value"],
            serde_json::json!([
                "resume",
                "/config/session.json",
                "e".repeat(64),
                "/state/checkpoints/native.json"
            ])
        );
        assert!(recovered["session.yaml"].contains("replicas: 0"));
        let source = inventory.control_image.clone();
        inventory.control_image = "fixture/runtime:latest".into();
        assert!(inventory.render().is_err());
        inventory.control_image = source;
        inventory.broker_claim = "existing\n---\nkind: Pod".into();
        assert!(inventory.render().is_err());
        Ok(())
    }
}
