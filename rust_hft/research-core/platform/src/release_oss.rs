//! Release-only OSS transport. Scientific AttemptWriter remains PG fenced.
use crate::release_budget::{self as budget, Ledger, Phase, Service};
use anyhow::{ensure, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
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
        ensure!(
            (3..=63).contains(&self.bucket.len())
                && self
                    .bucket
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "invalid OSS bucket"
        );
        ensure!(
            !self.region.is_empty()
                && self
                    .region
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "invalid OSS region"
        );
        let expected = format!("https://{}.oss-{}.aliyuncs.com/", self.bucket, self.region);
        let internal = format!(
            "https://{}.oss-{}-internal.aliyuncs.com/",
            self.bucket, self.region
        );
        ensure!(
            self.endpoint == expected || self.endpoint == internal,
            "OSS endpoint must bind bucket and region over HTTPS"
        );
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
    fn admits_prefix(&self, prefix: &str) -> bool {
        exact_prefix(prefix)
            && self
                .publication_namespaces
                .iter()
                .any(|namespace| prefix.starts_with(namespace))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub access_key_id: String,
    pub access_key_secret: String,
    pub security_token: String,
    pub expires_ms: i64,
    pub publisher: bool,
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
    #[serde(default)]
    pub budget: Option<budget::Reference>,
}
impl Session {
    pub fn validate(&self, now: i64) -> Result<()> {
        ensure!(
            self.expires_ms > now && self.expires_ms <= now + 900_000,
            "OSS session expired or exceeds fifteen minutes"
        );
        ensure!(
            [&self.access_key_id, &self.access_key_secret]
                .iter()
                .all(|s| !s.is_empty()
                    && s.len() <= 16_384
                    && s.bytes().all(|b| b.is_ascii_graphic())),
            "invalid OSS session"
        );
        // STS token length is variable. Transport and private-file bounds still apply.
        ensure!(
            !self.security_token.is_empty()
                && self.security_token.bytes().all(|b| b.is_ascii_graphic()),
            "invalid OSS security token"
        );
        ensure!(
            !self.prefixes.is_empty()
                && self.prefixes.windows(2).all(|w| w[0] < w[1])
                && self.prefixes.iter().all(|p| exact_prefix(p)),
            "OSS scope must use sorted exact source/Build prefixes"
        );
        Ok(())
    }
}
pub(crate) fn exact_prefix(p: &str) -> bool {
    p.strip_prefix("research/sources/")
        .and_then(|s| s.strip_suffix('/'))
        .is_some_and(|s| {
            s.len() == 40
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || p.strip_prefix("research/builds/")
            .and_then(|s| s.strip_suffix('/'))
            .is_some_and(crate::valid_digest)
}
fn safe_key(key: &str) -> bool {
    key.starts_with("research/")
        && key
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
}
fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut h = Hmac::<Sha256>::new_from_slice(key).expect("HMAC supports every key length");
    h.update(data.as_bytes());
    h.finalize().into_bytes().to_vec()
}
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
pub struct Oss {
    config: OssConfig,
    session: Session,
    client: reqwest::Client,
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
        let session: Session = serde_json::from_slice(&crate::transport::read_private_file(path)?)
            .map_err(|_| anyhow::anyhow!("invalid private OSS session file"))?;
        session.validate(Utc::now().timestamp_millis())?;
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
        Ok(Self {
            config: config.clone(),
            session,
            budget,
            client: crate::transport::TlsConfig::default()
                .client(std::time::Duration::from_secs(120), true)?,
        })
    }
    fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        version: Option<&str>,
    ) -> Result<reqwest::RequestBuilder> {
        let now = Utc::now();
        self.session.validate(now.timestamp_millis())?;
        ensure!(
            (key.is_empty() && method == reqwest::Method::GET)
                || (safe_key(key) && self.session.prefixes.iter().any(|p| key.starts_with(p))),
            "OSS key outside exact release scope"
        );
        ensure!(
            method != reqwest::Method::PUT || self.session.publisher,
            "reader cannot publish"
        );
        let date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let query = match version {
            Some(v) => {
                ensure!(!v.is_empty() && v.len() <= 1024, "invalid OSS version");
                format!("versionId={}", encode(v))
            }
            None if key.is_empty() => "versioning".into(),
            None => String::new(),
        };
        let mut headers = BTreeMap::from([
            ("x-oss-content-sha256", "UNSIGNED-PAYLOAD".to_owned()),
            ("x-oss-date", date.clone()),
            ("x-oss-security-token", self.session.security_token.clone()),
        ]);
        if method == reqwest::Method::PUT {
            headers.insert("x-oss-forbid-overwrite", "true".into());
            headers.insert("x-oss-storage-class", "Standard".into());
        }
        let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let canonical = format!(
            "{}\n/{}/{}\n{}\n{}\n\nUNSIGNED-PAYLOAD",
            method, self.config.bucket, key, query, canonical_headers
        );
        let scope = format!(
            "{}/{}/oss/aliyun_v4_request",
            &date[..8],
            self.config.region
        );
        let to_sign = format!(
            "OSS4-HMAC-SHA256\n{date}\n{scope}\n{}",
            crate::sha256(canonical.as_bytes())
        );
        let k = hmac(
            format!("aliyun_v4{}", self.session.access_key_secret).as_bytes(),
            &date[..8],
        );
        let k = hmac(&k, &self.config.region);
        let k = hmac(&k, "oss");
        let k = hmac(&k, "aliyun_v4_request");
        let signature: String = hmac(&k, &to_sign)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut url = reqwest::Url::parse(&self.config.endpoint)?.join(key)?;
        if !query.is_empty() {
            url.set_query(Some(&query));
        }
        let mut request = self.client.request(method, url).header(
            "Authorization",
            format!(
                "OSS4-HMAC-SHA256 Credential={}/{scope},Signature={signature}",
                self.session.access_key_id
            ),
        );
        for (k, v) in headers {
            request = request.header(k, v);
        }
        Ok(request)
    }
    pub fn require_reader(&self) -> Result<()> {
        ensure!(
            !self.session.publisher,
            "ACK import requires separate readonly OSS credentials"
        );
        Ok(())
    }
    /// Bind the private session to the publisher's independently recomputed plan.
    pub fn require_publisher_scope(&self, prefixes: &[String]) -> Result<()> {
        self.session.validate(Utc::now().timestamp_millis())?;
        ensure!(
            self.session.publisher
                && !prefixes.is_empty()
                && prefixes.iter().all(|p| self.config.admits_prefix(p))
                && prefixes.windows(2).all(|w| w[0] < w[1])
                && self.session.prefixes == prefixes,
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
        self.session.validate(Utc::now().timestamp_millis())?;
        self.ledger()?.reserve(
            if self.session.publisher {
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
        ensure!(
            limit > 0 && limit <= 512 * 1024 * 1024,
            "OSS read payload bound required"
        );
        let request = self.request(
            reqwest::Method::GET,
            key,
            version.or_else(|| self.session.versions.get(key).map(String::as_str)),
        )?;
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
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS read unavailable"))?;
        ensure!(
            response.status().is_success(),
            "OSS evidence missing or rejected"
        );
        crate::transport::BoundedBody::new(response, limit)
    }
    async fn require_unversioned_bucket(&self) -> Result<()> {
        let request = self.request(reqwest::Method::GET, "", None)?;
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
            .request(reqwest::Method::PUT, key, None)?
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
            !self.session.publisher,
            "OSS preflight requires source-reader session"
        );
        self.ledger()?.claim("preflight")?;
        self.require_unversioned_bucket().await?;
        let key = format!("research/sources/{source}/source.tar");
        let request = self.request(reqwest::Method::HEAD, &key, None)?;
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
    #[test]
    fn release_oss_scope_and_expiry_fail_closed() {
        for p in [
            "research/",
            "research/builds/*/",
            "research/sources/../",
            "research/attempts/x/",
        ] {
            assert!(!exact_prefix(p));
        }
        for key in [
            "research//x",
            "research/../x",
            "research/x?versionId=y",
            "research/./x",
        ] {
            assert!(!safe_key(key));
        }
        let mut session = Session {
            access_key_id: "test".into(),
            access_key_secret: "test".into(),
            security_token: "test".into(),
            expires_ms: 1001,
            publisher: false,
            versions: BTreeMap::new(),
            budget: None,
            prefixes: vec![format!("research/builds/{}/", "a".repeat(64))],
        };
        assert!(session.validate(1000).is_ok());
        session.security_token = "x".repeat(20_000);
        assert!(session.validate(1000).is_ok());
        session.security_token.push('\n');
        assert!(session.validate(1000).is_err());
        session.security_token = "test".into();
        assert!(session.validate(1001).is_err());
        session.expires_ms = 901001;
        assert!(session.validate(1000).is_err());
        session.expires_ms = 1001;
        session.prefixes.push(session.prefixes[0].clone());
        assert!(session.validate(1000).is_err());
        assert_eq!(encode("v+/="), "v%2B%2F%3D");
    }
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
    fn release_oss_requests_bind_scope_role_version_and_overwrite_header() {
        let prefix = format!("research/builds/{}/", "a".repeat(64));
        let mut oss = Oss {
            config: config(),
            session: Session {
                access_key_id: "fixture".into(),
                access_key_secret: "fixture".into(),
                security_token: "fixture".into(),
                expires_ms: Utc::now().timestamp_millis() + 60000,
                publisher: false,
                prefixes: vec![prefix.clone()],
                versions: BTreeMap::new(),
                budget: None,
            },
            client: reqwest::Client::new(),
            budget: None,
        };
        let key = format!("{prefix}program");
        assert!(oss.request(reqwest::Method::PUT, &key, None).is_err());
        assert!(oss
            .request(
                reqwest::Method::GET,
                "research/builds/foreign/program",
                None
            )
            .is_err());
        let get = oss
            .request(reqwest::Method::GET, &key, Some("v+/="))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(get.url().query(), Some("versionId=v%2B%2F%3D"));
        oss.session.publisher = true;
        let put = oss
            .request(reqwest::Method::PUT, &key, None)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(put.headers()["x-oss-forbid-overwrite"], "true");
        assert_eq!(put.headers()["x-oss-storage-class"], "Standard");
        assert!(put.headers()["authorization"]
            .to_str()
            .unwrap()
            .starts_with("OSS4-HMAC-SHA256 Credential=fixture/"));
        let policy = session_policy(&config(), &[prefix], true).unwrap();
        assert!(
            !policy.contains("DeleteObject")
                && !policy.contains("SetBucket")
                && !policy.contains("research/*")
        );
        oss.session.expires_ms = Utc::now().timestamp_millis() - 1;
        assert!(oss.request(reqwest::Method::GET, &key, None).is_err());
        let mut empty = config();
        empty.publication_namespaces.clear();
        assert!(empty.validate().is_err());
        let mut invalid = config();
        invalid.endpoint = "https://foreign/".into();
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn release_oss_v4_key_matches_independent_hmac_vector() {
        let key = hmac(b"aliyun_v4yourAccessKeySecret", "20250411");
        let key = hmac(&key, "cn-hangzhou");
        let key = hmac(&key, "oss");
        let key = hmac(&key, "aliyun_v4_request");
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "8a01ff4efcc65ca2cbc75375045c61ab5f3fa8b9a2d84f0add27ef16a25feb3c"
        );
    }
}
