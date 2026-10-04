//! Bounded HTTPS acquisition stays outside the pure scientific input crate.
use crate::{sha256, valid_digest};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

/// This descriptor is supplied by the trusted publisher/identity broker. No
/// endpoint or signed credential enters the scientific DataView identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectDescriptor {
    pub sha256: String,
    pub bytes: u64,
    pub url: String,
}

pub struct Objects {
    client: reqwest::Client,
}

impl Objects {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    pub async fn read(&self, descriptor: &ObjectDescriptor, max_bytes: u64) -> Result<Vec<u8>> {
        ensure!(
            valid_digest(&descriptor.sha256)
                && descriptor.bytes > 0
                && descriptor.bytes <= max_bytes,
            "invalid artifact descriptor"
        );
        let url = reqwest::Url::parse(&descriptor.url)?;
        ensure!(
            url.scheme() == "https" && url.username().is_empty() && url.password().is_none(),
            "artifact requires HTTPS"
        );
        // Do not propagate transport errors containing signed URLs into logs.
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("artifact request failed"))?
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("artifact request rejected"))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("artifact read interrupted"))?
        {
            ensure!(
                bytes.len() as u64 + chunk.len() as u64 <= descriptor.bytes,
                "artifact exceeds descriptor"
            );
            bytes.extend_from_slice(&chunk);
        }
        ensure!(
            bytes.len() as u64 == descriptor.bytes && sha256(&bytes) == descriptor.sha256,
            "artifact checksum mismatch"
        );
        Ok(bytes)
    }
}
