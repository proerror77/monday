//! Builds contain compiler inputs, never scientific parameters or Attempt state.
//! A writable Cargo cache is an optimization, not an executable or admission.
use crate::{identity, orchestrator::Artifact, sha256, valid_digest};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildSpec {
    pub schema: u32,
    pub code_commit: String,
    pub source_manifest_sha256: String,
    pub cargo_lock_sha256: String,
    /// Includes the pinned compiler distribution and rustc -Vv, not just a label.
    pub toolchain_manifest_sha256: String,
    pub target: String,
    pub packages: Vec<String>,
    pub binaries: Vec<String>,
    pub features: Vec<String>,
    pub default_features: bool,
    pub profile: String,
    pub profile_manifest_sha256: String,
    pub rustflags_sha256: String,
    /// Native compiler/linker, headers, system libraries and relevant build env.
    pub native_environment_sha256: String,
    pub builder_image: String,
}
fn sorted_names(values: &[String], required: bool) -> bool {
    (!required || !values.is_empty())
        && values.len() <= 64
        && values.windows(2).all(|v| v[0] < v[1])
        && values.iter().all(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-./".contains(&c))
        })
}
pub fn pinned_image(value: &str) -> bool {
    value.rsplit_once("@sha256:").is_some_and(|(name, hash)| {
        !name.is_empty() && !name.chars().any(char::is_whitespace) && valid_digest(hash)
    })
}
impl BuildSpec {
    pub fn id(&self) -> Result<String> {
        ensure!(
            self.schema == 1
                && self.code_commit.len() == 40
                && self
                    .code_commit
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                && [
                    &self.source_manifest_sha256,
                    &self.cargo_lock_sha256,
                    &self.toolchain_manifest_sha256,
                    &self.profile_manifest_sha256,
                    &self.rustflags_sha256,
                    &self.native_environment_sha256
                ]
                .into_iter()
                .all(|s| valid_digest(s)),
            "incomplete build input identity"
        );
        ensure!(
            matches!(
                self.target.as_str(),
                "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
            ) && sorted_names(&self.packages, true)
                && sorted_names(&self.binaries, true)
                && self.binaries.iter().all(|s| s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)))
                && sorted_names(&self.features, false)
                && matches!(self.profile.as_str(), "release" | "research")
                && pinned_image(&self.builder_image),
            "unbounded/unpinned build selection"
        );
        identity(self)
    }
    /// Exact scoped build; no implicit --workspace, --all-features or Cargo at Run time.
    pub fn cargo_arguments(&self) -> Result<Vec<String>> {
        self.id()?;
        let mut args = vec![
            "build".into(),
            "--locked".into(),
            "--target".into(),
            self.target.clone(),
            "--profile".into(),
            self.profile.clone(),
        ];
        for package in &self.packages {
            args.extend(["-p".into(), package.clone()]);
        }
        for binary in &self.binaries {
            args.extend(["--bin".into(), binary.clone()]);
        }
        if !self.default_features {
            args.push("--no-default-features".into());
        }
        if !self.features.is_empty() {
            args.extend(["--features".into(), self.features.join(",")]);
        }
        Ok(args)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuiltExecutable {
    pub name: String,
    pub blob: Artifact,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildArtifact {
    pub schema: u32,
    pub build: BuildSpec,
    pub image: String,
    pub executables: Vec<BuiltExecutable>,
    /// Trusted release verifier binds OCI digest, contained bytes and source.
    pub release_receipt_sha256: String,
}
impl BuildArtifact {
    pub fn id(&self) -> Result<String> {
        let build_id = self.build.id()?;
        ensure!(
            self.schema == 1
                && pinned_image(&self.image)
                && valid_digest(&self.release_receipt_sha256)
                && self.executables.len() == self.build.binaries.len(),
            "incomplete executable release"
        );
        for (expected, executable) in self.build.binaries.iter().zip(&self.executables) {
            ensure!(
                expected == &executable.name
                    && executable.blob.key == format!("research/builds/{build_id}/{expected}")
                    && valid_digest(&executable.blob.sha256)
                    && executable.blob.bytes > 0
                    && executable.blob.bytes <= 512 * 1024 * 1024,
                "executable coverage/identity mismatch"
            );
        }
        identity(self)
    }
    pub fn admits_command(&self, command: &[String]) -> Result<()> {
        self.id()?;
        ensure!(
            command.first().is_some_and(|cmd| self
                .executables
                .iter()
                .any(|e| cmd == &format!("/usr/local/bin/{}", e.name))),
            "Run must invoke a released executable directly"
        );
        Ok(())
    }
    /// Used for bounded local/import checks. The controller uses streaming
    /// readback before every launch, including an infrastructure retry.
    pub fn verify_bytes(&self, mut read: impl FnMut(&Artifact) -> Result<Vec<u8>>) -> Result<()> {
        self.id()?;
        for executable in &self.executables {
            let bytes = read(&executable.blob)?;
            ensure!(
                bytes.len() as u64 == executable.blob.bytes
                    && sha256(&bytes) == executable.blob.sha256,
                "released executable missing or changed"
            );
        }
        Ok(())
    }
}
