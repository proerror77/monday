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
    #[serde(default)]
    pub import_admission_keys: BTreeMap<String, String>,
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
        vec!["api", "--paginate", &endpoint]
    } else {
        vec!["api", &endpoint]
    };
    decode_api_output(&command(root, "gh", &args, repository)?, pages)
}
fn decode_api_output(output: &[u8], pages: bool) -> Result<Value> {
    if !pages {
        return Ok(serde_json::from_slice(output)?);
    }
    // Older distro gh supports --paginate but not --slurp. Preserve the same
    // page array using the authenticated JSON stream, without dropping pages.
    let pages = serde_json::Deserializer::from_slice(output)
        .into_iter::<Value>()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        !pages.is_empty(),
        "GitHub pagination returned no JSON pages"
    );
    Ok(Value::Array(pages))
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
    if let Some(oss) = &gateway.oss {
        oss.require_publisher_scope(&publication_prefixes(&source, &builds)?)?;
    }
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
        recheck_publication_authority(
            |path, pages| api(root, repository, path, pages),
            request,
            policy,
            &software_producer,
            &check_ids,
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
        // Signed objects cannot be recalled. Failure blocks success and ACK import.
        recheck_publication_authority(
            |path, pages| api(root, repository, path, pages),
            request,
            policy,
            &software_producer,
            &check_ids,
        )?;
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
fn recheck_publication_authority(
    mut read: impl FnMut(&str, bool) -> Result<Value>,
    request: &PublicationRequest,
    policy: &PublisherPolicy,
    software: &ReleaseProducer,
    expected_checks: &[u64],
) -> Result<()> {
    let repo = &policy.trust.repository;
    ensure!(
        read("git/ref/heads/main", false)?["object"]["sha"] == request.source_sha,
        "publication source drifted from main"
    );
    validate_run(
        &read(&format!("actions/runs/{}", request.publisher_run_id), false)?,
        repo,
        &request.source_sha,
        request.publisher_run_id,
        request.publisher_run_attempt,
        &policy.trust.producer_workflow_path,
    )?;
    let job = read(&format!("actions/jobs/{}", request.publisher_job_id), false)?;
    validate_job(
        &job,
        request.publisher_run_id,
        request.publisher_run_attempt,
        &request.source_sha,
        true,
    )?;
    ensure!(
        job["name"]
            == format!(
                "Publish {}",
                request
                    .image
                    .rsplit_once("@sha256:")
                    .context("pinned image required")?
                    .0
                    .rsplit('/')
                    .next()
                    .context("image name")?
            ),
        "publisher job does not own the release"
    );
    validate_run(
        &read(&format!("actions/runs/{}", software.run_id), false)?,
        repo,
        &request.source_sha,
        software.run_id,
        software.run_attempt,
        &software.workflow_path,
    )?;
    validate_job(
        &read(&format!("actions/jobs/{}", software.job_id), false)?,
        software.run_id,
        software.run_attempt,
        &request.source_sha,
        false,
    )?;
    ensure!(
        required_checks(
            &read(
                &format!(
                    "commits/{}/check-runs?filter=latest&per_page=100",
                    request.source_sha
                ),
                true
            )?,
            &request.source_sha
        )? == expected_checks,
        "publication required checks changed"
    );
    Ok(())
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
/// Public approval material, without a future publisher identity or OCI digest.
#[derive(Serialize)]
pub struct ReleaseScopePlan {
    pub schema: u32,
    pub source: SourceArchive,
    pub builds: Vec<BuildSpec>,
    pub publisher_prefixes: Vec<String>,
}
#[allow(clippy::too_many_arguments)]
fn validate_scope_software(
    manifest: &SoftwareRelease,
    source: &str,
    software_run: u64,
    software_products: &str,
    product: &str,
    policy: &PublisherPolicy,
    run: &Value,
    job: &Value,
    completed_run: bool,
) -> Result<()> {
    let selected: BTreeSet<_> = software_products.split(',').collect();
    let produced: BTreeSet<_> = manifest.products.iter().map(String::as_str).collect();
    ensure!(
        manifest.schema == "monday.research-image-release.v6"
            && manifest.source_sha == source
            && manifest.workflow_run_id == software_run.to_string()
            && manifest.target == manifest.build_inputs.target
            && manifest.cargo_locks == manifest.build_inputs.locks
            && manifest.build_inputs.builder_image == policy.builder_image
            && pinned_image(&policy.builder_image)
            && policy.trust.schema == 1
            && policy.trust.producer_workflow_path == ".github/workflows/acr-publish.yml"
            && policy
                .image_repositories
                .get(product)
                .is_some_and(|r| !r.is_empty())
            && selected == produced
            && !selected.contains("")
            && selected.len() == software_products.split(',').count()
            && produced.len() == manifest.products.len()
            && selected.contains(product),
        "scope compiler/product provenance mismatch"
    );
    let workflow = run["path"].as_str().context("software workflow missing")?;
    ensure!(
        matches!(
            workflow,
            ".github/workflows/ploy-ci.yml" | ".github/workflows/acr-publish.yml"
        ),
        "untrusted scope software workflow"
    );
    let producer = ReleaseProducer {
        repository: policy.trust.repository.clone(),
        workflow_path: workflow.into(),
        source_sha: source.into(),
        run_id: software_run,
        run_attempt: manifest.workflow_run_attempt,
        job_id: manifest.workflow_job_id,
    };
    if completed_run {
        completed_producer(run, job, &producer, false, "")
    } else {
        validate_run(
            run,
            &producer.repository,
            source,
            software_run,
            producer.run_attempt,
            workflow,
        )?;
        validate_job(job, software_run, producer.run_attempt, source, false)?;
        ensure!(
            job["id"] == producer.job_id,
            "scope software job identity mismatch"
        );
        Ok(())
    }
}
#[allow(clippy::too_many_arguments)]
fn materialize_scope_plan(
    root: &Path,
    source_sha: &str,
    software_run: u64,
    software_products: &str,
    product: &str,
    policy: &PublisherPolicy,
    completed_run: bool,
) -> Result<ReleaseScopePlan> {
    let repository = &policy.trust.repository;
    ensure!(
        sha_is_valid(source_sha)
            && software_run > 0
            && repository.split('/').count() == 2
            && repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b)),
        "invalid scope source/repository"
    );
    ensure!(
        command(root, "git", &["rev-parse", "HEAD"], repository)?
            == format!("{source_sha}\n").as_bytes()
            && command(
                root,
                "git",
                &["status", "--porcelain", "--untracked-files=no"],
                repository
            )?
            .is_empty(),
        "scope plan requires clean exact source checkout"
    );
    let normalized = command(
        root,
        "bash",
        &[
            ".github/scripts/research-release-products.sh",
            "normalize",
            software_products,
        ],
        repository,
    )?;
    let normalized = std::str::from_utf8(&normalized)?.trim();
    let scratch = Scratch::new()?;
    let software = scratch.join("software");
    command(
        root,
        "bash",
        &[
            ".github/scripts/download-research-release.sh",
            &software_run.to_string(),
            source_sha,
            software.to_str().context("software path")?,
            normalized,
        ],
        repository,
    )?;
    let manifest: SoftwareRelease = read_json(&software.join("research-image-release.json"))?;
    let run = api(
        root,
        repository,
        &format!(
            "actions/runs/{software_run}/attempts/{}",
            manifest.workflow_run_attempt
        ),
        false,
    )?;
    let job = api(
        root,
        repository,
        &format!("actions/jobs/{}", manifest.workflow_job_id),
        false,
    )?;
    validate_scope_software(
        &manifest,
        source_sha,
        software_run,
        normalized,
        product,
        policy,
        &run,
        &job,
        completed_run,
    )?;
    let names = command(
        root,
        "bash",
        &[
            ".github/scripts/research-release-products.sh",
            "binaries",
            product,
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
            source_sha,
        ],
        repository,
    )?;
    let source = SourceArchive {
        schema: 1,
        code_commit: source_sha.into(),
        archive: measure(
            &archive,
            format!("research/sources/{source_sha}/source.tar"),
        )?,
    };
    project_scope_plan(source, &manifest.build_inputs, &names)
}
fn project_scope_plan(
    source: SourceArchive,
    inputs: &CompilationInputs,
    names: &BTreeSet<String>,
) -> Result<ReleaseScopePlan> {
    let builds = project_builds(&source, inputs, names)?;
    let publisher_prefixes = publication_prefixes(&source, &builds)?;
    Ok(ReleaseScopePlan {
        schema: 1,
        source,
        builds,
        publisher_prefixes,
    })
}
fn publication_prefixes(source: &SourceArchive, builds: &[BuildSpec]) -> Result<Vec<String>> {
    let mut publisher_prefixes = vec![format!("research/sources/{}/", source.code_commit)];
    for build in builds {
        publisher_prefixes.push(format!("research/builds/{}/", build.id()?));
    }
    publisher_prefixes.sort();
    ensure!(
        !builds.is_empty() && publisher_prefixes.windows(2).all(|w| w[0] < w[1]),
        "publication requires distinct actual source/Build prefixes"
    );
    Ok(publisher_prefixes)
}
/// Read-only pre-approval planning: no storage session, signing key or publisher.
pub fn scope_plan(
    root: &Path,
    source: &str,
    software_run: u64,
    software_products: &str,
    product: &str,
    policy: &PublisherPolicy,
) -> Result<ReleaseScopePlan> {
    check_source_authority(root, &policy.trust.repository, source)?;
    let scope = materialize_scope_plan(
        root,
        source,
        software_run,
        software_products,
        product,
        policy,
        true,
    )?;
    check_source_authority(root, &policy.trust.repository, source)?;
    Ok(scope)
}
/// Issuance still requires active authenticated publisher authority.
pub fn plan(
    root: &Path,
    request: &PublicationRequest,
    policy: &PublisherPolicy,
) -> Result<ReleasePlan> {
    authenticate(root, request, policy)?;
    let scope = materialize_scope_plan(
        root,
        &request.source_sha,
        request.software_run_id,
        &request.software_products,
        &request.product,
        policy,
        false,
    )?;
    Ok(ReleasePlan {
        schema: 1,
        image: request.image.clone(),
        source: scope.source,
        builds: scope.builds,
        publisher_prefixes: scope.publisher_prefixes,
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

/// Host-owned ACK approval. Updating this file never changes scientific grants.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportAdmission {
    pub schema: u32,
    pub revision: i64,
    pub expires_ms: i64,
    pub build_sha256: String,
    pub image_sha256: String,
    pub publication_proof_sha256: String,
    pub revoked: bool,
}
impl ImportAdmission {
    pub fn validate(&self, build: &str, image: &str, proof: &str, now: i64) -> Result<()> {
        ensure!(
            self.schema == 2
                && self.revision > 0
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedImportAdmission {
    pub schema: u32,
    pub key_id: String,
    pub admission: ImportAdmission,
    pub signature_hex: String,
}
impl SignedImportAdmission {
    /// Operator-only utility: consume an existing approved key, never generate one.
    pub fn sign(
        admission: ImportAdmission,
        key_id: String,
        key: &SigningKey,
        policy: &PublisherPolicy,
        now: i64,
    ) -> Result<Self> {
        ensure!(
            admission.schema == 2
                && admission.revision > 0
                && admission.expires_ms > now
                && valid_digest(&admission.build_sha256)
                && valid_digest(&admission.image_sha256)
                && valid_digest(&admission.publication_proof_sha256),
            "operator admission requires exact selectors and future expiry"
        );
        let mut signed = Self {
            schema: 2,
            key_id,
            admission,
            signature_hex: String::new(),
        };
        signed.signature_hex = key
            .sign(&signed.signing_bytes()?)
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        // Includes independent pinned-key enforcement, not merely a valid signature.
        signed.verify(policy)?;
        Ok(signed)
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = b"monday.ack-build-import-admission.v2\0".to_vec();
        bytes.extend(serde_json::to_vec(&(
            self.schema,
            &self.key_id,
            &self.admission,
        ))?);
        Ok(bytes)
    }
    pub fn verify(&self, policy: &PublisherPolicy) -> Result<&ImportAdmission> {
        use ed25519_dalek::{Signature, VerifyingKey};
        let public = policy
            .import_admission_keys
            .get(&self.key_id)
            .context("unknown independent ACK admission key")?;
        ensure!(
            self.schema == 2
                && valid_digest(public)
                && !policy.trust.keys.values().any(|key| key == public)
                && self.signature_hex.len() == 128
                && self
                    .signature_hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "ACK admission requires a distinct operator key and canonical signature"
        );
        let decode = |hex: &str| -> Result<Vec<u8>> {
            (0..hex.len())
                .step_by(2)
                .map(|i| Ok(u8::from_str_radix(&hex[i..i + 2], 16)?))
                .collect()
        };
        let key = VerifyingKey::from_bytes(
            &decode(public)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid ACK public key"))?,
        )?;
        ensure!(!key.is_weak(), "weak ACK admission key");
        let signature = Signature::from_slice(&decode(&self.signature_hex)?)?;
        key.verify_strict(&self.signing_bytes()?, &signature)
            .map_err(|_| anyhow::anyhow!("ACK admission signature rejected"))?;
        Ok(&self.admission)
    }
}
/// Signature-verified independent approval; only its owner may project it to PG.
pub struct VerifiedImportAdmission {
    signed: SignedImportAdmission,
    envelope_sha256: String,
}
impl VerifiedImportAdmission {
    pub fn from_signed(signed: SignedImportAdmission, policy: &PublisherPolicy) -> Result<Self> {
        signed.verify(policy)?;
        let a = &signed.admission;
        ensure!(
            a.schema == 2
                && a.revision > 0
                && valid_digest(&a.build_sha256)
                && valid_digest(&a.image_sha256)
                && valid_digest(&a.publication_proof_sha256),
            "exact independent admission selectors required"
        );
        let envelope_sha256 = identity(&signed)?;
        Ok(Self {
            signed,
            envelope_sha256,
        })
    }
    pub fn admission(&self) -> &ImportAdmission {
        &self.signed.admission
    }
    pub(crate) fn envelope_sha256(&self) -> &str {
        &self.envelope_sha256
    }
    pub(crate) fn document(&self) -> Result<Value> {
        Ok(serde_json::to_value(&self.signed)?)
    }
    pub(crate) fn validate_release(
        &self,
        published: &VerifiedPublishedBuildRelease,
        now: i64,
    ) -> Result<()> {
        let receipt = &published.verified().signed().receipt;
        self.admission().validate(
            &receipt.build_sha256,
            receipt
                .image
                .rsplit_once("@sha256:")
                .context("pinned import image")?
                .1,
            &receipt.publication_readback_sha256,
            now,
        )
    }
}
pub fn read_import_admission(
    path: &Path,
    policy: &PublisherPolicy,
) -> Result<VerifiedImportAdmission> {
    let signed: SignedImportAdmission =
        serde_json::from_slice(&crate::transport::read_private_file(path)?)?;
    VerifiedImportAdmission::from_signed(signed, policy)
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
// Issuance binds current main. Later import binds the signed original identities.
fn recheck_import_authority(
    mut read: impl FnMut(&str) -> Result<Value>,
    proof: &PublicationProof,
) -> Result<()> {
    for (producer, publisher) in [(&proof.software_producer, false), (&proof.producer, true)] {
        let run = read(&format!(
            "actions/runs/{}/attempts/{}",
            producer.run_id, producer.run_attempt
        ))?;
        let job = read(&format!("actions/jobs/{}", producer.job_id))?;
        completed_producer(&run, &job, producer, publisher, &proof.image)?;
    }
    let names = [
        "Monorepo CI gate",
        "Prediction Markets CI gate",
        "Security Summary Report",
    ];
    ensure!(
        proof.required_check_ids.len() == names.len()
            && proof
                .required_check_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                == names.len(),
        "original required check identities missing or duplicated"
    );
    for (id, name) in proof.required_check_ids.iter().zip(names) {
        let check = read(&format!("check-runs/{id}"))?;
        ensure!(
            *id > 0
                && check["id"] == *id
                && check["name"] == name
                && check["head_sha"] == proof.source.code_commit
                && check["app"]["slug"] == "github-actions"
                && check["app"]["id"] == 15368
                && check["status"] == "completed"
                && check["conclusion"] == "success",
            "original authenticated release check rejected"
        );
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
    let admission = read_import_admission(admission_path, policy)?;
    admission.admission().validate(
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
    recheck_import_authority(
        |path| api(root, &policy.trust.repository, path, false),
        &proof,
    )?;
    let admission = read_import_admission(admission_path, policy)?;
    admission.admission().validate(
        build,
        image,
        proof_id,
        chrono::Utc::now().timestamp_millis(),
    )?;
    let id = ledger.register_build(&published, &admission).await?;
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
    fn github_pagination_preserves_all_streamed_pages_and_rejects_bad_output() {
        let output = b"{\"jobs\":[]}\n{\"jobs\":[{\"id\":567}]}\n";
        assert_eq!(
            decode_api_output(output, true).unwrap(),
            json!([{"jobs":[]},{"jobs":[{"id":567}]}])
        );
        assert!(decode_api_output(output, false).is_err());
        assert!(decode_api_output(b"", true).is_err());
        assert!(decode_api_output(b"{}\n{malformed}", true).is_err());
        assert!(decode_api_output(b"{}\nunauthenticated error", true).is_err());
        assert_eq!(
            decode_api_output(b"{\"id\":567}", false).unwrap(),
            json!({"id":567})
        );
    }
    #[test]
    fn ack_import_rejects_expired_revoked_replayed_selectors_and_cancelled_producers() {
        let build = "a".repeat(64);
        let image = "b".repeat(64);
        let proof = "c".repeat(64);
        let mut admission = ImportAdmission {
            schema: 2,
            revision: 1,
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
    #[test]
    fn ack_admission_requires_distinct_authority_signature() {
        let ci = SigningKey::from_bytes(&[17; 32]);
        let operator = SigningKey::from_bytes(&[18; 32]);
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let mut policy: PublisherPolicy = serde_json::from_value(json!({
            "trust":{"schema":1,"repository":"owner/repo","producer_workflow_path":".github/workflows/acr-publish.yml","keys":{"ci":hex(&ci.verifying_key().to_bytes())}},
            "key_id":"ci","builder_image":inputs().builder_image,"image_repositories":{},
            "import_admission_keys":{"operator":hex(&operator.verifying_key().to_bytes())}
        })).unwrap();
        let mut signed = SignedImportAdmission {
            schema: 2,
            key_id: "operator".into(),
            admission: ImportAdmission {
                schema: 2,
                revision: 1,
                expires_ms: 1001,
                build_sha256: "a".repeat(64),
                image_sha256: "b".repeat(64),
                publication_proof_sha256: "c".repeat(64),
                revoked: false,
            },
            signature_hex: String::new(),
        };
        let admission = signed.admission.clone();
        let generated = SignedImportAdmission::sign(
            admission.clone(),
            "operator".into(),
            &operator,
            &policy,
            1000,
        )
        .unwrap();
        assert!(generated.verify(&policy).is_ok());
        generated
            .admission
            .validate(&"a".repeat(64), &"b".repeat(64), &"c".repeat(64), 1000)
            .unwrap();
        assert!(SignedImportAdmission::sign(
            admission.clone(),
            "operator".into(),
            &ci,
            &policy,
            1000
        )
        .is_err());
        assert!(SignedImportAdmission::sign(
            admission.clone(),
            "unknown".into(),
            &operator,
            &policy,
            1000
        )
        .is_err());
        assert!(SignedImportAdmission::sign(
            admission.clone(),
            "operator".into(),
            &operator,
            &policy,
            1001
        )
        .is_err());
        let mut malformed = admission.clone();
        malformed.build_sha256 = "*".into();
        assert!(SignedImportAdmission::sign(
            malformed,
            "operator".into(),
            &operator,
            &policy,
            1000
        )
        .is_err());
        let mut zero_revision = admission.clone();
        zero_revision.revision = 0;
        assert!(SignedImportAdmission::sign(
            zero_revision,
            "operator".into(),
            &operator,
            &policy,
            1000
        )
        .is_err());
        let mut revoked = admission;
        revoked.revoked = true;
        let revoked =
            SignedImportAdmission::sign(revoked, "operator".into(), &operator, &policy, 1000)
                .unwrap();
        assert!(revoked.verify(&policy).is_ok());
        assert!(revoked
            .admission
            .validate(&"a".repeat(64), &"b".repeat(64), &"c".repeat(64), 1000)
            .is_err());
        signed.signature_hex = hex(&operator.sign(&signed.signing_bytes().unwrap()).to_bytes());
        assert!(signed.verify(&policy).is_ok());
        signed.admission.revoked = true;
        assert!(signed.verify(&policy).is_err());
        signed.admission.revoked = false;
        signed.signature_hex = hex(&ci.sign(&signed.signing_bytes().unwrap()).to_bytes());
        assert!(signed.verify(&policy).is_err());
        policy
            .import_admission_keys
            .insert("operator".into(), hex(&ci.verifying_key().to_bytes()));
        assert!(signed.verify(&policy).is_err());
        assert!(serde_json::from_value::<SignedImportAdmission>(
            serde_json::to_value(&signed.admission).unwrap()
        )
        .is_err());
    }
    #[test]
    fn post_signature_recheck_rejects_cancel_drift_attempt_workflow_and_check_replacement() {
        let sha = "a".repeat(40);
        let request = PublicationRequest {
            source_sha: sha.clone(),
            software_run_id: 20,
            software_products: "cex-runner".into(),
            product: "cex-runner".into(),
            image: format!("registry/runner@sha256:{}", "b".repeat(64)),
            publisher_run_id: 10,
            publisher_run_attempt: 2,
            publisher_job_id: 30,
        };
        let policy: PublisherPolicy = serde_json::from_value(json!({"trust":{"schema":1,"repository":"owner/repo","producer_workflow_path":".github/workflows/acr-publish.yml","keys":{}},"key_id":"ci","builder_image":inputs().builder_image,"image_repositories":{}})).unwrap();
        let software = ReleaseProducer {
            repository: "owner/repo".into(),
            workflow_path: ".github/workflows/ci.yml".into(),
            source_sha: sha.clone(),
            run_id: 20,
            run_attempt: 1,
            job_id: 40,
        };
        let run = |id, attempt, path: &str| json!({"id":id,"run_attempt":attempt,"head_sha":sha,"head_branch":"main","head_repository":{"full_name":"owner/repo"},"path":path,"event":"workflow_run","status":"in_progress","conclusion":null});
        let checks = ["Monorepo CI gate","Prediction Markets CI gate","Security Summary Report"].iter().enumerate().map(|(i,name)| json!({"id":i+1,"name":name,"head_sha":sha,"status":"completed","conclusion":"success","app":{"slug":"github-actions","id":15368}})).collect::<Vec<_>>();
        let baseline = BTreeMap::from([
            (
                "git/ref/heads/main".to_owned(),
                json!({"object":{"sha":sha}}),
            ),
            (
                "actions/runs/10".into(),
                run(10, 2, ".github/workflows/acr-publish.yml"),
            ),
            (
                "actions/runs/20".into(),
                run(20, 1, ".github/workflows/ci.yml"),
            ),
            (
                "actions/jobs/30".into(),
                json!({"id":30,"run_id":10,"run_attempt":2,"head_sha":sha,"status":"in_progress","name":"Publish runner"}),
            ),
            (
                "actions/jobs/40".into(),
                json!({"id":40,"run_id":20,"run_attempt":1,"head_sha":sha,"status":"completed","conclusion":"success","name":"Research image binaries"}),
            ),
            (
                format!("commits/{sha}/check-runs?filter=latest&per_page=100"),
                json!([{"check_runs":checks}]),
            ),
        ]);
        let verify = |data: &BTreeMap<String, Value>| {
            recheck_publication_authority(
                |path, _| data.get(path).cloned().context("missing fixture"),
                &request,
                &policy,
                &software,
                &[1, 2, 3],
            )
        };
        assert!(verify(&baseline).is_ok()); // Before signing.
        for kind in 0..6 {
            let mut after = baseline.clone();
            match kind {
                0 => {
                    after.get_mut("git/ref/heads/main").unwrap()["object"]["sha"] =
                        json!("b".repeat(40))
                }
                1 => after.get_mut("actions/runs/10").unwrap()["conclusion"] = json!("cancelled"),
                2 => after.get_mut("actions/jobs/30").unwrap()["status"] = json!("completed"),
                3 => after.get_mut("actions/runs/10").unwrap()["run_attempt"] = json!(3),
                4 => {
                    after.get_mut("actions/runs/10").unwrap()["path"] =
                        json!(".github/workflows/foreign.yml")
                }
                _ => {
                    after
                        .get_mut(&format!(
                            "commits/{sha}/check-runs?filter=latest&per_page=100"
                        ))
                        .unwrap()[0]["check_runs"][0]["id"] = json!(99)
                }
            }
            assert!(verify(&after).is_err()); // After signing/upload readbacks.
        }
    }
    #[test]
    fn historical_import_binds_original_attempts_and_checks_after_main_or_latest_drift() {
        let source = source();
        let sha = &source.code_commit;
        let producer = ReleaseProducer {
            repository: "owner/repo".into(),
            workflow_path: ".github/workflows/acr-publish.yml".into(),
            source_sha: sha.clone(),
            run_id: 10,
            run_attempt: 2,
            job_id: 30,
        };
        let software = ReleaseProducer {
            workflow_path: ".github/workflows/ploy-ci.yml".into(),
            run_id: 20,
            run_attempt: 1,
            job_id: 40,
            ..producer.clone()
        };
        let mut proof = PublicationProof {
            schema: 1,
            source: source.clone(),
            image: format!("registry/runner@sha256:{}", "b".repeat(64)),
            producer,
            software_producer: software,
            required_check_ids: vec![1, 2, 3],
            compilation_inputs: inputs(),
            executables: vec![],
            build_sha256: "a".repeat(64),
        };
        let run = |id, attempt, path: &str| json!({"id":id,"run_attempt":attempt,"head_sha":sha,"head_branch":"main","head_repository":{"full_name":"owner/repo"},"path":path,"event":"workflow_run","status":"completed","conclusion":"success"});
        let mut original = BTreeMap::from([
            (
                "actions/runs/10/attempts/2".to_owned(),
                run(10, 2, ".github/workflows/acr-publish.yml"),
            ),
            (
                "actions/runs/20/attempts/1".into(),
                run(20, 1, ".github/workflows/ploy-ci.yml"),
            ),
            (
                "actions/jobs/30".into(),
                json!({"id":30,"run_id":10,"run_attempt":2,"head_sha":sha,"status":"completed","conclusion":"success","name":"Publish runner"}),
            ),
            (
                "actions/jobs/40".into(),
                json!({"id":40,"run_id":20,"run_attempt":1,"head_sha":sha,"status":"completed","conclusion":"success","name":"Research image binaries"}),
            ),
            // Current main and latest attempts/checks have changed. They are not imported identities.
            (
                "git/ref/heads/main".into(),
                json!({"object":{"sha":"f".repeat(40)}}),
            ),
            (
                "actions/runs/10".into(),
                run(10, 3, ".github/workflows/acr-publish.yml"),
            ),
            (
                format!("commits/{sha}/check-runs?filter=latest&per_page=100"),
                json!([{"check_runs":[]}]),
            ),
        ]);
        for (i, name) in [
            "Monorepo CI gate",
            "Prediction Markets CI gate",
            "Security Summary Report",
        ]
        .iter()
        .enumerate()
        {
            original.insert(format!("check-runs/{}",i+1),json!({"id":i+1,"name":name,"head_sha":sha,"status":"completed","conclusion":"success","app":{"slug":"github-actions","id":15368}}));
        }
        let verify = |data: &BTreeMap<String, Value>, proof: &PublicationProof| {
            recheck_import_authority(
                |path| data.get(path).cloned().context("missing original identity"),
                proof,
            )
        };
        assert!(verify(&original, &proof).is_ok());
        for (path, key, value) in [
            (
                "actions/runs/10/attempts/2",
                "conclusion",
                json!("cancelled"),
            ),
            ("actions/runs/10/attempts/2", "run_attempt", json!(3)),
            ("actions/jobs/30", "status", json!("in_progress")),
            ("check-runs/1", "head_sha", json!("f".repeat(40))),
            ("check-runs/1", "id", json!(99)),
            ("check-runs/1", "conclusion", json!("failure")),
            ("check-runs/1", "name", json!("foreign gate")),
            ("check-runs/1", "app", json!({"slug":"foreign","id":15368})),
        ] {
            let mut bad = original.clone();
            bad.get_mut(path).unwrap()[key] = value;
            assert!(verify(&bad, &proof).is_err());
        }
        let mut missing = original.clone();
        missing.remove("check-runs/1");
        assert!(verify(&missing, &proof).is_err());
        proof.required_check_ids = vec![1, 1, 3];
        assert!(verify(&original, &proof).is_err());
    }
    #[test]
    fn readonly_scope_binds_completed_software_and_exact_compiler_products_without_publisher() {
        let sha = "a".repeat(40);
        let policy: PublisherPolicy=serde_json::from_value(json!({
            "trust":{"schema":1,"repository":"owner/repo","producer_workflow_path":".github/workflows/acr-publish.yml","keys":{}},
            "key_id":"ci","builder_image":inputs().builder_image,"image_repositories":{"cex-runner":"registry/runner"}
        })).unwrap();
        let manifest = json!({"schema":"monday.research-image-release.v6","products":["cex-runner"],
            "source_sha":sha,"workflow_run_id":"20","workflow_run_attempt":1,"workflow_job_id":30,
            "target":inputs().target,"build_inputs":inputs(),"cargo_locks":inputs().locks,"binaries":[]});
        let run = json!({"id":20,"run_attempt":1,"head_sha":sha,"head_branch":"main","head_repository":{"full_name":"owner/repo"},
            "path":".github/workflows/ploy-ci.yml","event":"push","status":"completed","conclusion":"success"});
        let job = json!({"id":30,"run_id":20,"run_attempt":1,"head_sha":sha,"name":"Research image binaries","status":"completed","conclusion":"success"});
        let verify = |m: Value, r: &Value, j: &Value, products: &str| {
            validate_scope_software(
                &serde_json::from_value(m).unwrap(),
                &sha,
                20,
                products,
                "cex-runner",
                &policy,
                r,
                j,
                true,
            )
        };
        assert!(verify(manifest.clone(), &run, &job, "cex-runner").is_ok());
        for (path, value) in [
            ("/source_sha", json!("b".repeat(40))),
            ("/workflow_run_id", json!("21")),
            ("/workflow_run_attempt", json!(2)),
            ("/workflow_job_id", json!(31)),
            ("/target", json!("aarch64-unknown-linux-gnu")),
            ("/cargo_locks", json!({})),
            (
                "/build_inputs/builder_image",
                json!(format!("foreign@sha256:{}", "b".repeat(64))),
            ),
            ("/products", json!(["cex-runner", "cex-runner"])),
            ("/products", json!(["controller"])),
        ] {
            let mut bad = manifest.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(
                verify(bad, &run, &job, "cex-runner").is_err(),
                "accepted manifest mutation {path}"
            );
        }
        for (key, value) in [
            ("conclusion", json!("cancelled")),
            ("status", json!("in_progress")),
            ("path", json!(".github/workflows/foreign.yml")),
            ("head_sha", json!("b".repeat(40))),
            ("run_attempt", json!(2)),
            ("head_branch", json!("foreign")),
            ("head_repository", json!({"full_name":"foreign/repo"})),
        ] {
            let mut bad = run.clone();
            bad[key] = value;
            assert!(verify(manifest.clone(), &bad, &job, "cex-runner").is_err());
        }
        for (key, value) in [
            ("id", json!(31)),
            ("conclusion", json!("failure")),
            ("status", json!("in_progress")),
            ("run_attempt", json!(2)),
            ("name", json!("foreign")),
        ] {
            let mut bad = job.clone();
            bad[key] = value;
            assert!(verify(manifest.clone(), &run, &bad, "cex-runner").is_err());
        }
        for products in [
            "cex-runner,cex-runner",
            "cex-runner,controller",
            "controller",
            "",
        ] {
            assert!(verify(manifest.clone(), &run, &job, products).is_err());
        }
        // Rebuild within an active publication workflow remains supported for
        // issuance, but cannot be used as an independently completed scope plan.
        let mut active = run;
        active["path"] = json!(".github/workflows/acr-publish.yml");
        active["event"] = json!("workflow_dispatch");
        active["status"] = json!("in_progress");
        active["conclusion"] = Value::Null;
        let manifest: SoftwareRelease = serde_json::from_value(manifest).unwrap();
        assert!(validate_scope_software(
            &manifest,
            &sha,
            20,
            "cex-runner",
            "cex-runner",
            &policy,
            &active,
            &job,
            false
        )
        .is_ok());
        assert!(validate_scope_software(
            &manifest,
            &sha,
            20,
            "cex-runner",
            "cex-runner",
            &policy,
            &active,
            &job,
            true
        )
        .is_err());
        let names = inputs().recipes[0].binaries.iter().cloned().collect();
        let scope = project_scope_plan(source(), &inputs(), &names).unwrap();
        let expected = project_builds(&source(), &inputs(), &names).unwrap();
        assert_eq!(
            serde_json::to_value(&scope.builds).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        for build in &scope.builds {
            assert!(scope
                .publisher_prefixes
                .contains(&format!("research/builds/{}/", build.id().unwrap())));
        }
        let value = serde_json::to_value(scope).unwrap();
        assert!(value.get("image").is_none() && value.get("publisher_run_id").is_none());
    }
    #[test]
    fn scope_plan_rejects_dirty_tracked_scripts_before_any_software_or_cloud_read() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(out.status.success());
            out.stdout
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "Fixture"]);
        std::fs::write(root.join("guard.sh"), "committed").unwrap();
        git(&["add", "guard.sh"]);
        git(&["commit", "--quiet", "-m", "fixture"]);
        let sha = String::from_utf8(git(&["rev-parse", "HEAD"])).unwrap();
        std::fs::write(root.join("guard.sh"), "changed").unwrap();
        let policy:PublisherPolicy=serde_json::from_value(json!({"trust":{"schema":1,"repository":"owner/repo","producer_workflow_path":".github/workflows/acr-publish.yml","keys":{}},"key_id":"ci","builder_image":inputs().builder_image,"image_repositories":{}})).unwrap();
        let error = materialize_scope_plan(
            root,
            sha.trim(),
            20,
            "cex-runner",
            "cex-runner",
            &policy,
            true,
        )
        .err()
        .unwrap()
        .to_string();
        assert_eq!(error, "scope plan requires clean exact source checkout");
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
            import_admission_keys: BTreeMap::new(),
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
