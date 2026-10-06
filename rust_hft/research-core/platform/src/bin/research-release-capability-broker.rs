//! Verify GitHub authority and native producer inputs before issuing object access.
use anyhow::{ensure, Context, Result};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use hft_research_platform::{
    artifact_identity::{Access, Capability},
    build::pinned_image,
    identity,
    release_publisher::{read_json, PublisherPolicy},
    sha256,
    transport::TlsConfig,
};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, process::Command};

#[path = "../release_capability_contract.rs"]
mod contract;
use contract::{Phase, Plan, Request, Response};
const ISSUER: &str = "https://token.actions.githubusercontent.com";
const JWKS: &str = "https://token.actions.githubusercontent.com/.well-known/jwks";
const HOUR_MS: u64 = 3_600_000;
const LEASE_MS: u64 = 120_000;
const MAX_JSON: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    bind: String,
    endpoint: String,
    policy_file: PathBuf,
    repository_id: u64,
    owner_id: u64,
    capabilities_file: PathBuf,
    replay_file: PathBuf,
    scratch_root: PathBuf,
    publisher_binary: PathBuf,
    verifier_sandbox: PathBuf,
    tools_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    jti: String,
    exp: u64,
    nbf: u64,
    iat: u64,
    repository: String,
    #[serde(deserialize_with = "numeric_claim")]
    repository_id: u64,
    #[serde(deserialize_with = "numeric_claim")]
    repository_owner_id: u64,
    #[serde(rename = "ref")]
    reference: String,
    sha: String,
    workflow_ref: String,
    workflow_sha: String,
    #[serde(deserialize_with = "numeric_claim")]
    run_id: u64,
    #[serde(deserialize_with = "numeric_claim")]
    run_attempt: u64,
    #[serde(deserialize_with = "numeric_claim")]
    check_run_id: u64,
}
fn numeric_claim<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    let value = Value::deserialize(d)?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| *n > 0)
        .ok_or_else(|| serde::de::Error::custom("positive numeric claim required"))
}
#[derive(Clone, Deserialize)]
struct Key {
    kid: String,
    kty: String,
    n: String,
    e: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(rename = "use", default)]
    usage: Option<String>,
}
#[derive(Clone, Deserialize)]
struct KeySet {
    keys: Vec<Key>,
}

// This type cannot be built from caller context. Its constructor verifies RS256.
struct JobIdentity(Claims);
fn verify_identity(
    token: &str,
    keys: &KeySet,
    config: &Config,
    policy: &PublisherPolicy,
    request: &Request,
    now: u64,
) -> Result<JobIdentity> {
    ensure!(token.len() <= 16 * 1024, "OIDC identity exceeds bound");
    let header = decode_header(token).map_err(|_| anyhow::anyhow!("invalid OIDC identity"))?;
    ensure!(header.alg == Algorithm::RS256, "untrusted OIDC algorithm");
    let kid = header
        .kid
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .context("OIDC key identifier required")?;
    let found: Vec<_> = keys.keys.iter().filter(|k| k.kid == kid).collect();
    ensure!(found.len() == 1, "unknown or ambiguous GitHub OIDC key");
    let key = found[0];
    ensure!(
        key.kty == "RSA"
            && key.alg.as_deref().is_none_or(|a| a == "RS256")
            && key.usage.as_deref().is_none_or(|u| u == "sig"),
        "invalid GitHub OIDC key"
    );
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = 0;
    validation.validate_nbf = true;
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[&config.endpoint]);
    validation.set_required_spec_claims(&["iss", "aud", "sub", "exp", "nbf", "iat"]);
    let decoding = DecodingKey::from_rsa_components(&key.n, &key.e)
        .map_err(|_| anyhow::anyhow!("invalid GitHub RSA key"))?;
    let claims = decode::<Claims>(token, &decoding, &validation)
        .map_err(|_| anyhow::anyhow!("GitHub OIDC signature/claims rejected"))?
        .claims;
    ensure!(
        claims.exp > now / 1000
            && claims.nbf <= now / 1000
            && claims.iat <= now / 1000
            && claims.exp > claims.iat
            && claims.exp - claims.iat <= 900
            && !claims.jti.is_empty()
            && claims.jti.len() <= 256
            && claims.repository == policy.trust.repository
            && claims.repository == request.context.repository
            && claims.sub == format!("repo:{}:ref:refs/heads/main", policy.trust.repository)
            && claims.repository_id == config.repository_id
            && claims.repository_owner_id == config.owner_id
            && claims.reference == "refs/heads/main"
            && claims.sha == request.context.source_sha
            && claims.workflow_sha == claims.sha
            && claims.workflow_ref
                == format!(
                    "{}/{}@refs/heads/main",
                    policy.trust.repository, policy.trust.producer_workflow_path
                )
            && claims.run_id == request.context.publisher_run_id
            && claims.run_attempt == u64::from(request.context.publisher_run_attempt),
        "GitHub identity is outside this repository/workflow/source/run"
    );
    Ok(JobIdentity(claims))
}

struct Evidence {
    repository: Value,
    main: Value,
    publisher: Value,
    job: Value,
    producer: Value,
    checks: Vec<Value>,
    producer_jobs: Vec<Value>,
}
fn verify_evidence(
    e: &Evidence,
    identity: &JobIdentity,
    config: &Config,
    policy: &PublisherPolicy,
    request: &Request,
) -> Result<()> {
    let c = &request.context;
    let claims = &identity.0;
    ensure!(
        e.repository["id"] == config.repository_id
            && e.repository["owner"]["id"] == config.owner_id
            && e.repository["full_name"] == policy.trust.repository,
        "repository identity changed"
    );
    ensure!(
        e.main["object"]["sha"] == c.source_sha,
        "source is no longer current main"
    );
    ensure!(
        e.publisher["id"] == c.publisher_run_id
            && e.publisher["run_attempt"] == c.publisher_run_attempt
            && e.publisher["head_sha"] == c.source_sha
            && e.publisher["head_branch"] == "main"
            && e.publisher["head_repository"]["id"] == config.repository_id
            && e.publisher["path"] == policy.trust.producer_workflow_path
            && matches!(
                e.publisher["event"].as_str(),
                Some("workflow_run" | "workflow_dispatch")
            )
            && !matches!(
                e.publisher["conclusion"].as_str(),
                Some("failure" | "cancelled" | "timed_out" | "skipped")
            ),
        "publisher run changed or was cancelled"
    );
    ensure!(
        job_matches(c, claims.check_run_id, &e.job),
        "signed job identity does not own the selected image"
    );
    let automatic = e.producer["path"] == ".github/workflows/ploy-ci.yml"
        && e.producer["event"] == "push"
        && e.producer["status"] == "completed"
        && e.producer["conclusion"] == "success";
    let manual = e.producer["path"] == policy.trust.producer_workflow_path
        && e.producer["event"] == "workflow_dispatch"
        && e.producer["id"] == c.publisher_run_id
        && e.producer["run_attempt"] == c.publisher_run_attempt;
    ensure!(
        e.producer["id"] == c.software_run_id
            && e.producer["head_sha"] == c.source_sha
            && e.producer["head_branch"] == "main"
            && e.producer["head_repository"]["id"] == config.repository_id
            && (automatic || manual),
        "untrusted software producer"
    );
    let name = if automatic {
        "Research image binaries"
    } else {
        "Research release binaries"
    };
    ensure!(
        e.producer_jobs
            .iter()
            .filter(|j| j["name"] == name
                && j["run_id"] == c.software_run_id
                && j["run_attempt"] == e.producer["run_attempt"]
                && j["status"] == "completed"
                && j["conclusion"] == "success")
            .count()
            == 1,
        "successful software producer job absent or ambiguous"
    );
    for name in [
        "Monorepo CI gate",
        "Prediction Markets CI gate",
        "Security Summary Report",
    ] {
        let check = e
            .checks
            .iter()
            .filter(|x| {
                x["name"] == name && x["app"]["id"] == 15368 && x["app"]["slug"] == "github-actions"
            })
            .max_by_key(|x| x["id"].as_u64().unwrap_or_default())
            .context("authenticated required CI absent")?;
        ensure!(
            check["head_sha"] == c.source_sha
                && check["status"] == "completed"
                && check["conclusion"] == "success",
            "required exact-source CI did not pass"
        );
    }
    Ok(())
}

fn validate_request(request: &Request, policy: &PublisherPolicy, now: u64) -> Result<()> {
    let c = &request.context;
    ensure!(
        request.schema == 1
            && request.expires_ms > now
            && request.expires_ms - now <= HOUR_MS
            && c.repository == policy.trust.repository
            && policy.trust.schema == 1
            && policy.trust.producer_workflow_path == ".github/workflows/acr-publish.yml"
            && policy.image_repositories.get(&c.product) == Some(&c.image_repository)
            && c.source_sha.len() == 40
            && c.source_sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && c.software_run_id > 0
            && c.publisher_run_id > 0
            && c.publisher_run_attempt > 0
            && c.publisher_job_id > 0,
        "invalid requested publication context/lifetime"
    );
    Ok(())
}
fn scope(
    request: &Request,
    policy: &PublisherPolicy,
    plan: Option<&Plan>,
    now: u64,
) -> Result<Access> {
    let c = &request.context;
    validate_request(request, policy, now)?;
    let mut prefixes = BTreeSet::from([format!("research/sources/{}/", c.source_sha)]);
    if request.phase == Phase::Source {
        ensure!(
            request.image.is_none() && request.plan_sha256.is_none() && plan.is_none(),
            "source read cannot claim a Build"
        );
    } else {
        let plan = plan.context("independently verified native plan required")?;
        ensure!(
            plan.schema == 1
                && request.image.as_ref() == Some(&plan.image)
                && pinned_image(&plan.image)
                && plan.image.rsplit_once("@sha256:").map(|(repo, _)| repo)
                    == Some(c.image_repository.as_str())
                && plan.source.schema == 1
                && plan.source.code_commit == c.source_sha
                && !plan.builds.is_empty()
                && plan.builds.len() <= 255
                && request.plan_sha256.as_ref() == Some(&identity(plan)?),
            "native plan/image identity disagrees with request"
        );
        for build in &plan.builds {
            ensure!(
                build.code_commit == c.source_sha
                    && build.source_manifest_sha256 == identity(&plan.source)?
                    && build.builder_image == policy.builder_image,
                "foreign native compiler/source inputs"
            );
            ensure!(
                prefixes.insert(format!("research/builds/{}/", build.id()?)),
                "duplicate native Build"
            );
        }
        ensure!(
            plan.publisher_prefixes.len() == prefixes.len()
                && plan
                    .publisher_prefixes
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    == prefixes,
            "native plan has an expanded scope"
        );
    }
    let prefixes: Vec<_> = prefixes.into_iter().collect();
    ensure!(
        request.publisher_prefixes == prefixes,
        "caller cannot select its own object scope"
    );
    Ok(if request.phase == Phase::Publish {
        Access::Publisher { prefixes }
    } else {
        Access::Reader { prefixes }
    })
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn private_directory(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.canonicalize()? == path
            && path.is_dir()
            && path.metadata()?.permissions().mode() & 0o077 == 0,
        "private canonical broker directory required"
    );
    Ok(())
}
fn read_private<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    private_directory(path.parent().context("private parent absent")?)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    ensure!(
        file.metadata()?.is_file()
            && file.metadata()?.len() <= MAX_JSON as u64
            && file.metadata()?.permissions().mode() & 0o077 == 0,
        "invalid broker state file"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_JSON as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_JSON, "broker state exceeds bound");
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("malformed private broker state"))
}
fn replace(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("state parent absent")?;
    private_directory(parent)?;
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_JSON, "broker state exceeds bound");
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|_| anyhow::anyhow!("broker state replacement failed"))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    replays: BTreeMap<String, u64>,
    issued: BTreeMap<String, u64>,
}
fn projection_lock(config: &Config) -> Result<std::fs::File> {
    private_lock(&config.capabilities_file.with_extension("identity.lock"))
}
fn private_lock(path: &Path) -> Result<std::fs::File> {
    private_directory(path.parent().context("lock parent absent")?)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.permissions().mode() & 0o777 == 0o600,
        "invalid private sidecar lock"
    );
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => anyhow::bail!("projection writer unavailable"),
        }
    }
    Ok(file)
}
fn journal(config: &Config) -> Result<Journal> {
    let journal: Journal = read_private(&config.replay_file)?;
    ensure!(
        journal.schema == 1
            && journal.replays.len() <= 4096
            && journal.issued.len() <= 1024
            && journal
                .replays
                .keys()
                .chain(journal.issued.keys())
                .all(|hash| hft_research_platform::valid_digest(hash)),
        "invalid issuance journal"
    );
    Ok(journal)
}
fn revoke_after_restart(config: &Config) -> Result<()> {
    let _lock = projection_lock(config)?;
    let mut journal = journal(config)?;
    let mut caps: Vec<Capability> = read_private(&config.capabilities_file)?;
    caps.retain(|cap| !journal.issued.contains_key(&cap.token_sha256));
    replace(&config.capabilities_file, &caps)?;
    journal.issued.clear();
    replace(&config.replay_file, &journal)
}
fn refresh(config: &Config, token_hash: &str, active: bool, now: u64) -> Result<bool> {
    let _lock = projection_lock(config)?;
    let mut journal = journal(config)?;
    let mut caps: Vec<Capability> = read_private(&config.capabilities_file)?;
    let deadline = journal.issued.get(token_hash).copied().unwrap_or_default();
    let cap = caps.iter_mut().find(|cap| cap.token_sha256 == token_hash);
    let keep = active && deadline > now && cap.as_ref().is_some_and(|cap| cap.expires_ms > now);
    if keep {
        cap.context("live capability absent")?.expires_ms =
            deadline.min(now.checked_add(LEASE_MS).context("lease overflow")?);
    } else {
        caps.retain(|cap| cap.token_sha256 != token_hash);
        journal.issued.remove(token_hash);
    }
    replace(&config.capabilities_file, &caps)?;
    replace(&config.replay_file, &journal)?;
    Ok(keep)
}
fn install(
    config: &Config,
    request: &Request,
    job_identity: &JobIdentity,
    access: Access,
    now: u64,
) -> Result<Response> {
    ensure!(
        job_identity
            .0
            .exp
            .checked_mul(1000)
            .is_some_and(|expiry| expiry > now)
            && request.expires_ms > now
            && request.expires_ms - now <= HOUR_MS,
        "issuance identity expired"
    );
    let _lock = projection_lock(config)?;
    let mut caps: Vec<Capability> = read_private(&config.capabilities_file)?;
    let mut journal = journal(config)?;
    journal.replays.retain(|_, expiry| *expiry > now);
    let replay = sha256(format!("{}:{}", job_identity.0.iss, job_identity.0.jti).as_bytes());
    ensure!(
        !journal.replays.contains_key(&replay) && journal.replays.len() < 4096,
        "replayed or overloaded issuance"
    );
    caps.retain(|c| c.expires_ms > now);
    journal.issued.retain(|hash, deadline| {
        *deadline > now && caps.iter().any(|cap| &cap.token_sha256 == hash)
    });
    ensure!(
        caps.len() < 1024 && journal.issued.len() < 1024,
        "capability projection full"
    );
    let mut random = [0u8; 32];
    getrandom::getrandom(&mut random)
        .map_err(|_| anyhow::anyhow!("capability entropy unavailable"))?;
    let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let (role, prefixes) = match &access {
        Access::Publisher { prefixes } => ("publisher", prefixes.clone()),
        Access::Reader { prefixes } => ("reader", prefixes.clone()),
        _ => anyhow::bail!("Attempt authority is not available through this broker"),
    };
    let expires_ms = request
        .expires_ms
        .min(now.checked_add(LEASE_MS).context("lease overflow")?);
    let cap = Capability {
        token_sha256: sha256(token.as_bytes()),
        expires_ms,
        access,
    };
    ensure!(
        caps.iter().all(|c| c.token_sha256 != cap.token_sha256),
        "capability collision"
    );
    journal.replays.insert(replay, job_identity.0.exp * 1000);
    journal
        .issued
        .insert(cap.token_sha256.clone(), request.expires_ms);
    // Mark replay before granting access. A crash may deny a retry, never replay it.
    replace(&config.replay_file, &journal)?;
    caps.push(cap);
    replace(&config.capabilities_file, &caps)?;
    let observed: Vec<Capability> = read_private(&config.capabilities_file)?;
    ensure!(observed == caps, "capability projection readback differs");
    Ok(Response {
        schema: 1,
        request_sha256: identity(request)?,
        expires_ms,
        role: role.into(),
        prefixes,
        token,
    })
}

fn github_header(token: &str) -> String {
    use base64::Engine;
    format!(
        "AUTHORIZATION: basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
    )
}
fn job_matches(c: &contract::ContextBinding, check_run_id: u64, job: &Value) -> bool {
    job["id"] == c.publisher_job_id
        && job["run_id"] == c.publisher_run_id
        && job["run_attempt"] == c.publisher_run_attempt
        && job["head_sha"] == c.source_sha
        && job["status"] == "in_progress"
        && c.image_repository
            .rsplit('/')
            .next()
            .is_some_and(|name| job["name"] == format!("Publish {name}"))
        && job["check_run_url"]
            == format!(
                "https://api.github.com/repos/{}/check-runs/{check_run_id}",
                c.repository
            )
}
struct Broker {
    config: Config,
    policy: PublisherPolicy,
    http: reqwest::Client,
    verifiers: tokio::sync::Semaphore,
    keys: tokio::sync::Mutex<Option<(std::time::Instant, KeySet)>>,
    watchers: tokio::sync::Mutex<BTreeMap<String, Watcher>>,
    snapshots: tokio::sync::Mutex<BTreeMap<String, (std::time::Instant, Value)>>,
}
struct Watcher {
    hashes: BTreeSet<String>,
    read_token: String,
}
impl Broker {
    fn new(config: Config, policy: PublisherPolicy) -> Result<Self> {
        Ok(Self {
            config,
            policy,
            http: TlsConfig::default().client(Duration::from_secs(30), true)?,
            verifiers: tokio::sync::Semaphore::new(2),
            keys: tokio::sync::Mutex::new(None),
            watchers: tokio::sync::Mutex::new(BTreeMap::new()),
            snapshots: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }
    async fn renewal_snapshot(&self, path: &str, binding: &str, token: &str) -> Result<Value> {
        let key = format!("{path}:{binding}");
        let mut cache = self.snapshots.lock().await;
        cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(55));
        if let Some((_, value)) = cache.get(&key) {
            return Ok(value.clone());
        }
        ensure!(cache.len() < 128, "renewal snapshot bound exceeded");
        let value = self.api(path, token).await?;
        cache.insert(key, (std::time::Instant::now(), value.clone()));
        Ok(value)
    }
    async fn json(&self, url: &str, token: Option<&str>) -> Result<Value> {
        let request = self
            .http
            .get(url)
            .header("User-Agent", "monday-release-capability-broker")
            .header("Accept", "application/vnd.github+json");
        let request = if let Some(token) = token {
            request.bearer_auth(token)
        } else {
            request
        };
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("trusted authority read unavailable"))?;
        ensure!(
            response.status().is_success(),
            "trusted authority read rejected"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("authority response unavailable"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= MAX_JSON,
                "authority response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("malformed trusted authority response"))
    }
    async fn keys(&self, token: &str) -> Result<KeySet> {
        let header = decode_header(token).map_err(|_| anyhow::anyhow!("invalid OIDC header"))?;
        ensure!(header.alg == Algorithm::RS256, "untrusted OIDC algorithm");
        let mut cache = self.keys.lock().await;
        if cache.as_ref().is_some_and(|(at, keys)| {
            at.elapsed() < Duration::from_secs(300)
                && keys
                    .keys
                    .iter()
                    .any(|k| Some(&k.kid) == header.kid.as_ref())
        }) {
            return Ok(cache.as_ref().unwrap().1.clone());
        }
        ensure!(
            !cache
                .as_ref()
                .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(5)),
            "unknown GitHub key during refresh interval"
        );
        // Neither jku nor any caller-selected URL is used for key discovery.
        let keys: KeySet = serde_json::from_value(self.json(JWKS, None).await?)
            .map_err(|_| anyhow::anyhow!("invalid GitHub key set"))?;
        ensure!(
            !keys.keys.is_empty() && keys.keys.len() <= 32,
            "unbounded GitHub key set"
        );
        *cache = Some((std::time::Instant::now(), keys.clone()));
        Ok(keys)
    }
    async fn api(&self, path: &str, token: &str) -> Result<Value> {
        self.json(
            &format!(
                "https://api.github.com/repos/{}/{path}",
                self.policy.trust.repository
            ),
            Some(token),
        )
        .await
    }
    async fn pages(&self, path: &str, field: &str, token: &str) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        for page in 1..=10 {
            let sep = if path.contains('?') { "&" } else { "?" };
            let data = self
                .api(&format!("{path}{sep}per_page=100&page={page}"), token)
                .await?;
            let batch = data[field]
                .as_array()
                .context("invalid paginated authority response")?;
            values.extend(batch.iter().cloned());
            if batch.len() < 100 {
                return Ok(values);
            }
        }
        anyhow::bail!("authority pagination exceeds bound")
    }
    async fn evidence(&self, r: &Request, token: &str) -> Result<Evidence> {
        let c = &r.context;
        let repository = self
            .json(
                &format!(
                    "https://api.github.com/repos/{}",
                    self.policy.trust.repository
                ),
                Some(token),
            )
            .await?;
        let main = self.api("git/ref/heads/main", token).await?;
        let publisher = self
            .api(&format!("actions/runs/{}", c.publisher_run_id), token)
            .await?;
        let job = self
            .api(&format!("actions/jobs/{}", c.publisher_job_id), token)
            .await?;
        let producer = self
            .api(&format!("actions/runs/{}", c.software_run_id), token)
            .await?;
        let checks = self
            .pages(
                &format!("commits/{}/check-runs?filter=latest", c.source_sha),
                "check_runs",
                token,
            )
            .await?;
        let attempt = producer["run_attempt"]
            .as_u64()
            .context("software attempt absent")?;
        let producer_jobs = self
            .pages(
                &format!("actions/runs/{}/attempts/{attempt}/jobs", c.software_run_id),
                "jobs",
                token,
            )
            .await?;
        Ok(Evidence {
            repository,
            main,
            publisher,
            job,
            producer,
            checks,
            producer_jobs,
        })
    }
    async fn command(
        &self,
        root: &Path,
        program: &Path,
        args: &[&str],
        token: &str,
    ) -> Result<Vec<u8>> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .env_clear()
            .env("PATH", &self.config.tools_path)
            .env("GH_TOKEN", token)
            .env("GITHUB_REPOSITORY", &self.policy.trust.repository)
            .env("GH_CONFIG_DIR", "/work/gh-config")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env("GIT_CONFIG_VALUE_0", github_header(token))
            .env("GIT_CONFIG_KEY_1", "http.followRedirects")
            .env("GIT_CONFIG_VALUE_1", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| anyhow::anyhow!("native verifier unavailable"))?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("verifier stdout absent")?
            .take(MAX_JSON as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= MAX_JSON,
            "native verifier output exceeds bound"
        );
        ensure!(
            child.wait().await?.success(),
            "native verifier rejected source/producer inputs"
        );
        Ok(bytes)
    }
    async fn native_plan(&self, r: &Request, token: &str) -> Result<Plan> {
        let c = &r.context;
        let artifacts = self
            .pages(
                &format!("actions/runs/{}/artifacts", c.software_run_id),
                "artifacts",
                token,
            )
            .await?;
        let prefix = format!("research-image-release-{}-", c.source_sha);
        let candidates: Vec<_> = artifacts
            .iter()
            .filter(|a| {
                a["expired"] == false && a["name"].as_str().is_some_and(|s| s.starts_with(&prefix))
            })
            .collect();
        ensure!(
            candidates.len() == 1,
            "software artifact absent or ambiguous"
        );
        let products = candidates[0]["name"]
            .as_str()
            .unwrap()
            .strip_prefix(&prefix)
            .unwrap();
        ensure!(
            !products.is_empty()
                && products.len() <= 128
                && products
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b"-,".contains(&b)),
            "invalid authenticated product selection"
        );
        private_directory(&self.config.scratch_root)?;
        let scratch = tempfile::tempdir_in(&self.config.scratch_root)?;
        let root = scratch.path();
        std::fs::write(
            root.join("policy.json"),
            serde_json::to_vec(&self.policy_view())?,
        )?;
        std::fs::write(
            root.join("request.json"),
            serde_json::to_vec(
                &json!({"source_sha":c.source_sha,"software_run_id":c.software_run_id,"software_products":products,"product":c.product,"image":r.image,"publisher_run_id":c.publisher_run_id,"publisher_run_attempt":c.publisher_run_attempt,"publisher_job_id":c.publisher_job_id}),
            )?,
        )?;
        // Only tool roots, CA roots and this disposable directory enter the sandbox.
        // Projection, replay journal, broker configuration and TLS keys stay outside.
        let mut common = vec![
            "--die-with-parent",
            "--unshare-all",
            "--share-net",
            "--new-session",
            "--dir",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/bin",
            "/bin",
        ];
        for path in [
            "/lib",
            "/lib64",
            "/etc/ssl/certs",
            "/etc/resolv.conf",
            "/etc/hosts",
        ] {
            if Path::new(path).exists() {
                common.extend(["--ro-bind", path, path]);
            }
        }
        let root_str = root.to_str().context("non-UTF8 verifier directory")?;
        common.extend(["--bind", root_str, "/work", "--chdir", "/work", "--"]);
        let mut bootstrap = common.clone();
        bootstrap.extend(["git", "init", "source"]);
        self.command(root, &self.config.verifier_sandbox, &bootstrap, token)
            .await?;
        let url = format!("https://github.com/{}.git", self.policy.trust.repository);
        let mut fetch = common.clone();
        fetch.extend([
            "git",
            "-C",
            "source",
            "fetch",
            "--depth=1",
            &url,
            &c.source_sha,
        ]);
        self.command(root, &self.config.verifier_sandbox, &fetch, token)
            .await?;
        let mut checkout = common.clone();
        checkout.extend(["git", "-C", "source", "checkout", "--detach", &c.source_sha]);
        self.command(root, &self.config.verifier_sandbox, &checkout, token)
            .await?;
        let binary = self
            .config
            .publisher_binary
            .to_str()
            .context("non-UTF8 verifier path")?;
        let mut plan = common;
        plan.extend([
            binary,
            "plan",
            "/work/source",
            "/work/request.json",
            "/work/policy.json",
        ]);
        let bytes = self
            .command(root, &self.config.verifier_sandbox, &plan, token)
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid native verifier plan"))
    }
    fn policy_view(&self) -> Value {
        json!({"trust":self.policy.trust,"key_id":self.policy.key_id,"builder_image":self.policy.builder_image,"image_repositories":self.policy.image_repositories,"tls":{}})
    }
    async fn issue(self: &Arc<Self>, r: &Request, oidc: &str, token: &str) -> Result<Response> {
        validate_request(r, &self.policy, now_ms()?)?;
        ensure!(oidc.len() <= 16 * 1024, "OIDC identity exceeds bound");
        let verified = verify_identity(
            oidc,
            &self.keys(oidc).await?,
            &self.config,
            &self.policy,
            r,
            now_ms()?,
        )?;
        let evidence = self.evidence(r, token).await?;
        verify_evidence(&evidence, &verified, &self.config, &self.policy, r)?;
        let _permit = if r.phase == Phase::Source {
            None
        } else {
            Some(
                self.verifiers
                    .acquire()
                    .await
                    .context("native verifier unavailable")?,
            )
        };
        let plan = if r.phase == Phase::Source {
            None
        } else {
            Some(self.native_plan(r, token).await?)
        };
        let access = scope(r, &self.policy, plan.as_ref(), now_ms()?)?;
        // Native verification can take time. Repeat mutable authority before minting.
        let evidence = self.evidence(r, token).await?;
        verify_evidence(&evidence, &verified, &self.config, &self.policy, r)?;
        let response = install(&self.config, r, &verified, access, now_ms()?)?;
        let context = r.context.clone();
        let check_run_id = verified.0.check_run_id;
        let token_hash = sha256(response.token.as_bytes());
        self.track(context, check_run_id, token_hash, token).await?;
        Ok(response)
    }
    async fn track(
        self: &Arc<Self>,
        context: contract::ContextBinding,
        check_run_id: u64,
        token_hash: String,
        token: &str,
    ) -> Result<()> {
        let key = identity(&(
            &context.repository,
            &context.source_sha,
            context.publisher_run_id,
            context.publisher_run_attempt,
            context.publisher_job_id,
            check_run_id,
            &context.image_repository,
        ))?;
        let mut watchers = self.watchers.lock().await;
        if let Some(watcher) = watchers.get_mut(&key) {
            watcher.hashes.insert(token_hash);
            watcher.read_token = token.to_owned();
        } else {
            watchers.insert(
                key.clone(),
                Watcher {
                    hashes: BTreeSet::from([token_hash]),
                    read_token: token.to_owned(),
                },
            );
            let broker = self.clone();
            tokio::spawn(async move {
                broker.renew_while_running(context, check_run_id, key).await;
            });
        }
        Ok(())
    }
    async fn renew_while_running(
        &self,
        context: contract::ContextBinding,
        check_run_id: u64,
        key: String,
    ) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let Some((read_token, hashes)) = self
                .watchers
                .lock()
                .await
                .get(&key)
                .map(|watcher| (watcher.read_token.clone(), watcher.hashes.clone()))
            else {
                return;
            };
            let active = tokio::time::timeout(Duration::from_secs(35), async {
                let job = self
                    .api(
                        &format!("actions/jobs/{}", context.publisher_job_id),
                        &read_token,
                    )
                    .await?;
                let main = self
                    .renewal_snapshot("git/ref/heads/main", &context.source_sha, &read_token)
                    .await?;
                let run = self
                    .renewal_snapshot(
                        &format!("actions/runs/{}", context.publisher_run_id),
                        &context.publisher_run_attempt.to_string(),
                        &read_token,
                    )
                    .await?;
                Ok::<bool, anyhow::Error>(
                    job_matches(&context, check_run_id, &job)
                        && main["object"]["sha"] == context.source_sha
                        && run["id"] == context.publisher_run_id
                        && run["head_sha"] == context.source_sha
                        && run["head_repository"]["id"] == self.config.repository_id
                        && run["status"] == "in_progress"
                        && run["run_attempt"] == context.publisher_run_attempt,
                )
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false);
            let Ok(now) = now_ms() else {
                self.watchers.lock().await.remove(&key);
                return;
            };
            if !active {
                let hashes = self
                    .watchers
                    .lock()
                    .await
                    .remove(&key)
                    .map(|watcher| watcher.hashes)
                    .unwrap_or_default();
                for hash in hashes {
                    let _ = refresh(&self.config, &hash, false, now);
                }
                return;
            }
            let mut removed = BTreeSet::new();
            for hash in hashes {
                if !matches!(refresh(&self.config, &hash, true, now), Ok(true)) {
                    removed.insert(hash);
                }
            }
            let mut watchers = self.watchers.lock().await;
            if let Some(watcher) = watchers.get_mut(&key) {
                watcher.hashes.retain(|hash| !removed.contains(hash));
                if watcher.hashes.is_empty() {
                    watchers.remove(&key);
                    return;
                }
            }
        }
    }
}

async fn handle(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Json<Response>, StatusCode> {
    async fn run(broker: &Arc<Broker>, headers: HeaderMap, body: Bytes) -> Result<Response> {
        let oidc = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .context("OIDC identity required")?;
        let token = headers
            .get("x-monday-github-read-token")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty() && s.len() <= 16 * 1024)
            .context("job read token required")?;
        let request: Request = serde_json::from_slice(&body)
            .map_err(|_| anyhow::anyhow!("invalid capability request"))?;
        let mut response = broker.issue(&request, oidc, token).await?;
        response.request_sha256 = sha256(&body);
        Ok(response)
    }
    // Never echo JWTs, job tokens, request JSON, child output or state diagnostics.
    tokio::time::timeout(Duration::from_secs(150), run(&broker, headers, body))
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
        .map(Json)
        .map_err(|_| StatusCode::FORBIDDEN)
}

fn validate_paths(config: &Config, policy: &PublisherPolicy) -> Result<()> {
    private_directory(&config.scratch_root)?;
    ensure!(
        config.publisher_binary.is_absolute()
            && config.publisher_binary.starts_with("/usr/")
            && config.verifier_sandbox.is_absolute()
            && config
                .verifier_sandbox
                .file_name()
                .is_some_and(|name| name == "bwrap")
            && !config.tools_path.is_empty()
            && config
                .tools_path
                .split(':')
                .all(|p| p.starts_with("/usr/") || p == "/bin"),
        "isolated native verifier tools required"
    );
    for tool in [&config.publisher_binary, &config.verifier_sandbox] {
        ensure!(
            tool.canonicalize()? == *tool
                && tool.is_file()
                && tool.metadata()?.permissions().mode() & 0o022 == 0
                && tool.metadata()?.permissions().mode() & 0o111 != 0,
            "trusted canonical verifier executable required"
        );
    }
    let state_paths = BTreeSet::from([
        config.capabilities_file.clone(),
        config.replay_file.clone(),
        config.capabilities_file.with_extension("identity.lock"),
        config.replay_file.with_extension("broker.lock"),
    ]);
    ensure!(
        state_paths.len() == 4,
        "projection, journal and sidecar paths must differ"
    );
    let mut protected = vec![config.capabilities_file.clone(), config.replay_file.clone()];
    if let Some(path) = &policy.tls.identity_file {
        protected.push(path.clone());
    }
    for path in &protected {
        private_directory(path.parent().context("private state parent absent")?)?;
        ensure!(
            path.is_absolute()
                && !path.starts_with(&config.scratch_root)
                && !["/usr", "/bin", "/lib", "/lib64", "/etc/ssl/certs"]
                    .iter()
                    .any(|root| path.starts_with(root)),
            "verifier cannot read broker state or private TLS identity"
        );
    }
    let repo = &policy.trust.repository;
    ensure!(
        repo.split('/').count() == 2
            && repo.split('/').all(|part| !part.is_empty())
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b))
            && policy.trust.schema == 1
            && policy.trust.producer_workflow_path == ".github/workflows/acr-publish.yml",
        "fixed repository/workflow trust required"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2 && args[0] == "--config",
        "usage: research-release-capability-broker --config CONFIG.json"
    );
    let config: Config = read_private(Path::new(&args[1]))?;
    let bind: SocketAddr = config.bind.parse()?;
    ensure!(
        bind.ip().is_loopback() && bind.port() > 0,
        "broker requires loopback TLS ingress"
    );
    let endpoint = reqwest::Url::parse(&config.endpoint)?;
    ensure!(
        endpoint.scheme() == "https"
            && endpoint.host_str().is_some()
            && endpoint.as_str() == config.endpoint
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none()
            && endpoint.path() == "/broker/release-capability",
        "invalid operator broker endpoint"
    );
    ensure!(
        config.repository_id > 0 && config.owner_id > 0,
        "immutable GitHub repository identities required"
    );
    let policy: PublisherPolicy = read_json(&config.policy_file)?;
    validate_paths(&config, &policy)?;
    let _broker_lock = private_lock(&config.replay_file.with_extension("broker.lock"))?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    revoke_after_restart(&config)?;
    let broker = Arc::new(Broker::new(config, policy)?);
    let router = Router::new()
        .route("/broker/release-capability", post(handle))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(broker);
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use hft_research_platform::{build::BuildSpec, orchestrator::Artifact, release::SourceArchive};
    use jsonwebtoken::{encode, EncodingKey, Header};

    fn fixture() -> (
        tempfile::TempDir,
        Config,
        PublisherPolicy,
        Request,
        Claims,
        KeySet,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = Config {
            bind: "127.0.0.1:8092".into(),
            endpoint: "https://gateway.example/broker/release-capability".into(),
            policy_file: root.join("policy.json"),
            repository_id: 123,
            owner_id: 456,
            capabilities_file: root.join("capabilities.json"),
            replay_file: root.join("journal.json"),
            scratch_root: scratch,
            publisher_binary: "/usr/local/bin/research-release-publisher".into(),
            verifier_sandbox: "/usr/bin/bwrap".into(),
            tools_path: "/usr/local/bin:/usr/bin:/bin".into(),
        };
        replace(&config.capabilities_file, &Vec::<Capability>::new()).unwrap();
        replace(
            &config.replay_file,
            &Journal {
                schema: 1,
                ..Journal::default()
            },
        )
        .unwrap();
        let policy: PublisherPolicy = serde_json::from_value(json!({
            "trust":{"schema":1,"repository":"owner/repo","producer_workflow_path":".github/workflows/acr-publish.yml","keys":{"test-only":"a".repeat(64)}},
            "key_id":"test-only","builder_image":format!("registry/builder@sha256:{}","b".repeat(64)),
            "image_repositories":{"controller":"registry/controller"}
        })).unwrap();
        let now = now_ms().unwrap();
        let request = Request {
            schema: 1,
            context: contract::ContextBinding {
                repository: "owner/repo".into(),
                source_sha: "a".repeat(40),
                product: "controller".into(),
                image_repository: "registry/controller".into(),
                software_run_id: 1,
                publisher_run_id: 2,
                publisher_run_attempt: 1,
                publisher_job_id: 3,
            },
            phase: Phase::Source,
            publisher_prefixes: vec![format!("research/sources/{}/", "a".repeat(40))],
            image: None,
            plan_sha256: None,
            expires_ms: now + HOUR_MS,
        };
        let claims = Claims {
            iss: ISSUER.into(),
            aud: config.endpoint.clone(),
            sub: "repo:owner/repo:ref:refs/heads/main".into(),
            jti: "public-fixture-job-identity".into(),
            exp: now / 1000 + 300,
            nbf: now / 1000 - 1,
            iat: now / 1000 - 1,
            repository: "owner/repo".into(),
            repository_id: 123,
            repository_owner_id: 456,
            reference: "refs/heads/main".into(),
            sha: "a".repeat(40),
            workflow_ref: "owner/repo/.github/workflows/acr-publish.yml@refs/heads/main".into(),
            workflow_sha: "a".repeat(40),
            run_id: 2,
            run_attempt: 1,
            check_run_id: 30,
        };
        let keys = KeySet {
            keys: vec![ephemeral_crypto().1.clone()],
        };
        (temp, config, policy, request, claims, keys)
    }
    fn openssl(args: &[&str], input: &[u8]) -> Vec<u8> {
        let mut command = std::process::Command::new("openssl")
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("ephemeral test RSA tool unavailable");
        command.stdin.take().unwrap().write_all(input).unwrap();
        let result = command.wait_with_output().unwrap();
        assert!(result.status.success(), "ephemeral test RSA setup failed");
        assert!(
            result.stdout.len() <= 16 * 1024,
            "ephemeral test RSA output exceeds bound"
        );
        result.stdout
    }
    fn ephemeral_crypto() -> &'static (EncodingKey, Key) {
        static CRYPTO: std::sync::OnceLock<(EncodingKey, Key)> = std::sync::OnceLock::new();
        CRYPTO.get_or_init(|| {
            // Test-only, fresh private bytes remain in process memory and pipes.
            // No private key file or production trust is created.
            let generated = openssl(
                &[
                    "genpkey",
                    "-algorithm",
                    "RSA",
                    "-pkeyopt",
                    "rsa_keygen_bits:2048",
                    "-pkeyopt",
                    "rsa_keygen_pubexp:65537",
                    "-outform",
                    "DER",
                ],
                &[],
            );
            let der = openssl(
                &["rsa", "-inform", "DER", "-traditional", "-outform", "DER"],
                &generated,
            );
            let modulus = String::from_utf8(openssl(
                &["rsa", "-inform", "DER", "-noout", "-modulus"],
                &der,
            ))
            .unwrap();
            let hex = modulus.trim().strip_prefix("Modulus=").unwrap();
            assert_eq!(hex.len(), 512);
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let public = Key {
                kid: "public-test-only".into(),
                kty: "RSA".into(),
                n: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
                e: "AQAB".into(),
                alg: Some("RS256".into()),
                usage: Some("sig".into()),
            };
            (EncodingKey::from_rsa_der(&der), public)
        })
    }
    fn signed(claims: &Claims) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("public-test-only".into());
        encode(&header, claims, &ephemeral_crypto().0).unwrap()
    }
    fn evidence(r: &Request) -> Evidence {
        let c = &r.context;
        Evidence{
            repository:json!({"id":123,"owner":{"id":456},"full_name":c.repository}),
            main:json!({"object":{"sha":c.source_sha}}),
            publisher:json!({"id":c.publisher_run_id,"run_attempt":1,"head_sha":c.source_sha,
                "head_branch":"main","head_repository":{"id":123},"path":".github/workflows/acr-publish.yml",
                "event":"workflow_run","status":"in_progress","conclusion":null}),
            job:json!({"id":3,"run_id":2,"run_attempt":1,"head_sha":c.source_sha,"status":"in_progress",
                "name":"Publish controller","check_run_url":"https://api.github.com/repos/owner/repo/check-runs/30"}),
            producer:json!({"id":1,"run_attempt":1,"head_sha":c.source_sha,"head_branch":"main",
                "head_repository":{"id":123},"path":".github/workflows/ploy-ci.yml","event":"push",
                "status":"completed","conclusion":"success"}),
            producer_jobs:vec![json!({"id":4,"name":"Research image binaries","run_id":1,"run_attempt":1,
                "status":"completed","conclusion":"success"})],
            checks:["Monorepo CI gate","Prediction Markets CI gate","Security Summary Report"].into_iter()
                .enumerate().map(|(i,name)|json!({"id":100+i,"name":name,"head_sha":c.source_sha,
                    "status":"completed","conclusion":"success","app":{"id":15368,"slug":"github-actions"}})).collect(),
        }
    }
    fn native_plan(r: &Request, policy: &PublisherPolicy) -> Plan {
        let source = SourceArchive {
            schema: 1,
            code_commit: r.context.source_sha.clone(),
            archive: Artifact {
                key: format!("research/sources/{}/source.tar", r.context.source_sha),
                sha256: "c".repeat(64),
                bytes: 1,
            },
        };
        let build = BuildSpec {
            schema: 2,
            code_commit: source.code_commit.clone(),
            workspace_manifest: "research-core/platform/Cargo.toml".into(),
            source_manifest_sha256: identity(&source).unwrap(),
            cargo_lock_sha256: "d".repeat(64),
            toolchain_manifest_sha256: "e".repeat(64),
            target: "x86_64-unknown-linux-gnu".into(),
            packages: vec!["hft-research-platform".into()],
            binaries: vec!["researchctl".into()],
            features: vec!["control".into()],
            default_features: false,
            profile: "research".into(),
            profile_manifest_sha256: "f".repeat(64),
            rustflags_sha256: "0".repeat(64),
            native_environment_sha256: "1".repeat(64),
            builder_image: policy.builder_image.clone(),
        };
        Plan {
            schema: 1,
            image: format!("registry/controller@sha256:{}", "2".repeat(64)),
            source,
            publisher_prefixes: vec![
                r.publisher_prefixes[0].clone(),
                format!("research/builds/{}/", build.id().unwrap()),
            ],
            builds: vec![build],
        }
    }
    fn bind_plan(r: &mut Request, plan: &Plan, phase: Phase) {
        r.phase = phase;
        r.image = Some(plan.image.clone());
        r.plan_sha256 = Some(identity(plan).unwrap());
        r.publisher_prefixes = plan
            .publisher_prefixes
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    #[test]
    fn real_rs256_signature_and_exact_claims_are_required() {
        let (_temp, c, p, r, claims, keys) = fixture();
        let now = now_ms().unwrap();
        assert!(verify_identity(&signed(&claims), &keys, &c, &p, &r, now).is_ok());
        let mut tampered = signed(&claims).into_bytes();
        let signature = tampered.iter().rposition(|byte| *byte == b'.').unwrap() + 1;
        tampered[signature] = if tampered[signature] == b'A' {
            b'B'
        } else {
            b'A'
        };
        assert!(verify_identity(
            std::str::from_utf8(&tampered).unwrap(),
            &keys,
            &c,
            &p,
            &r,
            now
        )
        .is_err());
        for (field, value) in [
            ("iss", json!("https://attacker.example")),
            ("aud", json!("https://other.example")),
            ("repository_id", json!("999")),
            ("repository_owner_id", json!(999)),
            ("ref", json!("refs/pull/9/merge")),
            ("sha", json!("b".repeat(40))),
            ("workflow_sha", json!("b".repeat(40))),
            (
                "workflow_ref",
                json!("owner/repo/.github/workflows/evil.yml@refs/heads/main"),
            ),
            ("run_id", json!("9")),
            ("run_attempt", json!(2)),
            ("sub", json!("repo:other/repo:ref:refs/heads/main")),
            ("exp", json!(now / 1000 - 1)),
            ("nbf", json!(now / 1000 + 30)),
            ("iat", json!(now / 1000 + 30)),
        ] {
            let mut changed = serde_json::to_value(&claims).unwrap();
            changed[field] = value;
            let changed: Claims = serde_json::from_value(changed).unwrap();
            assert!(
                verify_identity(&signed(&changed), &keys, &c, &p, &r, now).is_err(),
                "{field}"
            );
        }
        let hs = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"public-test-only"),
        )
        .unwrap();
        assert!(verify_identity(&hs, &keys, &c, &p, &r, now).is_err());
        let mut ambiguous = keys.clone();
        ambiguous.keys.push(ambiguous.keys[0].clone());
        assert!(verify_identity(&signed(&claims), &ambiguous, &c, &p, &r, now).is_err());
    }
    #[test]
    fn github_claim_numbers_accept_strings_and_reject_zero() {
        let (_temp, _, _, _, claims, _) = fixture();
        let mut value = serde_json::to_value(&claims).unwrap();
        for name in [
            "repository_id",
            "repository_owner_id",
            "run_id",
            "run_attempt",
            "check_run_id",
        ] {
            value[name] = json!(value[name].as_u64().unwrap().to_string());
        }
        assert!(serde_json::from_value::<Claims>(value.clone()).is_ok());
        value["check_run_id"] = json!("0");
        assert!(serde_json::from_value::<Claims>(value).is_err());
    }
    #[test]
    fn stale_main_spoofed_checks_and_cross_job_inputs_cannot_authorize() {
        let (_temp, c, p, r, claims, keys) = fixture();
        let verified =
            verify_identity(&signed(&claims), &keys, &c, &p, &r, now_ms().unwrap()).unwrap();
        assert!(verify_evidence(&evidence(&r), &verified, &c, &p, &r).is_ok());
        for field in [
            "main",
            "repository",
            "publisher",
            "job",
            "producer",
            "producer_jobs",
            "checks",
        ] {
            let mut e = evidence(&r);
            match field {
                "main" => e.main["object"]["sha"] = json!("b".repeat(40)),
                "repository" => e.repository["owner"]["id"] = json!(789),
                "publisher" => e.publisher["run_attempt"] = json!(2),
                "job" => {
                    e.job["check_run_url"] =
                        json!("https://api.github.com/repos/owner/repo/check-runs/31")
                }
                "producer" => e.producer["event"] = json!("pull_request"),
                "producer_jobs" => e.producer_jobs[0]["conclusion"] = json!("failure"),
                "checks" => e.checks[0]["app"]["id"] = json!(999),
                _ => unreachable!(),
            }
            assert!(
                verify_evidence(&e, &verified, &c, &p, &r).is_err(),
                "{field}"
            );
        }
        let mut e = evidence(&r);
        let mut newer = e.checks[0].clone();
        newer["id"] = json!(999);
        newer["conclusion"] = json!("failure");
        e.checks.push(newer);
        assert!(verify_evidence(&e, &verified, &c, &p, &r).is_err());
        e = evidence(&r);
        e.producer_jobs.push(e.producer_jobs[0].clone());
        assert!(verify_evidence(&e, &verified, &c, &p, &r).is_err());
    }
    #[test]
    fn independently_derived_builds_define_exact_roles_and_scope() {
        let (_temp, _, p, mut r, _, _) = fixture();
        let now = now_ms().unwrap();
        assert!(matches!(
            scope(&r, &p, None, now).unwrap(),
            Access::Reader { .. }
        ));
        let plan = native_plan(&r, &p);
        bind_plan(&mut r, &plan, Phase::Publish);
        assert!(matches!(
            scope(&r, &p, Some(&plan), now).unwrap(),
            Access::Publisher { .. }
        ));
        assert!(scope(&r, &p, None, now).is_err());
        r.phase = Phase::Read;
        assert!(matches!(
            scope(&r, &p, Some(&plan), now).unwrap(),
            Access::Reader { .. }
        ));
        r.publisher_prefixes.push("research/".into());
        assert!(scope(&r, &p, Some(&plan), now).is_err());
        bind_plan(&mut r, &plan, Phase::Publish);
        let mut foreign = native_plan(&r, &p);
        foreign.builds[0].cargo_lock_sha256 = "9".repeat(64);
        assert!(scope(&r, &p, Some(&foreign), now).is_err());
        r.expires_ms = now + HOUR_MS + 1;
        assert!(scope(&r, &p, Some(&plan), now).is_err());
    }
    #[test]
    fn projection_is_hashed_replay_safe_revocable_and_restart_safe() {
        let (_temp, c, p, r, claims, keys) = fixture();
        let now = now_ms().unwrap();
        let verified = verify_identity(&signed(&claims), &keys, &c, &p, &r, now).unwrap();
        let existing = Capability {
            token_sha256: "8".repeat(64),
            expires_ms: now + HOUR_MS,
            access: Access::AttemptWriter {
                tenant: "unrelated".into(),
                task_id: "existing".into(),
                attempt: 1,
                fence: 1,
            },
        };
        replace(&c.capabilities_file, &vec![existing.clone()]).unwrap();
        let response = install(&c, &r, &verified, scope(&r, &p, None, now).unwrap(), now).unwrap();
        let hash = sha256(response.token.as_bytes());
        assert_eq!(response.role, "reader");
        assert_eq!(response.expires_ms, now + LEASE_MS);
        assert_eq!(response.request_sha256, identity(&r).unwrap());
        for path in [&c.capabilities_file, &c.replay_file] {
            let bytes = std::fs::read(path).unwrap();
            assert!(!String::from_utf8(bytes).unwrap().contains(&response.token));
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(install(&c, &r, &verified, scope(&r, &p, None, now).unwrap(), now).is_err());
        assert!(refresh(&c, &hash, true, now + 30_000).unwrap());
        assert_eq!(
            read_private::<Vec<Capability>>(&c.capabilities_file).unwrap()[1].expires_ms,
            now + 30_000 + LEASE_MS
        );
        revoke_after_restart(&c).unwrap();
        assert!(read_private::<Vec<Capability>>(&c.capabilities_file).unwrap() == vec![existing]);
        assert!(!refresh(&c, &hash, true, now + 60_000).unwrap());
        assert!(install(&c, &r, &verified, scope(&r, &p, None, now).unwrap(), now).is_err());
    }
    #[test]
    fn completion_and_expired_lease_remove_access_without_resurrection() {
        for active in [false, true] {
            let (_temp, c, p, r, claims, keys) = fixture();
            let now = now_ms().unwrap();
            let verified = verify_identity(&signed(&claims), &keys, &c, &p, &r, now).unwrap();
            let response =
                install(&c, &r, &verified, scope(&r, &p, None, now).unwrap(), now).unwrap();
            let stopped = now + if active { LEASE_MS + 1 } else { 30_000 };
            assert!(!refresh(&c, &sha256(response.token.as_bytes()), active, stopped).unwrap());
            assert!(read_private::<Vec<Capability>>(&c.capabilities_file)
                .unwrap()
                .is_empty());
            assert!(!refresh(
                &c,
                &sha256(response.token.as_bytes()),
                true,
                now + LEASE_MS + 2
            )
            .unwrap());
        }
    }
    #[test]
    fn simultaneous_exchanges_preserve_both_gateway_capabilities() {
        let (_temp, c, p, r, claims, keys) = fixture();
        let now = now_ms().unwrap();
        let mut identities = Vec::new();
        for i in 0..2 {
            let mut claims = claims.clone();
            claims.jti = format!("public-fixture-concurrent-{i}");
            identities.push(verify_identity(&signed(&claims), &keys, &c, &p, &r, now).unwrap());
        }
        std::thread::scope(|threads| {
            let mut handles = Vec::new();
            for verified in identities {
                let (c, p, r) = (&c, &p, &r);
                handles.push(threads.spawn(move || {
                    install(c, r, &verified, scope(r, p, None, now).unwrap(), now).unwrap()
                }));
            }
            for handle in handles {
                handle.join().unwrap();
            }
        });
        let caps: Vec<Capability> = read_private(&c.capabilities_file).unwrap();
        assert_eq!(caps.len(), 2);
        assert_ne!(caps[0].token_sha256, caps[1].token_sha256);
        assert_eq!(journal(&c).unwrap().issued.len(), 2);
    }
    #[test]
    fn shared_projection_lock_and_private_files_fail_closed() {
        let (_temp, c, p, r, claims, keys) = fixture();
        let now = now_ms().unwrap();
        let verified = verify_identity(&signed(&claims), &keys, &c, &p, &r, now).unwrap();
        let lock = projection_lock(&c).unwrap();
        assert!(install(&c, &r, &verified, scope(&r, &p, None, now).unwrap(), now).is_err());
        drop(lock);
        let broker_lock = private_lock(&c.replay_file.with_extension("broker.lock")).unwrap();
        assert!(private_lock(&c.replay_file.with_extension("broker.lock")).is_err());
        drop(broker_lock);
        assert!(install(
            &c,
            &r,
            &verified,
            Access::AttemptWriter {
                tenant: "x".into(),
                task_id: "x".into(),
                attempt: 1,
                fence: 1
            },
            now
        )
        .is_err());
        assert!(journal(&c).unwrap().issued.is_empty());
        let link = c.capabilities_file.with_file_name("symlink.json");
        std::os::unix::fs::symlink(&c.capabilities_file, &link).unwrap();
        assert!(read_private::<Vec<Capability>>(&link).is_err());
        std::fs::set_permissions(&c.capabilities_file, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(read_private::<Vec<Capability>>(&c.capabilities_file).is_err());
    }
    #[tokio::test]
    async fn source_publish_and_read_share_one_job_monitor() {
        let (_temp, config, policy, request, _, _) = fixture();
        let broker = Arc::new(Broker::new(config, policy).unwrap());
        for i in 0..3 {
            broker
                .track(
                    request.context.clone(),
                    30,
                    format!("{i:064x}"),
                    "ephemeral-test-read-token",
                )
                .await
                .unwrap();
        }
        let watchers = broker.watchers.lock().await;
        assert_eq!(watchers.len(), 1);
        assert_eq!(watchers.values().next().unwrap().hashes.len(), 3);
    }
    #[tokio::test]
    async fn http_rejection_never_echoes_forged_credentials() {
        let (_temp, config, policy, request, claims, keys) = fixture();
        let broker = Arc::new(Broker::new(config, policy).unwrap());
        *broker.keys.lock().await = Some((std::time::Instant::now(), keys));
        let mut forged = signed(&claims).into_bytes();
        let signature = forged.iter().rposition(|byte| *byte == b'.').unwrap() + 1;
        forged[signature] = if forged[signature] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", String::from_utf8(forged).unwrap())
                .parse()
                .unwrap(),
        );
        headers.insert(
            "x-monday-github-read-token",
            "public-test-only-not-a-github-token".parse().unwrap(),
        );
        let result = handle(
            State(broker.clone()),
            headers,
            Bytes::from(serde_json::to_vec(&request).unwrap()),
        )
        .await;
        assert!(matches!(result, Err(StatusCode::FORBIDDEN)));
        assert!(journal(&broker.config).unwrap().issued.is_empty());
    }
}
