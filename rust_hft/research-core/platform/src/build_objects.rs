//! Immutable Build object reads and the shared scoped OSS signing transport.
//! Publication policy, budgets and writes belong to the publisher-only owner.
use anyhow::{ensure, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The object location grants no publication or research authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OssLocation {
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
}
impl OssLocation {
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
        Ok(())
    }
}

/// Publication-only budget identity. Read-only controller sessions reject it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BudgetReference {
    pub ledger_path: PathBuf,
    pub envelope_sha256: String,
    pub binding_sha256: String,
    pub device: u64,
    pub inode: u64,
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
    pub budget: Option<BudgetReference>,
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
pub(crate) struct SignedTransport {
    config: OssLocation,
    pub(crate) session: Session,
    client: reqwest::Client,
}
impl SignedTransport {
    pub(crate) fn from_file(
        config: &OssLocation,
        path: &Path,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        config.validate()?;
        let session: Session = serde_json::from_slice(&crate::transport::read_private_file(path)?)
            .map_err(|_| anyhow::anyhow!("invalid private OSS session file"))?;
        session.validate(Utc::now().timestamp_millis())?;
        Ok(Self {
            config: config.clone(),
            session,
            client: crate::transport::TlsConfig::default().client(timeout, true)?,
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
    pub(crate) fn require_reader(&self) -> Result<()> {
        ensure!(
            !self.session.publisher,
            "ACK import requires separate readonly OSS credentials"
        );
        Ok(())
    }
    #[cfg(feature = "publisher")]
    pub(crate) fn publication_request(
        &self,
        method: reqwest::Method,
        key: &str,
        version: Option<&str>,
    ) -> Result<reqwest::RequestBuilder> {
        self.request(method, key, version)
    }
    pub(crate) fn get_request(
        &self,
        key: &str,
        version: Option<&str>,
        limit: u64,
    ) -> Result<reqwest::RequestBuilder> {
        ensure!(
            limit > 0 && limit <= 512 * 1024 * 1024,
            "OSS read payload bound required"
        );
        self.request(
            reqwest::Method::GET,
            key,
            version.or_else(|| self.session.versions.get(key).map(String::as_str)),
        )
    }
    pub(crate) async fn read(
        request: reqwest::RequestBuilder,
        limit: u64,
    ) -> Result<crate::transport::BoundedBody> {
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
}

pub struct BuildReader {
    transport: SignedTransport,
}
impl BuildReader {
    /// Reload the private projection for each launch verification. Its owner can
    /// rotate short-lived credentials without restarting the reconciler.
    pub fn from_file(config: &OssLocation, path: &Path) -> Result<Self> {
        let transport =
            SignedTransport::from_file(config, path, std::time::Duration::from_secs(15))?;
        ensure!(
            !transport.session.publisher
                && transport.session.budget.is_none()
                && transport
                    .session
                    .prefixes
                    .iter()
                    .all(|prefix| prefix.starts_with("research/builds/")),
            "Build readback requires separate read-only OSS credentials"
        );
        Ok(Self { transport })
    }
    /// Read the same immutable software objects verified by the OSS importer.
    /// This does not inspect scientific result objects or authorize a launch.
    pub async fn verify_build(&self, build: &crate::build::BuildArtifact) -> Result<()> {
        use sha2::Digest;
        self.transport.require_reader()?;
        build.id()?;
        for executable in &build.executables {
            let artifact = &executable.blob;
            let mut response = SignedTransport::read(
                self.transport
                    .get_request(&artifact.key, None, artifact.bytes)?,
                artifact.bytes,
            )
            .await?;
            let mut hash = Sha256::new();
            let mut count = 0_u64;
            while let Some(chunk) = response.chunk().await? {
                count = count
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| anyhow::anyhow!("Build object size overflow"))?;
                hash.update(&chunk);
            }
            ensure!(
                count == artifact.bytes && format!("{:x}", hash.finalize()) == artifact.sha256,
                "Build object integrity failure"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod build_reader_tests {
    use super::*;
    use crate::build::{BuildArtifact, BuildSpec, BuiltExecutable};
    use crate::orchestrator::Artifact;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    fn release_oss_requests_bind_scope_role_version_and_overwrite_header() {
        let prefix = format!("research/builds/{}/", "a".repeat(64));
        let mut oss = SignedTransport {
            config: location(),
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
        #[cfg(feature = "publisher")]
        {
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
        }
        oss.session.expires_ms = Utc::now().timestamp_millis() - 1;
        assert!(oss.request(reqwest::Method::GET, &key, None).is_err());
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

    fn build() -> BuildArtifact {
        let digest = "a".repeat(64);
        let spec = BuildSpec {
            schema: 2,
            code_commit: "a".repeat(40),
            workspace_manifest: "research-core/Cargo.toml".into(),
            source_manifest_sha256: digest.clone(),
            cargo_lock_sha256: digest.clone(),
            toolchain_manifest_sha256: digest.clone(),
            target: "x86_64-unknown-linux-gnu".into(),
            packages: vec!["fixture".into()],
            binaries: vec!["worker".into()],
            features: vec![],
            default_features: false,
            profile: "release".into(),
            profile_manifest_sha256: digest.clone(),
            rustflags_sha256: digest.clone(),
            native_environment_sha256: digest.clone(),
            builder_image: format!("builder@sha256:{digest}"),
        };
        let key = format!("research/builds/{}/worker", spec.id().unwrap());
        BuildArtifact {
            schema: 1,
            build: spec,
            image: format!("worker@sha256:{digest}"),
            executables: vec![BuiltExecutable {
                name: "worker".into(),
                blob: Artifact {
                    key,
                    sha256: crate::sha256(b"code"),
                    bytes: 4,
                },
            }],
            release_receipt_sha256: digest,
        }
    }
    fn location() -> OssLocation {
        OssLocation {
            bucket: "fixture".into(),
            region: "ap-northeast-1".into(),
            endpoint: "https://fixture.oss-ap-northeast-1.aliyuncs.com/".into(),
        }
    }
    fn session(build: &BuildArtifact) -> Session {
        Session {
            access_key_id: "fixture-id".into(),
            access_key_secret: "fixture-key".into(),
            security_token: "fixture-token".into(),
            expires_ms: Utc::now().timestamp_millis() + 60_000,
            publisher: false,
            prefixes: vec![format!("research/builds/{}/", build.build.id().unwrap())],
            versions: BTreeMap::new(),
            budget: None,
        }
    }

    #[test]
    fn build_reader_requires_current_private_readonly_projection() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = dir.path().canonicalize()?.join("reader.json");
        let mut value = session(&build());
        let save = |value: &Session| -> Result<()> {
            std::fs::write(&path, serde_json::to_vec(value)?)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        };
        save(&value)?;
        let reader = BuildReader::from_file(&location(), &path)?;
        assert!(reader
            .transport
            .request(reqwest::Method::PUT, &build().executables[0].blob.key, None)
            .is_err());
        assert!(reader
            .transport
            .request(reqwest::Method::GET, "research/tasks/other/result", None)
            .is_err());
        value.prefixes = vec![format!("research/sources/{}/", "a".repeat(40))];
        save(&value)?;
        assert!(BuildReader::from_file(&location(), &path).is_err());
        value.prefixes = session(&build()).prefixes;
        value.publisher = true;
        save(&value)?;
        assert!(BuildReader::from_file(&location(), &path).is_err());
        value.publisher = false;
        value.budget = Some(BudgetReference {
            ledger_path: dir.path().join("not-a-runtime-budget"),
            envelope_sha256: "a".repeat(64),
            binding_sha256: "b".repeat(64),
            device: 0,
            inode: 0,
        });
        save(&value)?;
        assert!(BuildReader::from_file(&location(), &path).is_err());
        value.budget = None;
        value.expires_ms = Utc::now().timestamp_millis() - 1;
        save(&value)?;
        assert!(BuildReader::from_file(&location(), &path).is_err());
        value.expires_ms = Utc::now().timestamp_millis() + 60_000;
        save(&value)?;
        assert!(BuildReader::from_file(&location(), &path).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(BuildReader::from_file(&location(), &path).is_err());
        let mut wrong = location();
        wrong.endpoint = "https://foreign.example/".into();
        assert!(wrong.validate().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn build_readback_uses_scoped_oss_and_rejects_changed_or_missing_bytes() -> Result<()> {
        // A loopback OSS stand-in; no local artifact gateway is created or seeded.
        // Public constructors still admit only the bucket-bound HTTPS endpoint.
        for (wire, succeeds) in [
            ("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ncode", true),
            ("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nevil", false),
            ("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\ncodes", false),
            ("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ncut", false),
            ("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", false),
            ("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", false),
            ("HTTP/1.1 302 Found\r\nLocation: https://foreign.example/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let build = build();
            let expected = format!("GET /{} HTTP/1.1", build.executables[0].blob.key);
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 8192];
                let count = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert!(request.starts_with(&expected));
                assert!(request.to_ascii_lowercase().contains("authorization: oss4-hmac-sha256"));
                socket.write_all(wire.as_bytes()).await.unwrap();
            });
            let reader = BuildReader { transport: SignedTransport {
                config: OssLocation { endpoint: format!("http://{address}/"), ..location() },
                session: session(&build),
                client: crate::transport::TlsConfig::default().client(std::time::Duration::from_secs(2), false)?,
            }};
            assert_eq!(reader.verify_build(&build).await.is_ok(), succeeds);
            server.await?;
        }
        Ok(())
    }
}
