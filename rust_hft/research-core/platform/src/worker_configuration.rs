//! Fixed-path private staging for the admitted scientific worker's init container.
use crate::orchestrator::{worker_configuration_reference, AttemptContext, TaskKind};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};

const STATIC_KEYS: [&str; 4] = [
    "campaign.json",
    "artifact-io.json",
    "ca.pem",
    "native-trust.json",
];
const LATE_KEYS: [&str; 3] = ["artifact.token", "tls.pem", "native-admission.json"];

/// The source mount is read-only and contains an immutable, independently
/// checked Kubernetes Secret. Projected links resolve only inside that mount.
/// The helper has no network, database, issuance, or scientific execution path.
pub fn stage_configuration(context: &AttemptContext) -> Result<()> {
    stage_at(
        context,
        Path::new("/configuration-inputs"),
        Path::new("/identity-inputs"),
        Path::new("/private-state"),
    )
}

fn mounted_file(root: &Path, name: &str, bound: u64) -> Result<Vec<u8>> {
    use rustix::fs::{open, Mode, OFlags};
    let root = root.canonicalize()?;
    let target = root.join(name).canonicalize()?;
    ensure!(
        target.starts_with(&root),
        "projected configuration escaped its mount"
    );
    let file = std::fs::File::from(open(
        &target,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= bound,
        "projected configuration must be bounded regular data"
    );
    let mut bytes = Vec::new();
    file.take(bound + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= bound,
        "projected configuration changed during read"
    );
    Ok(bytes)
}

fn private_directory(root: &Path, name: &str) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let root = root.canonicalize()?;
    let target = root.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&target)?;
    ensure!(
        target.canonicalize()? == target,
        "private staging directory changed"
    );
    Ok(target)
}

fn install(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    use rustix::fs::{open, Mode, OFlags};
    let mut file = std::fs::File::from(open(
        directory.join(name),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn stage_at(
    context: &AttemptContext,
    configuration: &Path,
    identities: &Path,
    destination: &Path,
) -> Result<()> {
    context.validate()?;
    ensure!(
        context.spec.kind == TaskKind::CexCampaign,
        "private staging is limited to the fixed native Campaign worker"
    );
    let expected = context
        .spec
        .worker_configuration
        .as_ref()
        .context("verified configuration identity required")?;
    let mut raw = BTreeMap::new();
    let mut encoded = BTreeMap::new();
    for name in STATIC_KEYS {
        if configuration.join(name).symlink_metadata().is_err() {
            continue;
        }
        let bytes = mounted_file(configuration, name, 1024 * 1024)?;
        encoded.insert(name.to_owned(), STANDARD.encode(&bytes));
        raw.insert(name, bytes);
    }
    let actual = worker_configuration_reference(
        &context.spec.profile.namespace,
        &expected.secret_name,
        &expected.secret_uid,
        &encoded,
    )?;
    ensure!(
        actual == *expected,
        "projected configuration differs from signed bytes"
    );
    let mut late = BTreeMap::new();
    for name in LATE_KEYS {
        if identities.join(name).symlink_metadata().is_err() {
            continue;
        }
        late.insert(name, mounted_file(identities, name, 64 * 1024)?);
    }
    ensure!(
        late.contains_key("artifact.token") && late.contains_key("native-admission.json"),
        "controlled Attempt identity is incomplete"
    );
    let config_dir = private_directory(destination, "config")?;
    let identity_dir = private_directory(destination, "identity")?;
    for (name, bytes) in raw {
        install(&config_dir, name, &bytes)?;
    }
    for (name, bytes) in late {
        install(&identity_dir, name, &bytes)?;
    }
    std::fs::File::open(config_dir)?.sync_all()?;
    std::fs::File::open(identity_dir)?.sync_all()?;
    std::fs::File::open(destination)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::{Backend, Profile},
        orchestrator::{Task, TaskSpec},
    };
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn projected_static_bytes_stage_private_without_token_identity_cycle() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let source = temporary.path().join("source");
        let late = temporary.path().join("late");
        let destination = temporary.path().join("destination");
        for path in [&source, &late, &destination] {
            std::fs::create_dir(path)?;
        }
        let destination = destination.canonicalize()?;
        let epoch = source.join("epoch");
        std::fs::create_dir(&epoch)?;
        let mut map = BTreeMap::new();
        for name in ["campaign.json", "artifact-io.json"] {
            let bytes = format!("{{\"synthetic_staging\":\"{name}\"}}").into_bytes();
            std::fs::write(epoch.join(name), &bytes)?;
            symlink(epoch.join(name), source.join(name))?;
            map.insert(name.to_owned(), STANDARD.encode(bytes));
        }
        std::fs::write(late.join("artifact.token"), "synthetic-attempt-token")?;
        std::fs::write(
            late.join("native-admission.json"),
            "synthetic-staging-only-not-authority",
        )?;
        let reference =
            worker_configuration_reference("research", "configuration", "uid-fixture", &map)?;
        let spec = TaskSpec {
            schema: 1,
            kind: TaskKind::CexCampaign,
            run_manifest_sha256: "a".repeat(64),
            view_manifest_sha256: "b".repeat(64),
            source_sha256: "c".repeat(64),
            image: format!("fixture@sha256:{}", "d".repeat(64)),
            command: vec!["/app/worker".into()],
            profile: Profile {
                backend: Backend::KubernetesJob,
                cluster: "fixture".into(),
                namespace: "research".into(),
                service_account: "worker".into(),
                architecture: "amd64".into(),
                cpu_millis: 1000,
                memory_mib: 128,
                scratch_mib: 64,
                gpu: 0,
                acceptance_sha256: "e".repeat(64),
                prepared_pvc: None,
                worker_secret: Some("configuration".into()),
            },
            timeout_ms: 1000,
            max_attempts: 1,
            output_prefix: "research/results".into(),
            fit_identity_sha256: None,
            worker_configuration: Some(reference.clone()),
        };
        let mut task = Task::new(spec.clone())?;
        let context = AttemptContext {
            spec,
            lease: task.claim("fixture-owner", 1000, 1000)?,
        };
        stage_at(&context, &source, &late, &destination)?;
        assert_eq!(
            std::fs::metadata(destination.join("config"))?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(destination.join("identity/artifact.token"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            crate::transport::read_private_file(&destination.join("identity/artifact.token"))?,
            b"synthetic-attempt-token"
        );
        let escaped = temporary.path().join("escaped");
        std::fs::create_dir(&escaped)?;
        std::fs::remove_file(source.join("campaign.json"))?;
        symlink(late.join("artifact.token"), source.join("campaign.json"))?;
        assert!(stage_at(&context, &source, &late, &escaped).is_err());
        assert!(!escaped.join("config").exists());
        map.insert(
            "artifact.token".into(),
            STANDARD.encode("must-not-enter-signed-static-inputs"),
        );
        assert!(
            worker_configuration_reference("research", "configuration", "uid-fixture", &map)
                .is_err()
        );
        Ok(())
    }
}
