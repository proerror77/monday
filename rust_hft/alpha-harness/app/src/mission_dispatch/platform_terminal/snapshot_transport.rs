//! Read-only PG facts come from the actual released host observer. A JSON path
//! or a worker-supplied digest cannot manufacture this private transport value.
use anyhow::{ensure, Context};
use hft_research_platform::{orchestrator::Artifact, release::VerifiedBuildRelease};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) struct ReadonlySnapshotBytes {
    bytes: Vec<u8>,
    observer_release_sha256: String,
}
impl ReadonlySnapshotBytes {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(super) fn observer_release_sha256(&self) -> &str {
        &self.observer_release_sha256
    }
}

pub(super) fn read(
    binary: &Path,
    release: &VerifiedBuildRelease,
    tenant: &str,
    request: &str,
) -> anyhow::Result<ReadonlySnapshotBytes> {
    let database = std::env::var_os("MONDAY_RESEARCH_DATABASE_URL")
        .context("operator readonly PG transport is absent")?;
    read_with_database(binary, release, tenant, request, &database)
}

fn read_with_database(
    binary: &Path,
    release: &VerifiedBuildRelease,
    tenant: &str,
    request: &str,
    database: &std::ffi::OsStr,
) -> anyhow::Result<ReadonlySnapshotBytes> {
    release
        .artifact()
        .admits_command(&["/usr/local/bin/researchctl".into()])?;
    ensure!(
        !tenant.is_empty()
            && tenant.len() <= 128
            && tenant.trim() == tenant
            && request.len() == 64
            && request
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
        "invalid original native terminal snapshot scope"
    );
    let executable = release
        .artifact()
        .executables
        .iter()
        .find(|v| v.name == "researchctl")
        .context("released readonly observer is absent")?;
    let directory = tempfile::tempdir()?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let directory_path = directory.path().canonicalize()?;
    let pinned = directory_path.join("researchctl");
    // Copy and verify one open regular source into owned private storage. The
    // command executes that exact retained byte sequence, avoiding path races.
    retain_executable(binary, &pinned, &executable.blob)?;
    let output = run(&pinned, &directory_path, tenant, request, database)?;
    Ok(ReadonlySnapshotBytes {
        bytes: output,
        observer_release_sha256: release.artifact().id()?,
    })
}

fn retain_executable(source: &Path, target: &Path, expected: &Artifact) -> anyhow::Result<()> {
    use rustix::fs::{open, Mode, OFlags};
    ensure!(
        source.is_absolute()
            && source
                .parent()
                .context("observer binary parent is absent")?
                .canonicalize()?
                == source.parent().unwrap(),
        "observer binary path must be absolute and canonical"
    );
    let mut input = File::from(open(
        source,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = input.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() == expected.bytes
            && expected.bytes > 0
            && expected.bytes <= 512 * 1024 * 1024,
        "observer binary differs from bounded released executable"
    );
    let mut retained = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut hash = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        copied = copied
            .checked_add(n as u64)
            .context("observer executable size overflow")?;
        ensure!(
            copied <= expected.bytes,
            "observer executable grew during readback"
        );
        hash.update(&buffer[..n]);
        retained.write_all(&buffer[..n])?;
    }
    ensure!(
        copied == expected.bytes && hex::encode(hash.finalize()) == expected.sha256,
        "observer executable actual bytes changed"
    );
    retained.sync_all()?;
    retained.set_permissions(std::fs::Permissions::from_mode(0o500))?;
    Ok(())
}

fn run(
    binary: &Path,
    directory: &Path,
    tenant: &str,
    request: &str,
    database: &std::ffi::OsStr,
) -> anyhow::Result<Vec<u8>> {
    let output = tempfile::NamedTempFile::new_in(directory)?;
    let child = Command::new(binary)
        .args(["terminal-snapshot", tenant, request])
        .env_clear()
        .env("MONDAY_RESEARCH_DATABASE_URL", database)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.reopen()?))
        // Neither credentials nor arbitrary database diagnostics enter logs.
        .stderr(Stdio::null())
        .spawn()
        .context("start exact released readonly terminal observer")?;
    struct OwnedObserver(std::process::Child);
    impl Drop for OwnedObserver {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = OwnedObserver(child);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(60) {
            anyhow::bail!("readonly terminal snapshot deadline exceeded");
        }
        ensure!(
            output.as_file().metadata()?.len() <= 1024 * 1024,
            "readonly snapshot output exceeded its bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    ensure!(
        status.success(),
        "released readonly terminal observer rejected the exact request"
    );
    const LIMIT: u64 = 1024 * 1024;
    let metadata = output.as_file().metadata()?;
    ensure!(
        metadata.len() > 0 && metadata.len() <= LIMIT,
        "terminal snapshot facts exceed bound"
    );
    let mut bytes = Vec::new();
    output.reopen()?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "terminal snapshot grew beyond bound"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(bytes: &[u8]) -> VerifiedBuildRelease {
        use ed25519_dalek::{Signer, SigningKey};
        use hft_research_platform::{build::*, release::*};
        let sha = |c: char| c.to_string().repeat(64);
        let source = SourceArchive {
            schema: 1,
            code_commit: "a".repeat(40),
            archive: Artifact {
                key: format!("research/sources/{}/source.tar", "a".repeat(40)),
                sha256: sha('a'),
                bytes: 1,
            },
        };
        let build = BuildSpec {
            schema: 2,
            code_commit: source.code_commit.clone(),
            workspace_manifest: "research-core/Cargo.toml".into(),
            source_manifest_sha256: hft_research_platform::identity(&source).unwrap(),
            cargo_lock_sha256: sha('b'),
            toolchain_manifest_sha256: sha('c'),
            target: "x86_64-unknown-linux-gnu".into(),
            packages: vec!["hft-research-platform".into()],
            binaries: vec!["researchctl".into()],
            features: vec!["control".into()],
            default_features: false,
            profile: "research".into(),
            profile_manifest_sha256: sha('d'),
            rustflags_sha256: sha('e'),
            native_environment_sha256: sha('f'),
            builder_image: format!("builder@sha256:{}", sha('0')),
        };
        let executable = BuiltExecutable {
            name: "researchctl".into(),
            blob: Artifact {
                key: format!("research/builds/{}/researchctl", build.id().unwrap()),
                sha256: hft_research_platform::sha256(bytes),
                bytes: bytes.len() as u64,
            },
        };
        let key = SigningKey::from_bytes(&[11; 32]);
        let mut signed = SignedBuildRelease {
            schema: 1,
            key_id: "fixture-release".into(),
            signature_hex: String::new(),
            receipt: BuildReleaseReceipt {
                schema: 1,
                build_sha256: build.id().unwrap(),
                source,
                image: format!("observer@sha256:{}", sha('1')),
                target: build.target.clone(),
                executables: vec![executable.clone()],
                producer: ReleaseProducer {
                    repository: "fixture/monday".into(),
                    workflow_path: ".github/workflows/fixture.yml".into(),
                    source_sha: build.code_commit.clone(),
                    run_id: 1,
                    run_attempt: 1,
                    job_id: 1,
                },
                publication_readback_sha256: sha('2'),
            },
        };
        signed.signature_hex = hex::encode(key.sign(&signed.signing_bytes().unwrap()).to_bytes());
        let artifact = BuildArtifact {
            schema: 1,
            build,
            image: signed.receipt.image.clone(),
            executables: vec![executable],
            release_receipt_sha256: hft_research_platform::identity(&signed).unwrap(),
        };
        BuildReleaseTrust {
            schema: 1,
            repository: "fixture/monday".into(),
            producer_workflow_path: ".github/workflows/fixture.yml".into(),
            keys: [(
                "fixture-release".into(),
                hex::encode(key.verifying_key().as_bytes()),
            )]
            .into(),
        }
        .verify(&artifact, &signed)
        .unwrap()
    }
    #[test]
    fn exact_retained_observer_bytes_and_request_survive_source_path_change() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("source-observer");
        let pinned = directory.path().join("researchctl");
        let bytes = b"#!/bin/sh\n[ \"$1\" = terminal-snapshot ] || exit 2\n[ \"$2\" = native-fixture ] || exit 3\n[ \"$MONDAY_RESEARCH_DATABASE_URL\" = fixture-only ] || exit 4\nprintf '{\"request\":\"%s\"}' \"$3\"\n";
        std::fs::write(&source, bytes).unwrap();
        let released = release(bytes);
        let actual = read_with_database(
            &source,
            &released,
            "native-fixture",
            &"a".repeat(64),
            std::ffi::OsStr::new("fixture-only"),
        )
        .unwrap();
        assert_eq!(
            actual.observer_release_sha256(),
            released.artifact().id().unwrap()
        );
        assert!(read(
            &directory.path().join("absent"),
            &released,
            "native-fixture",
            &"a".repeat(64)
        )
        .is_err());
        let artifact = Artifact {
            key: "fixture/researchctl".into(),
            sha256: hft_research_platform::sha256(bytes),
            bytes: bytes.len() as u64,
        };
        retain_executable(&source, &pinned, &artifact).unwrap();
        std::fs::write(&source, b"changed caller path").unwrap();
        let body = run(
            &pinned,
            directory.path(),
            "native-fixture",
            &"a".repeat(64),
            std::ffi::OsStr::new("fixture-only"),
        )
        .unwrap();
        let fact = ReadonlySnapshotBytes {
            bytes: body,
            observer_release_sha256: "b".repeat(64),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(fact.bytes()).unwrap()["request"],
            "a".repeat(64)
        );
        assert_eq!(fact.observer_release_sha256(), "b".repeat(64));
        assert!(retain_executable(&source, &directory.path().join("changed"), &artifact).is_err());
    }
}
