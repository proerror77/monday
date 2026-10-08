use anyhow::{ensure, Result};
use ed25519_dalek::{Signer, SigningKey};
use hft_research_platform::{
    build::BuildArtifact,
    identity,
    postgres::Ledger,
    release::{BuildReleaseTrust, SignedBuildRelease},
    release_publisher::{
        CompilationInputs, ImportAdmission, PublicationProof, PublisherPolicy, ReleaseGateway,
        SignedImportAdmission, VerifiedImportAdmission,
    },
    sha256,
    transport::TlsConfig,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
fn h(c: char) -> String {
    c.to_string().repeat(64)
}
fn openssl(root: &Path, args: &[&str]) -> Result<()> {
    ensure!(
        Command::new("openssl")
            .args(args)
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "synthetic TLS fixture generation failed"
    );
    Ok(())
}
pub struct Package {
    _directory: tempfile::TempDir,
    pub root: PathBuf,
    server: tokio::process::Child,
    pub gateway: ReleaseGateway,
    pub artifact: BuildArtifact,
    pub signed: SignedBuildRelease,
    pub trust: BuildReleaseTrust,
    pub proof_sha: String,
    pub admission: VerifiedImportAdmission,
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = self.server.start_kill();
    }
}
impl Package {
    pub async fn new(mut input: BuildArtifact) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        openssl(
            &root,
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "ca.key",
                "-out",
                "ca.crt",
                "-days",
                "1",
                "-subj",
                "/CN=synthetic-private-ca",
            ],
        )?;
        openssl(
            &root,
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "server.key",
                "-out",
                "server.csr",
                "-subj",
                "/CN=synthetic-gateway",
            ],
        )?;
        std::fs::write(
            root.join("server.ext"),
            "subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n",
        )?;
        openssl(
            &root,
            &[
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "server.crt",
                "-days",
                "1",
                "-extfile",
                "server.ext",
            ],
        )?;
        let program = b"synthetic fixture program";
        for executable in &mut input.executables {
            executable.blob.sha256 = sha256(program);
            executable.blob.bytes = program.len() as u64;
        }
        let (mut artifact, mut signed, trust) = super::attest(input);
        let proof = PublicationProof {
            schema: 1,
            source: signed.receipt.source.clone(),
            image: artifact.image.clone(),
            producer: signed.receipt.producer.clone(),
            software_producer: signed.receipt.producer.clone(),
            required_check_ids: vec![1, 2, 3],
            compilation_inputs: CompilationInputs {
                schema: "monday.compilation-inputs.v3".into(),
                target: artifact.build.target.clone(),
                profile: artifact.build.profile.clone(),
                compiler: h('a'),
                native: h('b'),
                flags: h('c'),
                profiles: h('d'),
                recipe: h('e'),
                locks: BTreeMap::new(),
                builder_image: artifact.build.builder_image.clone(),
                recipes: vec![],
                workspace_profiles: BTreeMap::new(),
            },
            executables: artifact.executables.clone(),
            build_sha256: artifact.build.id()?,
        };
        let proof_sha = identity(&proof)?;
        signed.receipt.publication_readback_sha256 = proof_sha.clone();
        signed.signature_hex = SigningKey::from_bytes(&[17; 32])
            .sign(&signed.signing_bytes()?)
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        artifact.release_receipt_sha256 = identity(&signed)?;
        trust.verify(&artifact, &signed)?;
        let prefix = format!(
            "research/builds/{}/releases/{}/{}",
            artifact.build.id()?,
            artifact.image.rsplit_once("@sha256:").unwrap().1,
            proof_sha
        );
        for (key, bytes) in [
            (
                format!("{prefix}/build-artifact.json"),
                serde_json::to_vec(&artifact)?,
            ),
            (
                format!("{prefix}/signed-release.json"),
                serde_json::to_vec(&signed)?,
            ),
            (
                format!("{prefix}/release-proof.json"),
                serde_json::to_vec(&proof)?,
            ),
            (
                signed.receipt.source.archive.key.clone(),
                b"source fixture".to_vec(),
            ),
            (artifact.executables[0].blob.key.clone(), program.to_vec()),
        ] {
            let path = root.join(key);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, bytes)?;
        }
        for executable in &artifact.executables {
            let path = root.join(&executable.blob.key);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, program)?;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        drop(listener);
        let server = tokio::process::Command::new("openssl")
            .args([
                "s_server",
                "-accept",
                &address.to_string(),
                "-cert",
                "server.crt",
                "-key",
                "server.key",
                "-WWW",
                "-quiet",
            ])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut ready = false;
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        ensure!(ready, "loopback TLS server did not start");
        let gateway = ReleaseGateway::with_tls(
            &format!("https://localhost:{}/", address.port()),
            "synthetic-reader".into(),
            &TlsConfig {
                ca_file: Some(root.join("ca.crt")),
                identity_file: None,
            },
        )?;
        let admission = admission_fixture(
            &artifact,
            &proof_sha,
            &trust,
            false,
            chrono::Utc::now().timestamp_millis() + 60_000,
            1,
        )?;
        Ok(Self {
            admission,
            _directory: directory,
            root,
            server,
            gateway,
            artifact,
            signed,
            trust,
            proof_sha,
        })
    }
}
impl Package {
    pub async fn import(&self, ledger: &Ledger) -> Result<String> {
        let published = hft_research_platform::release_publisher::read_build_release(
            &self.artifact.build.id()?,
            self.artifact.image.rsplit_once("@sha256:").unwrap().1,
            &self.proof_sha,
            &self.trust,
            &self.gateway,
        )
        .await?;
        let id = ledger.register_build(&published, &self.admission).await?;
        ensure!(
            ledger.build_artifact(&id).await? == self.artifact,
            "fixture projection differs"
        );
        Ok(id)
    }
}

pub fn admission_fixture(
    artifact: &BuildArtifact,
    proof: &str,
    trust: &BuildReleaseTrust,
    revoked: bool,
    expires_ms: i64,
    revision: i64,
) -> Result<VerifiedImportAdmission> {
    let operator = SigningKey::from_bytes(&[18; 32]);
    let key_hex = operator
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let policy: PublisherPolicy = serde_json::from_value(serde_json::json!({
        "trust":trust, "key_id":"fixture", "builder_image":artifact.build.builder_image,
        "image_repositories":{}, "import_admission_keys":{"operator":key_hex}
    }))?;
    let signed = SignedImportAdmission::sign(
        ImportAdmission {
            schema: 2,
            revision,
            expires_ms,
            build_sha256: artifact.build.id()?,
            image_sha256: artifact.image.rsplit_once("@sha256:").unwrap().1.into(),
            publication_proof_sha256: proof.into(),
            revoked,
        },
        "operator".into(),
        &operator,
        &policy,
        chrono::Utc::now().timestamp_millis(),
    )?;
    VerifiedImportAdmission::from_signed(signed, &policy)
}
