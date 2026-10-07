//! Release-only OSS transport. Scientific AttemptWriter remains PG fenced.
use anyhow::{ensure, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OssConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub role_arn: String,
    pub oidc_provider_arn: String,
    pub audience: String,
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
                && self.owner_id > 0,
            "approved RAM OIDC configuration required"
        );
        Ok(())
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
}
impl Session {
    pub fn validate(&self, now: i64) -> Result<()> {
        ensure!(
            self.expires_ms > now && self.expires_ms <= now + 900_000,
            "OSS session expired or exceeds fifteen minutes"
        );
        ensure!(
            [
                &self.access_key_id,
                &self.access_key_secret,
                &self.security_token
            ]
            .iter()
            .all(|s| !s.is_empty() && s.len() <= 16_384 && s.bytes().all(|b| b.is_ascii_graphic())),
            "invalid OSS session"
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
fn exact_prefix(p: &str) -> bool {
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
}
impl Oss {
    pub fn from_file(config: &OssConfig, path: &Path) -> Result<Self> {
        config.validate()?;
        let session: Session = serde_json::from_slice(&crate::transport::read_private_file(path)?)
            .map_err(|_| anyhow::anyhow!("invalid private OSS session file"))?;
        session.validate(Utc::now().timestamp_millis())?;
        Ok(Self {
            config: config.clone(),
            session,
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
    pub async fn get(&self, key: &str, version: Option<&str>) -> Result<reqwest::Response> {
        self.request(
            reqwest::Method::GET,
            key,
            version.or_else(|| self.session.versions.get(key).map(String::as_str)),
        )?
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("OSS read unavailable"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("OSS evidence missing or rejected"))
    }
    async fn require_unversioned_bucket(&self) -> Result<()> {
        let mut response = self
            .request(reqwest::Method::GET, "", None)?
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS versioning read unavailable"))?
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("OSS versioning read denied"))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("OSS versioning read interrupted"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 4096,
                "OSS versioning response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        require_unversioned_response(&bytes)
    }
    pub async fn put(&self, key: &str, body: reqwest::Body, size: u64) -> Result<()> {
        self.require_unversioned_bucket().await?;
        let response = self
            .request(reqwest::Method::PUT, key, None)?
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS upload unavailable"))?;
        // A conflict does not prove equality. The caller always performs a GET.
        ensure!(
            response.status().is_success() || response.status() == reqwest::StatusCode::CONFLICT,
            "OSS upload rejected"
        );
        Ok(())
    }
    pub async fn check(&self, source: &str) -> Result<()> {
        self.require_unversioned_bucket().await?;
        let response = self
            .request(
                reqwest::Method::HEAD,
                &format!("research/sources/{source}/source.tar"),
                None,
            )?
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OSS TLS unavailable"))?;
        ensure!(
            response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
            "OSS authenticated preflight rejected"
        );
        Ok(())
    }
}

fn require_unversioned_response(bytes: &[u8]) -> Result<()> {
    let mut text = std::str::from_utf8(bytes)?.trim();
    if text.starts_with("<?xml ") {
        text = text
            .split_once("?>")
            .map(|(_, s)| s.trim())
            .unwrap_or_default();
    }
    let root = text
        .strip_prefix("<VersioningConfiguration")
        .unwrap_or_default();
    let root_name_ended =
        root.starts_with('>') || root.starts_with('/') || root.starts_with(char::is_whitespace);
    let empty = root.split_once('>').is_some_and(|(tag, tail)| {
        if tag.trim_end().ends_with('/') {
            tail.trim().is_empty()
        } else {
            tail.trim() == "</VersioningConfiguration>"
        }
    });
    ensure!(root_name_ended && empty && !text.contains("Status") && !text.contains("Enabled") && !text.contains("Suspended"),
        "OSS overwrite refusal requires a never-versioned bucket; do not change bucket settings automatically");
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
            prefixes: vec![format!("research/builds/{}/", "a".repeat(64))],
        };
        assert!(session.validate(1000).is_ok());
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
    fn config() -> OssConfig {
        OssConfig {
            bucket: "test-bucket".into(),
            region: "cn-hangzhou".into(),
            endpoint: "https://test-bucket.oss-cn-hangzhou.aliyuncs.com/".into(),
            role_arn: "acs:ram::123:role/test".into(),
            oidc_provider_arn: "acs:ram::123:oidc-provider/test".into(),
            audience: "test".into(),
            repository_id: 1,
            owner_id: 2,
        }
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
            },
            client: reqwest::Client::new(),
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
