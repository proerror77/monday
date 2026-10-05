//! Scoped immutable artifact transport shared by scientific and preparation workers.
use crate::{orchestrator::Artifact, sha256};
use anyhow::{ensure, Context, Result};

/// The gateway must enforce per-attempt write identity and immutable object keys.
/// No bucket-wide key, signing key, deployment or database write key is needed.
pub struct Writer {
    client: reqwest::Client,
    base: reqwest::Url,
    token: String,
    prefix: String,
}
impl Writer {
    pub fn new(
        endpoint: &str,
        token: String,
        context: &crate::orchestrator::AttemptContext,
    ) -> Result<Self> {
        Self::with_tls(
            endpoint,
            token,
            context,
            &crate::transport::TlsConfig::default(),
        )
    }
    pub fn with_tls(
        endpoint: &str,
        token: String,
        context: &crate::orchestrator::AttemptContext,
        tls: &crate::transport::TlsConfig,
    ) -> Result<Self> {
        context.validate()?;
        let base = reqwest::Url::parse(endpoint)?;
        ensure!(
            base.scheme() == "https"
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none()
                && base.host_str().is_some()
                && base.path().ends_with('/')
                && !token.is_empty(),
            "invalid worker artifact identity"
        );
        Ok(Self {
            client: tls.client(std::time::Duration::from_secs(30), true)?,
            base,
            token,
            prefix: format!(
                "{}/{}/{}/",
                context.spec.output_prefix, context.lease.task_id, context.lease.attempt
            ),
        })
    }
    pub async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<Artifact> {
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && !name.contains("..")
                && !bytes.is_empty()
                && bytes.len() <= 16 * 1024 * 1024,
            "invalid worker output"
        );
        let artifact = Artifact {
            key: format!("{}{name}", self.prefix),
            sha256: sha256(&bytes),
            bytes: bytes.len() as u64,
        };
        let url = self.base.join(&artifact.key)?;
        let response = self
            .client
            .put(url.clone())
            .bearer_auth(&self.token)
            .header("If-None-Match", "*")
            .body(bytes)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("artifact upload unavailable"))?;
        if response.status() != reqwest::StatusCode::CONFLICT
            && response.status() != reqwest::StatusCode::PRECONDITION_FAILED
        {
            response
                .error_for_status()
                .map_err(|_| anyhow::anyhow!("artifact upload rejected"))?;
        }
        self.readback(&artifact).await?;
        Ok(artifact)
    }

    /// Upload a bounded regular file from one descriptor. The existing native
    /// archive identity is verified by an independent HTTPS readback. No
    /// second local SHA read or full archive allocation is needed.
    pub async fn put_file(
        &self,
        name: &str,
        path: &std::path::Path,
        expected_sha256: &str,
    ) -> Result<Artifact> {
        use rustix::fs::{open, Mode, OFlags};
        ensure!(
            !name.is_empty()
                && !name.contains("..")
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && crate::valid_digest(expected_sha256),
            "invalid scientific archive identity"
        );
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        let file = std::fs::File::from(fd);
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() > 0 && metadata.len() <= 512 * 1024 * 1024,
            "scientific archive exceeds gateway bound"
        );
        let artifact = Artifact {
            key: format!("{}{name}", self.prefix),
            sha256: expected_sha256.to_owned(),
            bytes: metadata.len(),
        };
        let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
        let response = self
            .client
            .put(self.base.join(&artifact.key)?)
            .bearer_auth(&self.token)
            .header("If-None-Match", "*")
            .header(reqwest::header::CONTENT_LENGTH, artifact.bytes)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("scientific archive upload unavailable"))?;
        if response.status() != reqwest::StatusCode::CONFLICT
            && response.status() != reqwest::StatusCode::PRECONDITION_FAILED
        {
            response
                .error_for_status()
                .map_err(|_| anyhow::anyhow!("scientific archive upload rejected"))?;
        }
        self.readback(&artifact).await?;
        Ok(artifact)
    }

    /// Bytes must match the native identity even after an ambiguous PUT. PG's
    /// reconciler later performs a separate readback before terminal acceptance.
    pub async fn readback(&self, artifact: &Artifact) -> Result<()> {
        use sha2::{Digest, Sha256};
        ensure!(
            artifact.key.starts_with(&self.prefix)
                && !artifact.key.contains("..")
                && artifact
                    .key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
                && crate::valid_digest(&artifact.sha256)
                && artifact.bytes > 0
                && artifact.bytes <= 512 * 1024 * 1024,
            "foreign or unbounded artifact readback"
        );
        let mut response = self
            .client
            .get(self.base.join(&artifact.key)?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("artifact readback unavailable"))?
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("artifact readback rejected"))?;
        let mut hash = Sha256::new();
        let mut count = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("artifact readback interrupted"))?
        {
            count = count
                .checked_add(chunk.len() as u64)
                .context("artifact size overflow")?;
            ensure!(count <= artifact.bytes, "conflicting output size");
            hash.update(chunk);
        }
        ensure!(
            count == artifact.bytes && format!("{:x}", hash.finalize()) == artifact.sha256,
            "conflicting immutable output"
        );
        Ok(())
    }
}
