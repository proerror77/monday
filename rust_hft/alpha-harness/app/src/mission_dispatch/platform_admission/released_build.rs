//! Signature and actual-byte readback are separate gates. Only this private
//! constructor joins them into the software input used by the native producer.
use anyhow::{ensure, Context};
use hft_research_platform::{
    build::BuildArtifact,
    release::{BuildReleaseTrust, SignedBuildRelease, VerifiedBuildRelease},
};
use reqwest::blocking::Client;
use std::{collections::BTreeMap, path::Path};

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
    directory: &Path,
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
    for (index, object) in expected.into_iter().enumerate() {
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
        let path = directory.join(format!("released-object-{index}"));
        let (bytes, sha256) =
            hft_research_artifacts::fetch_to_file(client, url, &path, object.bytes)?;
        ensure!(
            bytes == object.bytes && sha256 == object.sha256,
            "released source or executable bytes differ from trusted publication"
        );
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
