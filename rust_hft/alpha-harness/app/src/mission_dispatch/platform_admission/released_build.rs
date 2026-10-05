//! Signature and actual-byte readback are separate gates. Only this private
//! constructor joins them into the software input used by the native producer.
use anyhow::{ensure, Context};
use hft_research_platform::{
    build::BuildArtifact,
    release::{BuildReleaseTrust, SignedBuildRelease, VerifiedBuildRelease},
};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read};

pub(super) struct ReadbackBuildRelease {
    release: VerifiedBuildRelease,
}
impl ReadbackBuildRelease {
    pub(super) fn release(&self) -> &VerifiedBuildRelease {
        &self.release
    }
}

pub(super) fn verify_and_readback(
    trust: &BuildReleaseTrust,
    artifact: &BuildArtifact,
    signed: &SignedBuildRelease,
    urls: &BTreeMap<String, String>,
    client: &Client,
) -> anyhow::Result<ReadbackBuildRelease> {
    let release = trust.verify(artifact, signed)?;
    let expected = std::iter::once(&release.signed().receipt.source.archive)
        .chain(
            release
                .artifact()
                .executables
                .iter()
                .map(|executable| &executable.blob),
        )
        .collect::<Vec<_>>();
    ensure!(
        urls.len() == expected.len()
            && expected.iter().all(|object| urls.contains_key(&object.key)),
        "independent release readback requires exact source and executable coverage"
    );
    for object in expected {
        let url = urls
            .get(&object.key)
            .context("release readback URL is absent")?;
        let parsed = reqwest::Url::parse(url)?;
        ensure!(
            parsed.scheme() == "https"
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.fragment().is_none(),
            "release readback requires authenticated HTTPS object transport"
        );
        let response = client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(reqwest::Error::without_url)?;
        verify_bytes(object, response)?;
    }
    Ok(ReadbackBuildRelease { release })
}

fn verify_bytes(
    object: &hft_research_platform::orchestrator::Artifact,
    mut read: impl Read,
) -> anyhow::Result<()> {
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = read.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("release object size overflow")?;
        ensure!(
            bytes <= object.bytes,
            "released object exceeds its signed bound"
        );
        hash.update(&buffer[..count]);
    }
    ensure!(
        bytes == object.bytes && hex::encode(hash.finalize()) == object.sha256,
        "released source or executable bytes differ from trusted publication"
    );
    Ok(())
}

#[cfg(test)]
pub(super) fn from_test_readback_peer(
    trust: &BuildReleaseTrust,
    artifact: &BuildArtifact,
    signed: &SignedBuildRelease,
    objects: &BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<ReadbackBuildRelease> {
    let release = trust.verify(artifact, signed)?;
    ensure!(
        objects.len() == release.artifact().executables.len() + 1,
        "test peer changed release coverage"
    );
    for object in std::iter::once(&release.signed().receipt.source.archive)
        .chain(release.artifact().executables.iter().map(|e| &e.blob))
    {
        verify_bytes(
            object,
            std::io::Cursor::new(
                objects
                    .get(&object.key)
                    .context("test peer lacks release bytes")?,
            ),
        )?;
    }
    Ok(ReadbackBuildRelease { release })
}

pub(super) fn public_keys(trust: &BuildReleaseTrust) -> anyhow::Result<Vec<[u8; 32]>> {
    trust
        .keys
        .values()
        .map(|encoded| {
            let key: [u8; 32] = hex::decode(encoded)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid release public key length"))?;
            Ok(key)
        })
        .collect()
}
