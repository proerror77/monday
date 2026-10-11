//! Publisher-only OSS policy, budget admission, immutable writes and import.
use crate::{
    build_objects::{exact_prefix, OssLocation, SignedTransport},
    release_budget::{self as budget, Ledger, Phase, Service},
};
use anyhow::{ensure, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

const VERSIONING_RESPONSE_LIMIT: usize = 4096;
const VERSIONING_NODE_LIMIT: u32 = 32;
const OSS_XML_NAMESPACE: &str = "http://doc.oss-cn-hangzhou.aliyuncs.com";
const PUBLICATION_NAMESPACES: [&str; 2] = ["research/builds/", "research/sources/"];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OssConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub role_arn: String,
    pub oidc_provider_arn: String,
    pub audience: String,
    pub subject: String,
    pub publication_namespaces: Vec<String>,
    pub repository_id: u64,
    pub owner_id: u64,
}
impl OssConfig {
    pub fn validate(&self) -> Result<()> {
        self.location().validate()?;
        ensure!(
            self.role_arn.starts_with("acs:ram::")
                && self.role_arn.contains(":role/")
                && self.oidc_provider_arn.starts_with("acs:ram::")
                && self.oidc_provider_arn.contains(":oidc-provider/")
                && !self.audience.is_empty()
                && self.repository_id > 0
                && self.owner_id > 0
                && !self.subject.is_empty(),
            "approved RAM OIDC configuration required"
        );
        ensure!(
            self.publication_namespaces == PUBLICATION_NAMESPACES,
            "publication_namespaces must be exactly research/builds/ and research/sources/ in that order"
        );
        Ok(())
    }
    fn location(&self) -> OssLocation {
        OssLocation {
            bucket: self.bucket.clone(),
            region: self.region.clone(),
            endpoint: self.endpoint.clone(),
        }
    }
    fn admits_prefix(&self, prefix: &str) -> bool {
        exact_prefix(prefix)
            && self
                .publication_namespaces
                .iter()
                .any(|namespace| prefix.starts_with(namespace))
    }
}
pub struct Oss {
    transport: SignedTransport,
    budget: Option<Ledger>,
}
impl Oss {
    pub fn from_file(config: &OssConfig, path: &Path) -> Result<Self> {
        Self::load(config, path, false)
    }
    pub fn from_reader_file(config: &OssConfig, path: &Path) -> Result<Self> {
        Self::load(config, path, true)
    }
    fn load(config: &OssConfig, path: &Path, independent_reader: bool) -> Result<Self> {
        config.validate()?;
        let transport = SignedTransport::from_file(
            &config.location(),
            path,
            std::time::Duration::from_secs(120),
        )?;
        let session = &transport.session;
        ensure!(
            session.prefixes.iter().all(|p| config.admits_prefix(p)),
            "session exceeds publication namespaces"
        );
        let budget = session
            .budget
            .as_ref()
            .map(Ledger::from_reference)
            .transpose()?;
        if let Some(budget) = &budget {
            budget.require_session(
                session.publisher,
                &session.prefixes,
                &crate::identity(config)?,
            )?;
        }
        ensure!(
            if independent_reader {
                !session.publisher && budget.is_none()
            } else {
                budget.is_some()
            },
            "CI requires a private budget; independent import requires a reader session"
        );
        Ok(Self { transport, budget })
    }
    pub fn require_reader(&self) -> Result<()> {
        self.transport.require_reader()
    }
    /// Bind the private session to the publisher's independently recomputed plan.
    pub fn require_publisher_scope(&self, prefixes: &[String]) -> Result<()> {
        self.transport
            .session
            .validate(Utc::now().timestamp_millis())?;
        ensure!(
            self.transport.session.publisher
                && !prefixes.is_empty()
                && prefixes.iter().all(|p| exact_prefix(p))
                && prefixes.windows(2).all(|w| w[0] < w[1])
                && self.transport.session.prefixes == prefixes,
            "publisher session must equal the actual native source/Build plan"
        );
        Ok(())
    }
    fn ledger(&self) -> Result<&Ledger> {
        self.budget.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OSS request requires a cumulative private budget ledger")
        })
    }
    pub fn begin_publication(&self, inventory: &budget::Inventory) -> Result<()> {
        self.ledger()?.require_inventory(inventory)?;
        self.ledger()?.claim("publication")
    }
    pub fn complete_publication(&self) -> Result<()> {
        self.ledger()?.claim("publication_ready")
    }
    fn reserve(&self, service: Service, key: &str, request: u64, response: u64) -> Result<()> {
        self.transport
            .session
            .validate(Utc::now().timestamp_millis())?;
        self.ledger()?.reserve(
            if self.transport.session.publisher {
                Phase::Publish
            } else {
                Phase::Source
            },
            service,
            key,
            request,
            response,
        )
    }
    pub async fn get(
        &self,
        key: &str,
        version: Option<&str>,
        limit: u64,
    ) -> Result<crate::transport::BoundedBody> {
        let request = self.transport.get_request(key, version, limit)?;
        if self.budget.is_some() {
            self.reserve(
                Service::OssRead,
                key,
                0,
                limit.max(budget::SMALL_RESPONSE_LIMIT),
            )?;
        } else {
            self.require_reader()?;
        }
        SignedTransport::read(request, limit).await
    }
    async fn require_unversioned_bucket(&self) -> Result<()> {
        let request = self
            .transport
            .publication_request(reqwest::Method::GET, "", None)?;
        self.reserve(Service::OssRead, "", 0, VERSIONING_RESPONSE_LIMIT as u64)?;
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS versioning read unavailable"))?;
        ensure!(response.status().is_success(), "OSS versioning read denied");
        let mut response =
            crate::transport::BoundedBody::new(response, VERSIONING_RESPONSE_LIMIT as u64)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("OSS versioning read interrupted"))?
        {
            ensure!(
                chunk.len() <= VERSIONING_RESPONSE_LIMIT - bytes.len(),
                "OSS versioning response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        require_unversioned_response(&bytes)
    }
    pub async fn put_file(&self, key: &str, path: &Path, size: u64) -> Result<()> {
        use tokio::io::AsyncReadExt;
        ensure!(
            (1..=512 * 1024 * 1024).contains(&size),
            "OSS upload payload exceeds object bound"
        );
        let file = tokio::fs::File::open(path).await?;
        ensure!(
            file.metadata().await?.is_file() && file.metadata().await?.len() == size,
            "OSS upload file changed before send"
        );
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::with_capacity(
            file.take(size),
            64 * 1024,
        ));
        self.put(key, body, size).await
    }
    pub async fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= budget::JSON_LIMIT,
            "OSS metadata exceeds payload bound"
        );
        let size = bytes.len() as u64;
        self.put(key, bytes.into(), size).await
    }
    async fn put(&self, key: &str, body: reqwest::Body, size: u64) -> Result<()> {
        self.require_unversioned_bucket().await?;
        let request = self
            .transport
            .publication_request(reqwest::Method::PUT, key, None)?
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body);
        self.reserve(Service::OssWrite, key, size, budget::SMALL_RESPONSE_LIMIT)?;
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS upload unavailable"))?;
        // A conflict does not prove equality. The caller always performs a GET.
        ensure!(
            response.status().is_success() || response.status() == reqwest::StatusCode::CONFLICT,
            "OSS upload rejected"
        );
        let mut response =
            crate::transport::BoundedBody::new(response, budget::SMALL_RESPONSE_LIMIT)?;
        while response.chunk().await?.is_some() {}
        Ok(())
    }
    pub async fn check(&self, source: &str) -> Result<()> {
        ensure!(
            !self.transport.session.publisher,
            "OSS preflight requires source-reader session"
        );
        self.ledger()?.claim("preflight")?;
        self.require_unversioned_bucket().await?;
        let key = format!("research/sources/{source}/source.tar");
        let request = self
            .transport
            .publication_request(reqwest::Method::HEAD, &key, None)?;
        self.reserve(Service::OssRead, &key, 0, 0)?;
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS TLS unavailable"))?;
        ensure!(
            response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
            "OSS authenticated preflight rejected"
        );
        self.ledger()?.claim("preflight_ready")?;
        Ok(())
    }
}

fn require_unversioned_response(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() <= VERSIONING_RESPONSE_LIMIT,
        "OSS versioning response exceeds bound"
    );
    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("OSS versioning response is not UTF-8"))?;
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: VERSIONING_NODE_LIMIT,
        },
    )
    .map_err(|_| anyhow::anyhow!("OSS versioning response is not bounded well-formed XML"))?;
    let root = document.root_element();
    // The tree parser coalesces identical namespace declarations. This response
    // permits only one optional default namespace, so also count its raw '='.
    let opening = text[root.range().start..]
        .split_once('>')
        .map(|(opening, _)| opening)
        .unwrap_or_default();
    ensure!(
        root.tag_name().name() == "VersioningConfiguration"
            && root
                .tag_name()
                .namespace()
                .is_none_or(|namespace| {
                    namespace.is_empty() || namespace == OSS_XML_NAMESPACE
                })
            && root.attributes().len() == 0
            && root.namespaces().all(|namespace| {
                namespace.name().is_none()
                    && (namespace.uri().is_empty() || namespace.uri() == OSS_XML_NAMESPACE)
            })
            && opening.bytes().filter(|byte| *byte == b'=').count() <= 1
            && root.children().all(|node| {
                node.is_comment()
                    || (node.is_text()
                        && node
                            .text()
                            .is_some_and(|text| text.bytes().all(|b| b" \t\r\n".contains(&b))))
            })
            && document.root().children().all(|node| {
                node.is_element()
                    || node.is_comment()
                    || (node.is_text()
                        && node
                            .text()
                            .is_some_and(|text| text.bytes().all(|b| b" \t\r\n".contains(&b))))
            }),
        "OSS overwrite refusal requires a never-versioned bucket; do not change bucket settings automatically"
    );
    Ok(())
}

/// RAM intersects this policy with the operator role. It never grants Run authority.
pub fn session_policy(config: &OssConfig, prefixes: &[String], publisher: bool) -> Result<String> {
    config.validate()?;
    ensure!(
        !prefixes.is_empty()
            && prefixes.iter().all(|p| exact_prefix(p))
            && prefixes.windows(2).all(|w| w[0] < w[1]),
        "invalid OSS session prefixes"
    );
    ensure!(
        prefixes.iter().all(|p| config.admits_prefix(p)),
        "requested prefix outside publication namespaces"
    );
    let actions = if publisher {
        vec!["oss:GetObject", "oss:GetObjectVersion", "oss:PutObject"]
    } else {
        vec!["oss:GetObject", "oss:GetObjectVersion"]
    };
    Ok(serde_json::to_string(
        &serde_json::json!({"Version":"1", "Statement":[{"Effect":"Allow", "Action":actions, "Resource":prefixes.iter().map(|p| format!("acs:oss:*:*:{}/{}*",config.bucket,p)).collect::<Vec<_>>()},{"Effect":"Allow","Action":["oss:GetBucketVersioning"],"Resource":[format!("acs:oss:*:*:{}",config.bucket)]}]}),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_objects::Session;
    use std::collections::BTreeMap;
    #[test]
    fn release_oss_refuses_enabled_suspended_or_unknown_bucket_state() {
        assert!(
            require_unversioned_response(b"<?xml version=\"1.0\"?><VersioningConfiguration/>")
                .is_ok()
        );
        for body in [
            b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>"
                .as_slice(),
            b"<VersioningConfiguration><Status>Suspended</Status></VersioningConfiguration>",
            b"AccessDenied",
            b"<VersioningConfiguration><Unexpected/></VersioningConfiguration>",
            b"<VersioningConfiguration><Status>unknown</Status></VersioningConfiguration>",
        ] {
            assert!(require_unversioned_response(body).is_err());
        }
    }
    #[test]
    fn release_oss_rejects_malformed_versioning_xml() {
        let malformed = [
            "<VersioningConfiguration broken/>",
            "<VersioningConfiguration xmlns='unterminated/>",
            "<VersioningConfiguration xmlns='first' xmlns='second'/>",
            "<VersioningConfiguration xmlns='' xmlns=''/>",
            "<VersioningConfiguration xmlns='http://doc.oss-cn-hangzhou.aliyuncs.com' xmlns='http://doc.oss-cn-hangzhou.aliyuncs.com'/>",
            "<?xml invalid?><VersioningConfiguration/>",
            "<VersioningConfiguration xmlns='&unknown;'/>",
            "<VersioningConfiguration undeclared:attribute='value'/>",
        ];
        let accepted: Vec<_> = malformed
            .iter()
            .filter(|body| require_unversioned_response(body.as_bytes()).is_ok())
            .collect();
        assert!(accepted.is_empty(), "accepted malformed XML: {accepted:?}");
    }
    #[test]
    fn release_oss_accepts_only_bounded_empty_versioning_documents() {
        for body in [
            "<VersioningConfiguration/>",
            "<VersioningConfiguration></VersioningConfiguration>",
            "<VersioningConfiguration xmlns=''/>",
            "<?xml version='1.0' encoding='UTF-8'?><VersioningConfiguration xmlns='http://doc.oss-cn-hangzhou.aliyuncs.com'/>",
            "<!-- before --><VersioningConfiguration> \t\r\n<!-- inside --></VersioningConfiguration><!-- after -->",
        ] {
            assert!(require_unversioned_response(body.as_bytes()).is_ok(), "rejected {body}");
        }
        for body in [
            "<VersioningConfiguration xmlns='urn:foreign'/>",
            "<VersioningConfiguration xmlns='http://doc.oss-cn-hangzhou.aliyuncs.com' xmlns:other='urn:foreign'/>",
            "<VersioningConfiguration enabled='false'/>",
            "<VersioningConfiguration><Status/></VersioningConfiguration>",
            "<VersioningConfiguration>Enabled</VersioningConfiguration>",
            "<VersioningConfiguration/><VersioningConfiguration/>",
            "<VersioningConfiguration>\u{a0}</VersioningConfiguration>",
            "<?instruction value?><VersioningConfiguration/>",
            "<VersioningConfiguration><?instruction value?></VersioningConfiguration>",
            "<!DOCTYPE VersioningConfiguration [<!ENTITY e ' '>]><VersioningConfiguration>&e;</VersioningConfiguration>",
        ] {
            assert!(require_unversioned_response(body.as_bytes()).is_err(), "accepted {body}");
        }
        let base = "<VersioningConfiguration/>";
        let exact = format!(
            "{}{}",
            " ".repeat(VERSIONING_RESPONSE_LIMIT - base.len()),
            base
        );
        assert!(require_unversioned_response(exact.as_bytes()).is_ok());
        assert!(require_unversioned_response(format!(" {exact}").as_bytes()).is_err());
        let crowded = format!(
            "<VersioningConfiguration>{}</VersioningConfiguration>",
            "<!-- node -->".repeat(VERSIONING_NODE_LIMIT as usize)
        );
        assert!(crowded.len() < VERSIONING_RESPONSE_LIMIT);
        assert!(require_unversioned_response(crowded.as_bytes()).is_err());
        assert!(require_unversioned_response(b"\xff<VersioningConfiguration/>").is_err());
    }
    fn config() -> OssConfig {
        OssConfig {
            bucket: "test-bucket".into(),
            region: "cn-hangzhou".into(),
            endpoint: "https://test-bucket.oss-cn-hangzhou.aliyuncs.com/".into(),
            role_arn: "acs:ram::123:role/test".into(),
            oidc_provider_arn: "acs:ram::123:oidc-provider/test".into(),
            audience: "test".into(),
            subject: "operator-approved-subject".into(),
            publication_namespaces: PUBLICATION_NAMESPACES.map(str::to_owned).to_vec(),
            repository_id: 1,
            owner_id: 2,
        }
    }
    #[test]
    fn release_oss_stable_configuration_rejects_legacy_or_expanded_namespaces() {
        let original = serde_json::to_value(config()).unwrap();
        let mut legacy = original.clone();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("publication_namespaces");
        legacy["role_prefixes"] =
            serde_json::json!([format!("research/builds/{}/", "a".repeat(64))]);
        assert!(serde_json::from_value::<OssConfig>(legacy).is_err());
        let mut mixed = original;
        mixed["role_prefixes"] = serde_json::json!([]);
        assert!(serde_json::from_value::<OssConfig>(mixed).is_err());
        for namespaces in [
            vec![],
            vec!["research/"],
            vec!["research/builds/*", "research/sources/*"],
            vec!["research/builds/"],
            vec!["research/sources/", "research/builds/"],
            vec!["research/builds/", "research/builds/", "research/sources/"],
            vec![
                "research/builds/",
                "research/sources/",
                "research/attempts/",
            ],
        ] {
            let mut invalid = config();
            invalid.publication_namespaces = namespaces.into_iter().map(str::to_owned).collect();
            assert!(invalid.validate().is_err());
        }
    }
    #[test]
    fn release_oss_unchanged_config_narrows_each_release_and_reader_phase() {
        let config = config();
        for hex in ["a", "b"] {
            let prefixes = vec![
                format!("research/builds/{}/", hex.repeat(64)),
                format!("research/sources/{}/", hex.repeat(40)),
            ];
            let policy: serde_json::Value =
                serde_json::from_str(&session_policy(&config, &prefixes, true).unwrap()).unwrap();
            assert_eq!(
                policy["Statement"][0]["Resource"],
                serde_json::json!(prefixes
                    .iter()
                    .map(|p| format!("acs:oss:*:*:test-bucket/{p}*"))
                    .collect::<Vec<_>>())
            );
            assert_eq!(
                policy["Statement"][0]["Action"],
                serde_json::json!(["oss:GetObject", "oss:GetObjectVersion", "oss:PutObject"])
            );
            let source_only = session_policy(&config, &prefixes[1..], false).unwrap();
            assert!(
                !source_only.contains("PutObject") && !source_only.contains("research/builds/")
            );
        }
        for prefixes in [
            config.publication_namespaces.clone(),
            vec!["research/attempts/1/".into()],
            vec!["research/builds/../".into()],
            vec![format!("research/builds/{}/", "A".repeat(64))],
            vec![format!("research/builds/{}/", "a".repeat(64)); 2],
            vec![
                format!("research/sources/{}/", "a".repeat(40)),
                format!("research/builds/{}/", "a".repeat(64)),
            ],
        ] {
            assert!(session_policy(&config, &prefixes, true).is_err());
            assert!(session_policy(&config, &prefixes, false).is_err());
        }
    }
    #[test]
    fn release_oss_private_session_must_match_recomputed_publication_plan() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap().join("session");
        let expected = vec![
            format!("research/builds/{}/", "a".repeat(64)),
            format!("research/sources/{}/", "a".repeat(40)),
        ];
        let mut session = Session {
            access_key_id: "fixture".into(),
            access_key_secret: "fixture".into(),
            security_token: "fixture".into(),
            expires_ms: Utc::now().timestamp_millis() + 60_000,
            publisher: true,
            prefixes: expected.clone(),
            versions: BTreeMap::new(),
            budget: None,
        };
        let save = |session: &Session| {
            std::fs::write(&path, serde_json::to_vec(session).unwrap()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        save(&session);
        assert!(Oss::from_file(&config(), &path).is_err());
        assert!(Oss::from_reader_file(&config(), &path).is_err());
        let (binding, mut inventory, mut envelope) =
            crate::release_budget::tests::fixture("controller", 1, 1);
        inventory.publisher_prefixes = expected.clone();
        inventory.program_objects[0].key = format!("{}program", expected[0]);
        envelope.limits = inventory.publication_limits().unwrap();
        envelope.allocations[0].limits = envelope.limits;
        let ledger = Ledger::create(
            &path.with_file_name("ledger"),
            envelope,
            binding,
            inventory,
            crate::identity(&config()).unwrap(),
        )
        .unwrap();
        session.budget = Some(ledger.reference());
        save(&session);
        let oss = Oss::from_file(&config(), &path).unwrap();
        assert!(oss.require_publisher_scope(&expected).is_ok());
        assert!(oss.require_reader().is_err());
        for prefixes in [
            vec![expected[0].clone()],
            vec![
                format!("research/builds/{}/", "b".repeat(64)),
                expected[1].clone(),
            ],
            vec![
                expected[0].clone(),
                format!("research/builds/{}/", "b".repeat(64)),
                expected[1].clone(),
            ],
        ] {
            session.prefixes = prefixes;
            save(&session);
            assert!(Oss::from_file(&config(), &path).is_err());
        }
        session.prefixes = expected.clone();
        session.publisher = false;
        session.budget = None;
        save(&session);
        assert!(Oss::from_file(&config(), &path).is_err());
        let oss = Oss::from_reader_file(&config(), &path).unwrap();
        assert!(oss.require_publisher_scope(&expected).is_err());
        assert!(oss.require_reader().is_ok());
        session.prefixes = config().publication_namespaces;
        save(&session);
        assert!(Oss::from_file(&config(), &path).is_err());
        session.prefixes = expected;
        session.expires_ms = Utc::now().timestamp_millis() - 1;
        save(&session);
        assert!(Oss::from_file(&config(), &path).is_err());
    }
    #[test]
    fn release_oss_policy_excludes_unadmitted_authority() {
        let prefix = format!("research/builds/{}/", "a".repeat(64));
        let policy = session_policy(&config(), &[prefix], true).unwrap();
        assert!(
            !policy.contains("DeleteObject")
                && !policy.contains("SetBucket")
                && !policy.contains("research/*")
        );
        let mut empty = config();
        empty.publication_namespaces.clear();
        assert!(empty.validate().is_err());
        let mut invalid = config();
        invalid.endpoint = "https://foreign/".into();
        assert!(invalid.validate().is_err());
    }
}
