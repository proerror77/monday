//! Independent release issuer. This module is absent from research services.
//! Inputs are authenticated/read back before a private key signs any receipt.
use crate::{
    build::{pinned_image, BuildArtifact, BuildSpec, BuiltExecutable},
    identity,
    orchestrator::Artifact,
    release::{
        BuildReleaseReceipt, BuildReleaseTrust, ReleaseProducer, SignedBuildRelease, SourceArchive,
    },
    sha256, valid_digest,
};
use anyhow::{ensure, Context, Result};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    process::{Command, Stdio},
};
const MAX_OBJECT: u64 = 512 * 1024 * 1024;
const MAX_JSON: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherPolicy {
    #[serde(default)]
    pub oss: Option<crate::release_oss::OssConfig>,
    pub trust: BuildReleaseTrust,
    pub key_id: String,
    pub builder_image: String,
    pub image_repositories: BTreeMap<String, String>,
    #[serde(default)]
    pub tls: crate::transport::TlsConfig,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRequest {
    pub source_sha: String,
    pub software_run_id: u64,
    pub software_products: String,
    pub product: String,
    pub image: String,
    pub publisher_run_id: u64,
    pub publisher_run_attempt: u32,
    pub publisher_job_id: u64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompilationRecipe {
    pub manifest: String,
    pub package: String,
    pub features: String,
    pub binaries: Vec<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompilationInputs {
    pub schema: String,
    pub target: String,
    pub profile: String,
    pub compiler: String,
    pub native: String,
    pub flags: String,
    pub profiles: String,
    pub recipe: String,
    pub locks: BTreeMap<String, String>,
    pub builder_image: String,
    pub recipes: Vec<CompilationRecipe>,
    pub workspace_profiles: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SoftwareRelease {
    schema: String,
    products: Vec<String>,
    source_sha: String,
    workflow_run_id: String,
    workflow_run_attempt: u32,
    workflow_job_id: u64,
    target: String,
    build_inputs: CompilationInputs,
    cargo_locks: BTreeMap<String, String>,
    binaries: Vec<SoftwareBinary>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SoftwareBinary {
    file: String,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationProof {
    pub schema: u32,
    pub source: SourceArchive,
    pub image: String,
    pub producer: ReleaseProducer,
    pub software_producer: ReleaseProducer,
    pub required_check_ids: Vec<u64>,
    pub compilation_inputs: CompilationInputs,
    pub executables: Vec<BuiltExecutable>,
    pub build_sha256: String,
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_JSON + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_JSON, "release JSON exceeds bound");
    Ok(serde_json::from_slice(&bytes)?)
}

/// Secret material never enters JSON, the ledger, command arguments or logs.
pub fn read_signing_key(path: &Path) -> Result<SigningKey> {
    use std::os::unix::fs::PermissionsExt;
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| anyhow::anyhow!("signing key unavailable"))?;
    let mut file = std::fs::File::from(fd);
    let info = file.metadata()?;
    ensure!(
        info.is_file() && info.permissions().mode() & 0o077 == 0 && info.len() == 64,
        "signing key must be private 32-byte lowercase hex"
    );
    let mut encoded = [0u8; 64];
    file.read_exact(&mut encoded)?;
    let mut bytes = [0; 32];
    for (i, output) in bytes.iter_mut().enumerate() {
        let a = encoded[2 * i];
        let b = encoded[2 * i + 1];
        ensure!(
            [a, b]
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)),
            "invalid signing key encoding"
        );
        fn digit(b: u8) -> u8 {
            if b <= b'9' {
                b - b'0'
            } else {
                b - b'a' + 10
            }
        }
        *output = digit(a) * 16 + digit(b);
    }
    let key = SigningKey::from_bytes(&bytes);
    encoded.fill(0);
    bytes.fill(0);
    Ok(key)
}

/// Fail before registry publication if operator issuer configuration is absent,
/// mismatched or unsafe. This performs no signing and reads no scientific state.
pub fn check_configuration(policy: &PublisherPolicy, key: &SigningKey) -> Result<()> {
    let repository = &policy.trust.repository;
    ensure!(
        policy.trust.schema == 1
            && repository.split('/').count() == 2
            && repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b))
            && policy.trust.producer_workflow_path == ".github/workflows/acr-publish.yml"
            && (1..=16).contains(&policy.trust.keys.len())
            && !policy.key_id.is_empty()
            && policy.key_id.len() <= 128
            && pinned_image(&policy.builder_image)
            && !policy.image_repositories.is_empty()
            && policy.image_repositories.len() <= 16
            && policy
                .image_repositories
                .values()
                .all(|name| !name.is_empty()
                    && !name.contains('@')
                    && !name.contains(char::is_whitespace)),
        "invalid operator release policy"
    );
    let public = key
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    ensure!(
        policy.trust.keys.get(&policy.key_id) == Some(&public)
            && policy.trust.keys.values().all(|k| valid_digest(k)),
        "signer does not match operator public trust"
    );
    Ok(())
}

/// Check the selected publication against authenticated producer metadata before
/// a registry login or push. A well-formed policy can still name another target.
pub fn check_publication_configuration(
    policy: &PublisherPolicy,
    key: &SigningKey,
    software_manifest: &Path,
    repository: &str,
    product: &str,
    image_repository: &str,
) -> Result<()> {
    check_configuration(policy, key)?;
    let manifest: SoftwareRelease = read_json(software_manifest)?;
    ensure!(
        policy.trust.repository == repository
            && policy.image_repositories.get(product).map(String::as_str) == Some(image_repository)
            && manifest.schema == "monday.research-image-release.v6"
            && manifest.build_inputs.schema == "monday.compilation-inputs.v3"
            && manifest.build_inputs.builder_image == policy.builder_image
            && manifest.products.contains(&product.to_owned()),
        "operator policy does not admit selected publication"
    );
    Ok(())
}

fn command(root: &Path, program: &str, args: &[&str], repository: &str) -> Result<Vec<u8>> {
    // Never return stderr (gh and transport tools may include credential URLs).
    let mut child = Command::new(program)
        .current_dir(root)
        .args(args)
        .env("GITHUB_REPOSITORY", repository)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("release verifier tool unavailable")?;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .context("missing verifier output")?
        .take(8 * MAX_JSON + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > 8 * MAX_JSON {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("release verifier output exceeds bound");
    }
    ensure!(
        child.wait()?.success(),
        "release verifier rejected evidence"
    );
    Ok(output)
}
fn api(root: &Path, repository: &str, endpoint: &str, pages: bool) -> Result<Value> {
    let endpoint = format!("repos/{repository}/{endpoint}");
    let args = if pages {
        vec!["api", "--paginate", "--slurp", &endpoint]
    } else {
        vec!["api", &endpoint]
    };
    Ok(serde_json::from_slice(&command(
        root, "gh", &args, repository,
    )?)?)
}
fn sha_is_valid(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn required_checks(pages: &Value, source: &str) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for name in [
        "Monorepo CI gate",
        "Prediction Markets CI gate",
        "Security Summary Report",
    ] {
        let latest = pages
            .as_array()
            .context("invalid check pages")?
            .iter()
            .flat_map(|p| p["check_runs"].as_array().into_iter().flatten())
            .filter(|c| {
                c["name"] == name && c["app"]["slug"] == "github-actions" && c["app"]["id"] == 15368
            })
            .max_by_key(|c| c["id"].as_u64().unwrap_or_default())
            .context("missing required authenticated CI")?;
        ensure!(
            latest["head_sha"] == source
                && latest["status"] == "completed"
                && latest["conclusion"] == "success",
            "required exact-source CI did not pass"
        );
        ids.push(
            latest["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .context("invalid check identity")?,
        );
    }
    Ok(ids)
}
fn validate_run(
    run: &Value,
    repository: &str,
    sha: &str,
    id: u64,
    attempt: u32,
    workflow: &str,
) -> Result<()> {
    ensure!(
        run["id"] == id
            && run["run_attempt"] == attempt
            && run["head_sha"] == sha
            && run["head_branch"] == "main"
            && run["head_repository"]["full_name"] == repository
            && run["path"] == workflow
            && matches!(
                run["event"].as_str(),
                Some("push" | "workflow_run" | "workflow_dispatch")
            ),
        "untrusted release run/source/attempt"
    );
    ensure!(
        !matches!(
            run["conclusion"].as_str(),
            Some("failure" | "cancelled" | "timed_out" | "skipped")
        ),
        "release run failed or cancelled"
    );
    Ok(())
}
fn validate_job(job: &Value, run: u64, attempt: u32, source: &str, publisher: bool) -> Result<()> {
    ensure!(
        job["run_id"] == run
            && job["run_attempt"] == attempt
            && job["head_sha"] == source
            && job["id"].as_u64().is_some_and(|id| id > 0),
        "release job provenance mismatch"
    );
    ensure!(
        if publisher {
            job["status"] == "in_progress"
                && job["name"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("Publish "))
        } else {
            job["status"] == "completed"
                && job["conclusion"] == "success"
                && matches!(
                    job["name"].as_str(),
                    Some("Research image binaries" | "Research release binaries")
                )
        },
        "release job not admitted"
    );
    Ok(())
}

fn project_builds(
    source: &SourceArchive,
    inputs: &CompilationInputs,
    names: &BTreeSet<String>,
) -> Result<Vec<BuildSpec>> {
    ensure!(
        inputs.schema == "monday.compilation-inputs.v3"
            && inputs.profile == "release"
            && inputs.target == "x86_64-unknown-linux-gnu"
            && pinned_image(&inputs.builder_image)
            && [
                inputs.compiler.as_str(),
                &inputs.native,
                &inputs.flags,
                &inputs.profiles,
                &inputs.recipe
            ]
            .iter()
            .all(|h| valid_digest(h)),
        "invalid producer compilation inputs"
    );
    let mut covered = BTreeSet::new();
    let mut builds = Vec::new();
    for recipe in &inputs.recipes {
        let binaries: Vec<_> = recipe
            .binaries
            .iter()
            .filter(|b| names.contains(*b))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if binaries.is_empty() {
            continue;
        }
        for b in &binaries {
            ensure!(covered.insert(b.clone()), "ambiguous executable owner");
        }
        let lock = recipe.manifest.replace("Cargo.toml", "Cargo.lock");
        let features = recipe
            .features
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| {
                if s.contains('/') {
                    s.to_string()
                } else {
                    format!("{}/{}", recipe.package, s)
                }
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let build = BuildSpec {
            schema: 2,
            code_commit: source.code_commit.clone(),
            source_manifest_sha256: identity(source)?,
            workspace_manifest: recipe.manifest.clone(),
            cargo_lock_sha256: inputs
                .locks
                .get(&lock)
                .context("owning lock missing")?
                .clone(),
            toolchain_manifest_sha256: inputs.compiler.clone(),
            target: inputs.target.clone(),
            packages: vec![recipe.package.clone()],
            binaries,
            features,
            default_features: true,
            profile: inputs.profile.clone(),
            profile_manifest_sha256: inputs
                .workspace_profiles
                .get(&recipe.manifest)
                .context("owning profile missing")?
                .clone(),
            rustflags_sha256: inputs.flags.clone(),
            native_environment_sha256: inputs.native.clone(),
            builder_image: inputs.builder_image.clone(),
        };
        build.id()?;
        builds.push(build);
    }
    ensure!(
        &covered == names && !builds.is_empty(),
        "unbuilt executable requested"
    );
    Ok(builds)
}

fn measure(path: &Path, key: String) -> Result<Artifact> {
    let info = std::fs::symlink_metadata(path)?;
    ensure!(
        info.is_file() && (1..=MAX_OBJECT).contains(&info.len()),
        "release object is not a bounded regular file"
    );
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut chunk = [0u8; 65536];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(size <= MAX_OBJECT, "release object exceeds bound");
        digest.update(&chunk[..count]);
    }
    ensure!(size == info.len(), "release object changed during readback");
    Ok(Artifact {
        key,
        sha256: format!("{:x}", digest.finalize()),
        bytes: size,
    })
}

struct Scratch(tempfile::TempDir);
impl Scratch {
    fn new() -> Result<Self> {
        Ok(Self(tempfile::tempdir()?))
    }
    fn join(&self, path: &str) -> std::path::PathBuf {
        self.0.path().join(path)
    }
}

/// Release transport for direct OSS or the retained HTTPS capability gateway.
/// Upload and independent GET readback are distinct checks, including retries.
pub struct ReleaseGateway {
    oss: Option<crate::release_oss::Oss>,
    client: reqwest::Client,
    base: reqwest::Url,
    token: String,
}
impl ReleaseGateway {
    pub fn oss(config: &crate::release_oss::OssConfig, session: &Path) -> Result<Self> {
        let mut transport = Self::new(&config.endpoint, "unused".into())?;
        transport.oss = Some(crate::release_oss::Oss::from_file(config, session)?);
        Ok(transport)
    }
    pub async fn check_oss(&self, source: &str) -> Result<()> {
        self.oss
            .as_ref()
            .context("OSS backend required")?
            .check(source)
            .await
    }
    async fn get(&self, key: &str) -> Result<reqwest::Response> {
        if let Some(oss) = &self.oss {
            return oss.get(key, None).await;
        }
        self.client
            .get(self.url(key)?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("release read unavailable"))?
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("release evidence missing"))
    }

    pub fn new(endpoint: &str, token: String) -> Result<Self> {
        let base = reqwest::Url::parse(endpoint)?;
        ensure!(
            endpoint.ends_with('/')
                && base.scheme() == "https"
                && base.host_str().is_some()
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none()
                && base.path().ends_with('/')
                && !token.is_empty(),
            "publisher requires scoped HTTPS gateway"
        );
        Ok(Self {
            oss: None,
            client: crate::transport::TlsConfig::default()
                .client(std::time::Duration::from_secs(120), true)?,
            base,
            token,
        })
    }
    pub fn with_tls(
        endpoint: &str,
        token: String,
        tls: &crate::transport::TlsConfig,
    ) -> Result<Self> {
        let mut gateway = Self::new(endpoint, token)?;
        gateway.client = tls.client(std::time::Duration::from_secs(120), true)?;
        Ok(gateway)
    }
    /// Establish server trust before registry writes. A HEAD response can reject
    /// this route; a TLS handshake failure cannot admit publication.
    pub async fn check_transport(&self) -> Result<()> {
        let response = self
            .client
            .head(self.base.clone())
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("release gateway TLS unavailable"))?;
        ensure!(
            !response.status().is_redirection(),
            "release gateway redirects are not admitted"
        );
        Ok(())
    }
    fn url(&self, key: &str) -> Result<reqwest::Url> {
        ensure!(
            key.starts_with("research/")
                && !key.contains("..")
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c)),
            "unsafe release key"
        );
        Ok(self.base.join(key)?)
    }
    async fn get_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T> {
        let mut response = self.get(key).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("release read interrupted"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= MAX_JSON as usize,
                "proof exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    async fn verify(&self, artifact: &Artifact) -> Result<()> {
        let mut response = self.get(&artifact.key).await?;
        let mut digest = Sha256::new();
        let mut size = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("release readback interrupted"))?
        {
            size += chunk.len() as u64;
            ensure!(size <= artifact.bytes, "release readback exceeds bound");
            digest.update(&chunk);
        }
        ensure!(
            size == artifact.bytes && format!("{:x}", digest.finalize()) == artifact.sha256,
            "release bytes changed or missing"
        );
        Ok(())
    }
    async fn publish_file(&self, path: &Path, artifact: &Artifact) -> Result<()> {
        let file = tokio::fs::File::open(path).await?;
        if let Some(oss) = &self.oss {
            oss.put(
                &artifact.key,
                reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file)),
                artifact.bytes,
            )
            .await?;
            return self.verify(artifact).await;
        }
        let response = self
            .client
            .put(self.url(&artifact.key)?)
            .bearer_auth(&self.token)
            .header("If-None-Match", "*")
            .header(reqwest::header::CONTENT_LENGTH, artifact.bytes)
            .body(reqwest::Body::wrap_stream(
                tokio_util::io::ReaderStream::new(file),
            ))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("release upload unavailable"))?;
        ensure!(
            response.status().is_success()
                || response.status() == reqwest::StatusCode::CONFLICT
                || response.status() == reqwest::StatusCode::PRECONDITION_FAILED,
            "release upload rejected"
        );
        self.verify(artifact).await
    }
    async fn publish_json(&self, key: String, value: &impl Serialize) -> Result<Artifact> {
        let bytes = serde_json::to_vec(value)?;
        ensure!(
            bytes.len() as u64 <= MAX_JSON,
            "release proof exceeds bound"
        );
        let artifact = Artifact {
            key,
            sha256: sha256(&bytes),
            bytes: bytes.len() as u64,
        };
        if let Some(oss) = &self.oss {
            oss.put(&artifact.key, bytes.into(), artifact.bytes).await?;
            self.verify(&artifact).await?;
            return Ok(artifact);
        }
        let response = self
            .client
            .put(self.url(&artifact.key)?)
            .bearer_auth(&self.token)
            .header("If-None-Match", "*")
            .body(bytes)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("release proof upload unavailable"))?;
        ensure!(
            response.status().is_success()
                || response.status() == reqwest::StatusCode::CONFLICT
                || response.status() == reqwest::StatusCode::PRECONDITION_FAILED,
            "release proof upload rejected"
        );
        self.verify(&artifact).await?;
        Ok(artifact)
    }
}

#[derive(Serialize)]
pub struct PublishedBuild {
    pub build_sha256: String,
    pub image_sha256: String,
    pub publication_proof_sha256: String,
    pub artifact: BuildArtifact,
}

/// All metadata below comes from authenticated Actions downloads and actual
/// source/OCI/gateway bytes. The request carries selectors, never claimed hashes.
pub async fn publish(
    root: &Path,
    request: &PublicationRequest,
    policy: &PublisherPolicy,
    key: &SigningKey,
    gateway: &ReleaseGateway,
) -> Result<Vec<PublishedBuild>> {
    check_configuration(policy, key)?;
    let repository = &policy.trust.repository;
    ensure!(
        repository.split('/').count() == 2
            && repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b))
            && policy.trust.producer_workflow_path == ".github/workflows/acr-publish.yml"
            && sha_is_valid(&request.source_sha)
            && request.software_run_id > 0
            && request.publisher_run_id > 0
            && request.publisher_run_attempt > 0
            && request.publisher_job_id > 0
            && pinned_image(&request.image)
            && pinned_image(&policy.builder_image),
        "invalid release selectors/policy"
    );
    ensure!(
        request.image.rsplit_once("@sha256:").map(|(name, _)| name)
            == policy
                .image_repositories
                .get(&request.product)
                .map(String::as_str),
        "untrusted image repository/product"
    );
    let sha = &request.source_sha;
    ensure!(
        command(root, "git", &["rev-parse", "HEAD"], repository)? == format!("{sha}\n").as_bytes()
            && command(
                root,
                "git",
                &["status", "--porcelain", "--untracked-files=no"],
                repository
            )?
            .is_empty(),
        "publisher checkout must be the clean exact source"
    );
    let check_ids = authenticate(root, request, policy)?;
    let scratch = Scratch::new()?;
    let software = scratch.join("software");
    command(
        root,
        "bash",
        &[
            ".github/scripts/download-research-release.sh",
            &request.software_run_id.to_string(),
            sha,
            software.to_str().context("scratch path")?,
            &request.software_products,
        ],
        repository,
    )?;
    let manifest: SoftwareRelease = read_json(&software.join("research-image-release.json"))?;
    ensure!(
        manifest.schema == "monday.research-image-release.v6"
            && manifest.source_sha == *sha
            && manifest.workflow_run_id == request.software_run_id.to_string()
            && manifest.target == manifest.build_inputs.target
            && manifest.cargo_locks == manifest.build_inputs.locks
            && manifest.build_inputs.builder_image == policy.builder_image
            && manifest.products.join(",") == request.software_products,
        "software inputs/source mismatch"
    );
    let software_run = api(
        root,
        repository,
        &format!("actions/runs/{}", request.software_run_id),
        false,
    )?;
    let software_workflow = software_run["path"]
        .as_str()
        .context("missing software workflow")?;
    ensure!(
        matches!(
            software_workflow,
            ".github/workflows/ploy-ci.yml" | ".github/workflows/acr-publish.yml"
        ),
        "untrusted software workflow"
    );
    validate_run(
        &software_run,
        repository,
        sha,
        request.software_run_id,
        manifest.workflow_run_attempt,
        software_workflow,
    )?;
    validate_job(
        &api(
            root,
            repository,
            &format!("actions/jobs/{}", manifest.workflow_job_id),
            false,
        )?,
        request.software_run_id,
        manifest.workflow_run_attempt,
        sha,
        false,
    )?;
    let selected = command(
        root,
        "bash",
        &[
            ".github/scripts/research-release-products.sh",
            "binaries",
            &request.product,
        ],
        repository,
    )?;
    let names = std::str::from_utf8(&selected)?
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    ensure!(
        !request.product.contains(',') && manifest.products.contains(&request.product),
        "product not built"
    );
    // Measure the complete committed source archive, not a caller-owned tar/hash.
    let source_path = scratch.join("source.tar");
    command(
        root,
        "git",
        &[
            "archive",
            "--format=tar",
            "--output",
            source_path.to_str().context("archive path")?,
            sha,
        ],
        repository,
    )?;
    let source = SourceArchive {
        schema: 1,
        code_commit: sha.clone(),
        archive: measure(&source_path, format!("research/sources/{sha}/source.tar"))?,
    };
    let builds = project_builds(&source, &manifest.build_inputs, &names)?;
    gateway.publish_file(&source_path, &source.archive).await?;
    // Independently pull the immutable OCI identity and compare contained bytes.
    command(root, "docker", &["pull", &request.image], repository)?;
    command(
        root,
        "bash",
        &[
            ".github/scripts/verify-research-product-image.sh",
            &request.image,
            sha,
            software
                .join("research-bin")
                .to_str()
                .context("binary path")?,
            &request.product,
        ],
        repository,
    )?;
    let producer = ReleaseProducer {
        repository: repository.clone(),
        workflow_path: policy.trust.producer_workflow_path.clone(),
        source_sha: sha.clone(),
        run_id: request.publisher_run_id,
        run_attempt: request.publisher_run_attempt,
        job_id: request.publisher_job_id,
    };
    let software_producer = ReleaseProducer {
        repository: repository.clone(),
        workflow_path: software_workflow.into(),
        source_sha: sha.clone(),
        run_id: request.software_run_id,
        run_attempt: manifest.workflow_run_attempt,
        job_id: manifest.workflow_job_id,
    };
    let mut artifacts = Vec::new();
    for build in builds {
        let build_id = build.id()?;
        let prefix = format!("research/builds/{build_id}");
        let release_prefix = format!(
            "{prefix}/releases/{}",
            request
                .image
                .rsplit_once("@sha256:")
                .context("immutable image required")?
                .1
        );
        let mut executables = Vec::new();
        for name in &build.binaries {
            let path = software.join("research-bin").join(name);
            // Download acquisition already verified these immutable producer
            // bytes; OCI comparison and gateway GET retain their own boundaries.
            let expected = manifest
                .binaries
                .iter()
                .filter(|b| b.file == *name)
                .collect::<Vec<_>>();
            ensure!(
                expected.len() == 1 && valid_digest(&expected[0].sha256),
                "producer executable identity missing"
            );
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && (1..=MAX_OBJECT).contains(&metadata.len()),
                "producer executable size/type changed"
            );
            let blob = Artifact {
                key: format!("{prefix}/{name}"),
                sha256: expected[0].sha256.clone(),
                bytes: metadata.len(),
            };
            gateway.publish_file(&path, &blob).await?;
            executables.push(BuiltExecutable {
                name: name.clone(),
                blob,
            });
        }
        let proof = PublicationProof {
            schema: 1,
            source: source.clone(),
            image: request.image.clone(),
            producer: producer.clone(),
            software_producer: software_producer.clone(),
            required_check_ids: check_ids.clone(),
            compilation_inputs: manifest.build_inputs.clone(),
            executables: executables.clone(),
            build_sha256: build_id.clone(),
        };
        let publication_proof_sha256 = identity(&proof)?;
        let proof_prefix = format!("{release_prefix}/{publication_proof_sha256}");
        let proof_blob = gateway
            .publish_json(format!("{proof_prefix}/release-proof.json"), &proof)
            .await?;
        // Re-read mutable authority just before signing, after all byte readbacks.
        ensure!(
            authenticate(root, request, policy)? == check_ids,
            "release CI changed during publication"
        );
        validate_run(
            &api(
                root,
                repository,
                &format!("actions/runs/{}", request.software_run_id),
                false,
            )?,
            repository,
            sha,
            request.software_run_id,
            manifest.workflow_run_attempt,
            software_workflow,
        )?;
        validate_job(
            &api(
                root,
                repository,
                &format!("actions/jobs/{}", manifest.workflow_job_id),
                false,
            )?,
            request.software_run_id,
            manifest.workflow_run_attempt,
            sha,
            false,
        )?;
        let mut signed = SignedBuildRelease {
            schema: 1,
            key_id: policy.key_id.clone(),
            receipt: BuildReleaseReceipt {
                schema: 1,
                build_sha256: build_id,
                source: source.clone(),
                image: request.image.clone(),
                target: build.target.clone(),
                executables: executables.clone(),
                producer: producer.clone(),
                publication_readback_sha256: proof_blob.sha256,
            },
            signature_hex: String::new(),
        };
        signed.signature_hex = key
            .sign(&signed.signing_bytes()?)
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let artifact = BuildArtifact {
            schema: 1,
            build,
            image: request.image.clone(),
            executables,
            release_receipt_sha256: identity(&signed)?,
        };
        policy.trust.verify(&artifact, &signed)?;
        gateway
            .publish_json(format!("{proof_prefix}/signed-release.json"), &signed)
            .await?;
        gateway
            .publish_json(format!("{proof_prefix}/build-artifact.json"), &artifact)
            .await?;
        artifacts.push(PublishedBuild {
            build_sha256: artifact.build.id()?,
            image_sha256: request
                .image
                .rsplit_once("@sha256:")
                .context("immutable image")?
                .1
                .into(),
            publication_proof_sha256,
            artifact,
        });
    }
    Ok(artifacts)
}
pub fn check_source_authority(root: &Path, repository: &str, source: &str) -> Result<()> {
    ensure!(
        sha_is_valid(source)
            && api(root, repository, "git/ref/heads/main", false)?["object"]["sha"] == source,
        "OSS preflight source is not current main"
    );
    required_checks(
        &api(
            root,
            repository,
            &format!("commits/{source}/check-runs?filter=latest&per_page=100"),
            true,
        )?,
        source,
    )?;
    Ok(())
}
fn authenticate(root: &Path, r: &PublicationRequest, p: &PublisherPolicy) -> Result<Vec<u64>> {
    let repository = &p.trust.repository;
    ensure!(
        api(root, repository, "git/ref/heads/main", false)?["object"]["sha"] == r.source_sha,
        "release source is no longer current main"
    );
    validate_run(
        &api(
            root,
            repository,
            &format!("actions/runs/{}", r.publisher_run_id),
            false,
        )?,
        repository,
        &r.source_sha,
        r.publisher_run_id,
        r.publisher_run_attempt,
        &p.trust.producer_workflow_path,
    )?;
    validate_job(
        &api(
            root,
            repository,
            &format!("actions/jobs/{}", r.publisher_job_id),
            false,
        )?,
        r.publisher_run_id,
        r.publisher_run_attempt,
        &r.source_sha,
        true,
    )?;
    let job = api(
        root,
        repository,
        &format!("actions/jobs/{}", r.publisher_job_id),
        false,
    )?;
    let image_name = r
        .image
        .rsplit_once("@sha256:")
        .context("immutable image required")?
        .0
        .rsplit('/')
        .next()
        .context("image repository required")?;
    ensure!(
        job["name"] == format!("Publish {image_name}"),
        "publication job does not own this image"
    );
    required_checks(
        &api(
            root,
            repository,
            &format!(
                "commits/{}/check-runs?filter=latest&per_page=100",
                r.source_sha
            ),
            true,
        )?,
        &r.source_sha,
    )
}

#[derive(Serialize)]
pub struct ReleasePlan {
    pub schema: u32,
    pub image: String,
    pub source: SourceArchive,
    pub builds: Vec<BuildSpec>,
    pub publisher_prefixes: Vec<String>,
}
/// Exact broker scopes computed from authenticated software and committed source.
/// This prepares selectors only; it never uploads, signs, imports, or grants a Run.
pub fn plan(
    root: &Path,
    request: &PublicationRequest,
    policy: &PublisherPolicy,
) -> Result<ReleasePlan> {
    let repository = &policy.trust.repository;
    ensure!(
        sha_is_valid(&request.source_sha)
            && repository.split('/').count() == 2
            && repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b)),
        "invalid plan source/repository"
    );
    ensure!(
        command(root, "git", &["rev-parse", "HEAD"], repository)?
            == format!("{}\n", request.source_sha).as_bytes(),
        "plan source checkout mismatch"
    );
    authenticate(root, request, policy)?;
    let scratch = Scratch::new()?;
    let software = scratch.join("software");
    command(
        root,
        "bash",
        &[
            ".github/scripts/download-research-release.sh",
            &request.software_run_id.to_string(),
            &request.source_sha,
            software.to_str().context("software path")?,
            &request.software_products,
        ],
        repository,
    )?;
    let manifest: SoftwareRelease = read_json(&software.join("research-image-release.json"))?;
    ensure!(
        manifest.schema == "monday.research-image-release.v6"
            && manifest.source_sha == request.source_sha
            && manifest.workflow_run_id == request.software_run_id.to_string()
            && manifest.build_inputs.builder_image == policy.builder_image
            && manifest.products.contains(&request.product),
        "plan compiler provenance mismatch"
    );
    let names = command(
        root,
        "bash",
        &[
            ".github/scripts/research-release-products.sh",
            "binaries",
            &request.product,
        ],
        repository,
    )?;
    let names = std::str::from_utf8(&names)?
        .lines()
        .map(str::to_owned)
        .collect();
    let archive = scratch.join("source.tar");
    command(
        root,
        "git",
        &[
            "archive",
            "--format=tar",
            "--output",
            archive.to_str().context("archive path")?,
            &request.source_sha,
        ],
        repository,
    )?;
    let source = SourceArchive {
        schema: 1,
        code_commit: request.source_sha.clone(),
        archive: measure(
            &archive,
            format!("research/sources/{}/source.tar", request.source_sha),
        )?,
    };
    let builds = project_builds(&source, &manifest.build_inputs, &names)?;
    let mut publisher_prefixes = vec![format!("research/sources/{}/", request.source_sha)];
    for build in &builds {
        publisher_prefixes.push(format!("research/builds/{}/", build.id()?));
    }
    Ok(ReleasePlan {
        schema: 1,
        image: request.image.clone(),
        source,
        builds,
        publisher_prefixes,
    })
}

/// Independent importer consumes gateway proof/bytes and a public trust file.
/// It has no signing key, grant, budget, task submission or backend activation.
/// Only the importer can construct this value, after independent bounded GETs
/// of the exact package, proof, source and every program. Pure signature
/// verification remains a separate, read-only contract.
pub struct VerifiedPublishedBuildRelease {
    verified: crate::release::VerifiedBuildRelease,
}
impl VerifiedPublishedBuildRelease {
    pub fn artifact(&self) -> &BuildArtifact {
        self.verified.artifact()
    }
    pub(crate) fn verified(&self) -> &crate::release::VerifiedBuildRelease {
        &self.verified
    }
}

pub async fn read_build_release(
    build_id: &str,
    oci_sha256: &str,
    publication_proof_sha256: &str,
    trust: &BuildReleaseTrust,
    gateway: &ReleaseGateway,
) -> Result<VerifiedPublishedBuildRelease> {
    ensure!(
        valid_digest(build_id)
            && valid_digest(oci_sha256)
            && valid_digest(publication_proof_sha256),
        "invalid Build/OCI identity"
    );
    let prefix =
        format!("research/builds/{build_id}/releases/{oci_sha256}/{publication_proof_sha256}");
    let artifact: BuildArtifact = gateway
        .get_json(&format!("{prefix}/build-artifact.json"))
        .await?;
    let signed: SignedBuildRelease = gateway
        .get_json(&format!("{prefix}/signed-release.json"))
        .await?;
    let verified = trust.verify(&artifact, &signed)?;
    ensure!(
        artifact.build.id()? == build_id
            && artifact
                .image
                .rsplit_once("@sha256:")
                .map(|(_, digest)| digest)
                == Some(oci_sha256),
        "projected Build identity mismatch"
    );
    let proof: PublicationProof = gateway
        .get_json(&format!("{prefix}/release-proof.json"))
        .await?;
    ensure!(
        identity(&proof)? == signed.receipt.publication_readback_sha256
            && signed.receipt.publication_readback_sha256 == publication_proof_sha256
            && proof.schema == 1
            && proof.build_sha256 == build_id
            && proof.source == signed.receipt.source
            && proof.image == artifact.image
            && proof.producer == signed.receipt.producer
            && proof.executables == artifact.executables,
        "release publication proof mismatch"
    );
    gateway.verify(&proof.source.archive).await?;
    for executable in &artifact.executables {
        gateway.verify(&executable.blob).await?;
    }
    Ok(VerifiedPublishedBuildRelease { verified })
}

pub async fn import_build(
    build_id: &str,
    oci_sha256: &str,
    publication_proof_sha256: &str,
    trust: &BuildReleaseTrust,
    gateway: &ReleaseGateway,
    ledger: &crate::postgres::Ledger,
) -> Result<String> {
    let published = read_build_release(
        build_id,
        oci_sha256,
        publication_proof_sha256,
        trust,
        gateway,
    )
    .await?;
    let id = ledger.register_build(&published).await?;
    ensure!(
        ledger.build_artifact(&id).await? == *published.artifact(),
        "PG Build projection readback mismatch"
    );
    Ok(id)
}

/// Host-owned ACK approval. Updating this file never changes scientific grants.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportAdmission {
    pub schema: u32,
    pub expires_ms: i64,
    pub build_sha256: String,
    pub image_sha256: String,
    pub publication_proof_sha256: String,
    pub revoked: bool,
}
impl ImportAdmission {
    pub fn validate(&self, build: &str, image: &str, proof: &str, now: i64) -> Result<()> {
        ensure!(
            self.schema == 1
                && !self.revoked
                && self.expires_ms > now
                && valid_digest(build)
                && valid_digest(image)
                && valid_digest(proof)
                && self.build_sha256 == build
                && self.image_sha256 == image
                && self.publication_proof_sha256 == proof,
            "ACK import admission expired, revoked or foreign"
        );
        Ok(())
    }
}
fn completed_producer(
    run: &Value,
    job: &Value,
    p: &ReleaseProducer,
    publisher: bool,
    image: &str,
) -> Result<()> {
    validate_run(
        run,
        &p.repository,
        &p.source_sha,
        p.run_id,
        p.run_attempt,
        &p.workflow_path,
    )?;
    ensure!(
        run["status"] == "completed"
            && run["conclusion"] == "success"
            && job["id"] == p.job_id
            && job["run_id"] == p.run_id
            && job["run_attempt"] == p.run_attempt
            && job["head_sha"] == p.source_sha
            && job["status"] == "completed"
            && job["conclusion"] == "success",
        "ACK import requires completed successful exact producers"
    );
    if publisher {
        ensure!(
            job["name"]
                == format!(
                    "Publish {}",
                    image
                        .rsplit_once("@sha256:")
                        .context("pinned image required")?
                        .0
                        .rsplit('/')
                        .next()
                        .context("image name")?
                ),
            "wrong publication job"
        );
    } else {
        validate_job(job, p.run_id, p.run_attempt, &p.source_sha, false)?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub async fn import_oss_build(
    root: &Path,
    build: &str,
    image: &str,
    proof_id: &str,
    policy: &PublisherPolicy,
    store: &ReleaseGateway,
    ledger: &crate::postgres::Ledger,
    admission_path: &Path,
) -> Result<String> {
    store
        .oss
        .as_ref()
        .context("ACK import requires OSS")?
        .require_reader()?;
    let admission: ImportAdmission =
        serde_json::from_slice(&crate::transport::read_private_file(admission_path)?)?;
    admission.validate(
        build,
        image,
        proof_id,
        chrono::Utc::now().timestamp_millis(),
    )?;
    let published = read_build_release(build, image, proof_id, &policy.trust, store).await?;
    let prefix = format!("research/builds/{build}/releases/{image}/{proof_id}");
    let proof: PublicationProof = store
        .get_json(&format!("{prefix}/release-proof.json"))
        .await?;
    ensure!(
        identity(&proof)? == proof_id
            && proof.producer.repository == policy.trust.repository
            && proof.producer.workflow_path == policy.trust.producer_workflow_path
            && proof.software_producer.repository == policy.trust.repository
            && matches!(
                proof.software_producer.workflow_path.as_str(),
                ".github/workflows/ploy-ci.yml" | ".github/workflows/acr-publish.yml"
            )
            && proof.software_producer.source_sha == proof.source.code_commit
            && proof.producer.source_sha == proof.source.code_commit
            && policy.image_repositories.values().any(|repo| proof
                .image
                .rsplit_once("@sha256:")
                .map(|(r, _)| r)
                == Some(repo.as_str())),
        "foreign import producer/workflow/image"
    );
    let repo = &policy.trust.repository;
    ensure!(
        api(root, repo, "git/ref/heads/main", false)?["object"]["sha"] == proof.source.code_commit,
        "ACK import source drifted from main"
    );
    for (producer, publisher) in [(&proof.software_producer, false), (&proof.producer, true)] {
        let run = api(
            root,
            repo,
            &format!("actions/runs/{}", producer.run_id),
            false,
        )?;
        let job = api(
            root,
            repo,
            &format!("actions/jobs/{}", producer.job_id),
            false,
        )?;
        completed_producer(&run, &job, producer, publisher, &proof.image)?;
    }
    ensure!(
        required_checks(
            &api(
                root,
                repo,
                &format!(
                    "commits/{}/check-runs?filter=latest&per_page=100",
                    proof.source.code_commit
                ),
                true
            )?,
            &proof.source.code_commit
        )? == proof.required_check_ids,
        "ACK import required checks changed"
    );
    ensure!(
        api(root, repo, "git/ref/heads/main", false)?["object"]["sha"] == proof.source.code_commit,
        "ACK import source changed during readback"
    );
    let admission: ImportAdmission =
        serde_json::from_slice(&crate::transport::read_private_file(admission_path)?)?;
    admission.validate(
        build,
        image,
        proof_id,
        chrono::Utc::now().timestamp_millis(),
    )?;
    let id = ledger.register_build(&published).await?;
    ensure!(
        ledger.build_artifact(&id).await? == *published.artifact(),
        "ACK PG Build readback mismatch"
    );
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ack_import_rejects_expired_revoked_replayed_selectors_and_cancelled_producers() {
        let build = "a".repeat(64);
        let image = "b".repeat(64);
        let proof = "c".repeat(64);
        let mut admission = ImportAdmission {
            schema: 1,
            expires_ms: 1001,
            build_sha256: build.clone(),
            image_sha256: image.clone(),
            publication_proof_sha256: proof.clone(),
            revoked: false,
        };
        assert!(admission.validate(&build, &image, &proof, 1000).is_ok());
        assert!(admission.validate(&build, &image, &proof, 1001).is_err());
        assert!(admission
            .validate(&build, &image, &"d".repeat(64), 1000)
            .is_err());
        admission.revoked = true;
        assert!(admission.validate(&build, &image, &proof, 1000).is_err());
        let p = ReleaseProducer {
            repository: "owner/repo".into(),
            workflow_path: ".github/workflows/acr-publish.yml".into(),
            source_sha: "a".repeat(40),
            run_id: 10,
            run_attempt: 2,
            job_id: 30,
        };
        let run = json!({"id":10,"run_attempt":2,"head_sha":p.source_sha,"head_branch":"main","head_repository":{"full_name":"owner/repo"},"path":p.workflow_path,"event":"workflow_run","status":"completed","conclusion":"success"});
        let job = json!({"id":30,"run_id":10,"run_attempt":2,"head_sha":p.source_sha,"status":"completed","conclusion":"success","name":"Publish runner"});
        let image = format!("registry/runner@sha256:{}", "b".repeat(64));
        assert!(completed_producer(&run, &job, &p, true, &image).is_ok());
        for (key, value) in [
            ("conclusion", json!("cancelled")),
            ("status", json!("in_progress")),
            ("path", json!(".github/workflows/foreign.yml")),
            ("run_attempt", json!(3)),
            ("head_sha", json!("b".repeat(40))),
        ] {
            let mut bad = run.clone();
            bad[key] = value;
            assert!(completed_producer(&bad, &job, &p, true, &image).is_err());
        }
        let mut wrong = job.clone();
        wrong["name"] = json!("Publish foreign");
        assert!(completed_producer(&run, &wrong, &p, true, &image).is_err());
    }
    fn source() -> SourceArchive {
        SourceArchive {
            schema: 1,
            code_commit: "a".repeat(40),
            archive: Artifact {
                key: format!("research/sources/{}/source.tar", "a".repeat(40)),
                sha256: "b".repeat(64),
                bytes: 19,
            },
        }
    }
    fn inputs() -> CompilationInputs {
        CompilationInputs {
            schema: "monday.compilation-inputs.v3".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            profile: "release".into(),
            compiler: "a".repeat(64),
            native: "b".repeat(64),
            flags: "c".repeat(64),
            profiles: "d".repeat(64),
            recipe: "e".repeat(64),
            locks: BTreeMap::from([
                ("research-core/Cargo.lock".into(), "f".repeat(64)),
                ("data-pipelines/Cargo.lock".into(), "a".repeat(64)),
            ]),
            builder_image: format!("builder@sha256:{}", "a".repeat(64)),
            recipes: vec![
                CompilationRecipe {
                    manifest: "research-core/Cargo.toml".into(),
                    package: "science".into(),
                    features: "db".into(),
                    binaries: vec!["science".into()],
                },
                CompilationRecipe {
                    manifest: "data-pipelines/Cargo.toml".into(),
                    package: "collector".into(),
                    features: String::new(),
                    binaries: vec!["materializer".into(), "foreign".into()],
                },
            ],
            workspace_profiles: BTreeMap::from([
                ("research-core/Cargo.toml".into(), "d".repeat(64)),
                ("data-pipelines/Cargo.toml".into(), "e".repeat(64)),
            ]),
        }
    }
    #[test]
    fn release_publisher_configuration_requires_matching_operator_key_and_policy() {
        let key = SigningKey::from_bytes(&[17; 32]);
        let public = key
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut policy = PublisherPolicy {
            oss: None,
            trust: BuildReleaseTrust {
                schema: 1,
                repository: "owner/repo".into(),
                producer_workflow_path: ".github/workflows/acr-publish.yml".into(),
                keys: BTreeMap::from([("issuer".into(), public.clone())]),
            },
            key_id: "issuer".into(),
            builder_image: inputs().builder_image,
            image_repositories: BTreeMap::from([(
                "cex-runner".into(),
                "registry/research-runner".into(),
            )]),
            tls: Default::default(),
        };
        check_configuration(&policy, &key).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let manifest = temporary.path().join("software.json");
        let mut software = json!({
            "schema":"monday.research-image-release.v6",
            "products":["cex-runner"],
            "source_sha":"a".repeat(40),
            "workflow_run_id":"20",
            "workflow_run_attempt":1,
            "workflow_job_id":30,
            "target":"x86_64-unknown-linux-gnu",
            "build_inputs":inputs(),
            "cargo_locks":inputs().locks,
            "binaries":[]
        });
        std::fs::write(&manifest, serde_json::to_vec(&software).unwrap()).unwrap();
        check_publication_configuration(
            &policy,
            &key,
            &manifest,
            "owner/repo",
            "cex-runner",
            "registry/research-runner",
        )
        .unwrap();
        for (repository, product, image) in [
            ("foreign/repo", "cex-runner", "registry/research-runner"),
            ("owner/repo", "controller", "registry/research-runner"),
            ("owner/repo", "cex-runner", "registry/foreign"),
        ] {
            assert!(check_publication_configuration(
                &policy, &key, &manifest, repository, product, image
            )
            .is_err());
        }
        software["build_inputs"]["builder_image"] =
            json!(format!("builder@sha256:{}", "b".repeat(64)));
        std::fs::write(&manifest, serde_json::to_vec(&software).unwrap()).unwrap();
        assert!(check_publication_configuration(
            &policy,
            &key,
            &manifest,
            "owner/repo",
            "cex-runner",
            "registry/research-runner"
        )
        .is_err());
        policy.trust.keys.insert("issuer".into(), "0".repeat(64));
        assert!(check_configuration(&policy, &key).is_err());
        policy.trust.keys.insert("issuer".into(), public);
        policy.builder_image = "builder:latest".into();
        assert!(check_configuration(&policy, &key).is_err());
        policy.builder_image = inputs().builder_image;
        policy.image_repositories.clear();
        assert!(check_configuration(&policy, &key).is_err());
    }
    #[test]
    fn release_publisher_projects_actual_owned_subsets_with_exact_compiler_inputs() {
        let inputs = inputs();
        let names = BTreeSet::from(["science".into(), "materializer".into()]);
        let builds = project_builds(&source(), &inputs, &names).unwrap();
        assert_eq!(builds.len(), 2);
        assert_eq!(builds[0].workspace_manifest, "research-core/Cargo.toml");
        assert_eq!(builds[0].features, vec!["science/db"]);
        assert_eq!(builds[1].binaries, vec!["materializer"]);
        assert_eq!(builds[1].packages, vec!["collector"]);
        assert_eq!(builds[1].cargo_lock_sha256, "a".repeat(64));
        assert_eq!(builds[1].profile_manifest_sha256, "e".repeat(64));
        assert!(!builds[1]
            .cargo_arguments()
            .unwrap()
            .contains(&"foreign".into()));
        let mut changed = inputs.clone();
        changed.native = "f".repeat(64);
        assert_ne!(
            builds[0].id().unwrap(),
            project_builds(&source(), &changed, &names).unwrap()[0]
                .id()
                .unwrap()
        );
        assert!(project_builds(&source(), &inputs, &BTreeSet::from(["unbuilt".into()])).is_err());
        let mut ambiguous = inputs.clone();
        ambiguous.recipes.push(ambiguous.recipes[0].clone());
        assert!(project_builds(&source(), &ambiguous, &names).is_err());
        let mut missing = inputs.clone();
        missing.workspace_profiles.clear();
        assert!(project_builds(&source(), &missing, &names).is_err());
        let mut floating = inputs;
        floating.builder_image = "builder:latest".into();
        assert!(project_builds(&source(), &floating, &names).is_err());
    }
    fn checks() -> Value {
        json!([{"check_runs":[
            {"id":11,"name":"Monorepo CI gate","head_sha":"a".repeat(40),"status":"completed","conclusion":"success","app":{"id":15368,"slug":"github-actions"}},
            {"id":12,"name":"Prediction Markets CI gate","head_sha":"a".repeat(40),"status":"completed","conclusion":"success","app":{"id":15368,"slug":"github-actions"}},
            {"id":13,"name":"Security Summary Report","head_sha":"a".repeat(40),"status":"completed","conclusion":"success","app":{"id":15368,"slug":"github-actions"}}
        ]}])
    }
    #[test]
    fn release_publisher_rejects_wrong_sha_app_missing_skipped_and_new_pending_ci() {
        assert_eq!(
            required_checks(&checks(), &"a".repeat(40)).unwrap(),
            vec![11, 12, 13]
        );
        for (field, value) in [
            ("head_sha", json!("b".repeat(40))),
            ("conclusion", json!("skipped")),
            ("status", json!("in_progress")),
            ("app", json!({"id":1,"slug":"github-actions"})),
        ] {
            let mut altered = checks();
            altered[0]["check_runs"][0][field] = value;
            assert!(required_checks(&altered, &"a".repeat(40)).is_err());
        }
        let mut altered = checks();
        altered[0]["check_runs"].as_array_mut().unwrap().remove(1);
        assert!(required_checks(&altered, &"a".repeat(40)).is_err());
        let mut newer = checks();
        let mut pending = newer[0]["check_runs"][0].clone();
        pending["id"] = json!(99);
        pending["status"] = json!("in_progress");
        newer[0]["check_runs"].as_array_mut().unwrap().push(pending);
        assert!(required_checks(&newer, &"a".repeat(40)).is_err());
    }
    #[test]
    fn release_publisher_rejects_foreign_run_rerun_failed_and_wrong_job() {
        let sha = "a".repeat(40);
        let workflow = ".github/workflows/acr-publish.yml";
        let run = json!({"id":20,"run_attempt":2,"head_sha":sha,"head_branch":"main","head_repository":{"full_name":"owner/repo"},"path":workflow,"event":"workflow_run","status":"in_progress","conclusion":null});
        validate_run(&run, "owner/repo", &sha, 20, 2, workflow).unwrap();
        for (field, value) in [
            ("head_repository", json!({"full_name":"fork/repo"})),
            ("run_attempt", json!(3)),
            ("head_sha", json!("b".repeat(40))),
            ("conclusion", json!("cancelled")),
            ("path", json!(".github/workflows/fork.yml")),
        ] {
            let mut altered = run.clone();
            altered[field] = value;
            assert!(validate_run(&altered, "owner/repo", &sha, 20, 2, workflow).is_err());
        }
        let job = json!({"id":30,"run_id":20,"run_attempt":2,"head_sha":sha,"status":"in_progress","name":"Publish research-runner"});
        validate_job(&job, 20, 2, &sha, true).unwrap();
        for (field, value) in [
            ("run_attempt", json!(3)),
            ("run_id", json!(21)),
            ("head_sha", json!("b".repeat(40))),
            ("status", json!("completed")),
            ("name", json!("untrusted")),
        ] {
            let mut altered = job.clone();
            altered[field] = value;
            assert!(validate_job(&altered, 20, 2, &sha, true).is_err());
        }
    }
    #[test]
    fn release_publisher_measures_bytes_and_rejects_symlinks_and_absent_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("binary");
        std::fs::write(&file, b"actual bytes").unwrap();
        let measured = measure(&file, "research/builds/test/binary".into()).unwrap();
        assert_eq!(measured.sha256, sha256(b"actual bytes"));
        assert_eq!(measured.bytes, 12);
        std::fs::write(&file, b"changed").unwrap();
        assert_ne!(
            measure(&file, measured.key).unwrap().sha256,
            measured.sha256
        );
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(measure(&link, String::new()).is_err());
        assert!(read_signing_key(&tmp.path().join("missing")).is_err());
        use std::os::unix::fs::PermissionsExt;
        let key = tmp.path().join("key");
        std::fs::write(&key, "11".repeat(32)).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_signing_key(&key).is_ok());
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_signing_key(&key).is_err());
        assert!(read_signing_key(&link).is_err());
        let fifo = tmp.path().join("fifo");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(read_signing_key(&fifo).is_err());
    }
    #[test]
    fn release_publisher_requires_private_gateway_without_redirect_credentials() {
        for endpoint in [
            "http://gateway/",
            "https://token@gateway/",
            "https://gateway/?token=x",
            "https://gateway/#fragment",
            "https://gateway",
        ] {
            assert!(ReleaseGateway::new(endpoint, "token".into()).is_err());
        }
        assert!(ReleaseGateway::new("https://gateway/", String::new()).is_err());
    }
}
