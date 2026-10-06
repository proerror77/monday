//! Exchange job identity for a bounded source/Build capability. Never mint one.
use anyhow::{bail, ensure, Context, Result};
use hft_research_platform::{
    build::{pinned_image, BuildSpec},
    identity,
    release::SourceArchive,
    release_publisher::{read_json, PublisherPolicy},
    transport::TlsConfig,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LIFETIME_MS: u64 = 60 * 60 * 1000;
const MAX_RESPONSE: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextBinding {
    repository: String,
    source_sha: String,
    product: String,
    image_repository: String,
    software_run_id: u64,
    publisher_run_id: u64,
    publisher_run_attempt: u32,
    publisher_job_id: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Source,
    Publish,
    Read,
}

#[derive(Debug, Serialize)]
struct Request {
    schema: u32,
    context: ContextBinding,
    phase: Phase,
    publisher_prefixes: Vec<String>,
    image: Option<String>,
    plan_sha256: Option<String>,
    expires_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: u32,
    image: String,
    source: SourceArchive,
    builds: Vec<BuildSpec>,
    publisher_prefixes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    schema: u32,
    request_sha256: String,
    expires_ms: u64,
    role: String,
    prefixes: Vec<String>,
    token: String,
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn endpoint(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("invalid capability endpoint")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "capability endpoint requires HTTPS without credentials/query/fragment"
    );
    Ok(url)
}

fn request(
    policy: &PublisherPolicy,
    context: ContextBinding,
    phase: Phase,
    plan: Option<Plan>,
    now: u64,
) -> Result<Request> {
    ensure!(
        context.repository == policy.trust.repository
            && policy.trust.schema == 1
            && policy.trust.producer_workflow_path == ".github/workflows/acr-publish.yml"
            && policy.image_repositories.get(&context.product) == Some(&context.image_repository)
            && context.source_sha.len() == 40
            && context
                .source_sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && context.software_run_id > 0
            && context.publisher_run_id > 0
            && context.publisher_run_attempt > 0
            && context.publisher_job_id > 0,
        "capability context is outside the publication policy"
    );
    let source = format!("research/sources/{}/", context.source_sha);
    let mut prefixes = BTreeSet::from([source]);
    let mut image = None;
    let mut plan_sha = None;
    if phase == Phase::Source {
        ensure!(plan.is_none(), "source preflight cannot claim a Build plan");
    } else {
        let plan = plan.context("actual native Build plan required")?;
        ensure!(
            plan.schema == 1
                && pinned_image(&plan.image)
                && plan.image.rsplit_once("@sha256:").map(|(repo, _)| repo)
                    == Some(context.image_repository.as_str())
                && plan.source.schema == 1
                && plan.source.code_commit == context.source_sha
                && !plan.builds.is_empty()
                && plan.builds.len() <= 255,
            "capability plan has a foreign source/image"
        );
        for build in &plan.builds {
            ensure!(
                build.code_commit == context.source_sha
                    && build.source_manifest_sha256 == identity(&plan.source)?
                    && build.builder_image == policy.builder_image,
                "capability plan has foreign compiler/source inputs"
            );
            ensure!(
                prefixes.insert(format!("research/builds/{}/", build.id()?)),
                "duplicate Build in capability plan"
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
            "capability plan expands its source/Build scope"
        );
        plan_sha = Some(identity(&plan)?);
        image = Some(plan.image);
    }
    Ok(Request {
        schema: 1,
        context,
        phase,
        publisher_prefixes: prefixes.into_iter().collect(),
        image,
        plan_sha256: plan_sha,
        expires_ms: now
            .checked_add(LIFETIME_MS)
            .context("capability deadline overflow")?,
    })
}

fn validate_response(bytes: &[u8], request: &Request, now: u64) -> Result<String> {
    ensure!(
        bytes.len() <= MAX_RESPONSE,
        "capability response exceeds bound"
    );
    // Never include an untrusted response or its token in diagnostic output.
    let response: Response = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("malformed capability response"))?;
    let role = if request.phase == Phase::Publish {
        "publisher"
    } else {
        "reader"
    };
    ensure!(
        response.schema == 1
            && response.request_sha256 == identity(request)?
            && response.role == role
            && response.prefixes == request.publisher_prefixes
            && response.expires_ms > now
            && response.expires_ms <= request.expires_ms
            && response.expires_ms - now <= LIFETIME_MS,
        "capability response changed request binding, scope, role or lifetime"
    );
    ensure!(
        (32..=4096).contains(&response.token.len())
            && response.token.bytes().all(|b| b.is_ascii_graphic()),
        "invalid capability bearer token"
    );
    Ok(response.token)
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    ensure!(
        response.status().is_success(),
        "capability exchange rejected"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("capability response unavailable"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE,
            "capability response exceeds bound"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn exchange(request: &Request, broker: &reqwest::Url, tls: &TlsConfig) -> Result<String> {
    let mut oidc = reqwest::Url::parse(
        &std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL")
            .context("GitHub job OIDC request URL required")?,
    )
    .map_err(|_| anyhow::anyhow!("invalid GitHub OIDC request URL"))?;
    ensure!(
        oidc.scheme() == "https"
            && oidc
                .host_str()
                .is_some_and(|host| host.ends_with(".actions.githubusercontent.com"))
            && oidc.username().is_empty()
            && oidc.password().is_none()
            && oidc.fragment().is_none(),
        "foreign GitHub OIDC request endpoint"
    );
    let query: Vec<_> = oidc
        .query_pairs()
        .filter(|(key, _)| key != "audience")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    oidc.set_query(None);
    oidc.query_pairs_mut()
        .extend_pairs(query)
        .append_pair("audience", broker.as_str());
    let job_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .context("GitHub job id-token permission required")?;
    let client = TlsConfig::default().client(Duration::from_secs(30), true)?;
    let response = client
        .get(oidc)
        .bearer_auth(job_token)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("GitHub OIDC exchange unavailable"))?;
    let bytes = bounded_body(response).await?;
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("malformed GitHub OIDC response"))?;
    let oidc_token = body["value"]
        .as_str()
        .filter(|v| !v.is_empty())
        .context("GitHub OIDC identity missing")?;
    let client = tls.client(Duration::from_secs(30), true)?;
    let github_read_token =
        std::env::var("GH_TOKEN").context("job read-only GitHub token required")?;
    let response = client
        .post(broker.clone())
        .bearer_auth(oidc_token)
        .header("X-Monday-GitHub-Read-Token", github_read_token)
        .json(request)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("release capability broker unavailable"))?;
    validate_response(&bounded_body(response).await?, request, now_ms()?)
}

fn write_token(path: &Path, token: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().context("private token directory required")?;
    ensure!(
        path.is_absolute()
            && parent.canonicalize()? == parent
            && parent.metadata()?.permissions().mode() & 0o077 == 0,
        "capability output requires a private canonical directory"
    );
    // create_new rejects existing files and symlinks, including concurrent swaps.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (phase, policy, context, plan, broker, gateway, output) = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["source", policy, context, broker, gateway, output] => (Phase::Source, *policy, *context, None, *broker, *gateway, *output),
        ["publish", policy, context, plan, broker, gateway, output] => (Phase::Publish, *policy, *context, Some(*plan), *broker, *gateway, *output),
        ["read", policy, context, plan, broker, gateway, output] => (Phase::Read, *policy, *context, Some(*plan), *broker, *gateway, *output),
        _ => bail!("usage: research-release-capability source POLICY CONTEXT HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE | publish|read POLICY CONTEXT PLAN HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE"),
    };
    let policy: PublisherPolicy = read_json(Path::new(policy))?;
    let broker = endpoint(broker)?;
    ensure!(
        broker.origin() == endpoint(gateway)?.origin(),
        "broker and gateway must share the operator TLS origin"
    );
    let request = request(
        &policy,
        read_json(Path::new(context))?,
        phase,
        plan.map(|p| read_json(Path::new(p))).transpose()?,
        now_ms()?,
    )?;
    write_token(
        Path::new(output),
        &exchange(&request, &broker, &policy.tls).await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> Request {
        Request {
            schema: 1,
            context: ContextBinding {
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
            expires_ms: 10_000 + LIFETIME_MS,
        }
    }
    fn policy() -> PublisherPolicy {
        serde_json::from_value(json!({
            "trust":{"schema":1,"repository":"owner/repo","producer_workflow_path":".github/workflows/acr-publish.yml","keys":{"test-only":"a".repeat(64)}},
            "key_id":"test-only","builder_image":format!("registry/builder@sha256:{}","b".repeat(64)),
            "image_repositories":{"controller":"registry/controller"}
        })).unwrap()
    }
    fn plan() -> Plan {
        let source = SourceArchive {
            schema: 1,
            code_commit: "a".repeat(40),
            archive: hft_research_platform::orchestrator::Artifact {
                key: format!("research/sources/{}/source.tar", "a".repeat(40)),
                sha256: "c".repeat(64),
                bytes: 1,
            },
        };
        let build = BuildSpec {
            schema: 2,
            code_commit: source.code_commit.clone(),
            workspace_manifest: "research-core/Cargo.toml".into(),
            source_manifest_sha256: identity(&source).unwrap(),
            cargo_lock_sha256: "d".repeat(64),
            toolchain_manifest_sha256: "e".repeat(64),
            target: "x86_64-unknown-linux-gnu".into(),
            packages: vec!["test-controller".into()],
            binaries: vec!["test-controller".into()],
            features: vec![],
            default_features: false,
            profile: "research".into(),
            profile_manifest_sha256: "f".repeat(64),
            rustflags_sha256: "0".repeat(64),
            native_environment_sha256: "1".repeat(64),
            builder_image: policy().builder_image,
        };
        let prefixes = vec![
            format!("research/sources/{}/", source.code_commit),
            format!("research/builds/{}/", build.id().unwrap()),
        ];
        Plan {
            schema: 1,
            image: format!("registry/controller@sha256:{}", "2".repeat(64)),
            source,
            builds: vec![build],
            publisher_prefixes: prefixes,
        }
    }
    fn response(request: &Request) -> serde_json::Value {
        json!({"schema":1,"request_sha256":identity(request).unwrap(),"expires_ms":request.expires_ms,"role":if request.phase==Phase::Publish {"publisher"} else {"reader"},"prefixes":request.publisher_prefixes,"token":"ephemeral-test-only-token-0123456789"})
    }
    #[test]
    fn actual_build_plan_derives_scope_and_import_gets_only_reader() {
        let policy = policy();
        let publish = request(
            &policy,
            source().context,
            Phase::Publish,
            Some(plan()),
            10_000,
        )
        .unwrap();
        assert_eq!(publish.publisher_prefixes.len(), 2);
        assert!(validate_response(
            &serde_json::to_vec(&response(&publish)).unwrap(),
            &publish,
            10_000
        )
        .is_ok());
        let read = request(&policy, source().context, Phase::Read, Some(plan()), 10_000).unwrap();
        assert_eq!(read.publisher_prefixes, publish.publisher_prefixes);
        let mut wrong = response(&read);
        wrong["role"] = json!("publisher");
        assert!(validate_response(&serde_json::to_vec(&wrong).unwrap(), &read, 10_000).is_err());
        let before = request(&policy, source().context, Phase::Source, None, 10_000).unwrap();
        assert_eq!(before.publisher_prefixes, source().publisher_prefixes);
        assert!(before.image.is_none() && before.plan_sha256.is_none());
    }
    #[test]
    fn untrusted_plan_cannot_expand_scope_or_substitute_source_and_builder() {
        let policy = policy();
        let mut wide = plan();
        wide.publisher_prefixes.push("research/".into());
        let mut foreign_source = plan();
        foreign_source.builds[0].code_commit = "b".repeat(40);
        let mut foreign_builder = plan();
        foreign_builder.builds[0].builder_image = format!("other@sha256:{}", "b".repeat(64));
        let mut foreign_image = plan();
        foreign_image.image = format!("other/controller@sha256:{}", "2".repeat(64));
        let mut duplicate = plan();
        duplicate.builds.push(duplicate.builds[0].clone());
        for bad in [
            wide,
            foreign_source,
            foreign_builder,
            foreign_image,
            duplicate,
        ] {
            assert!(request(&policy, source().context, Phase::Publish, Some(bad), 10_000).is_err());
        }
        assert!(request(&policy, source().context, Phase::Publish, None, 10_000).is_err());
        assert!(request(
            &policy,
            source().context,
            Phase::Source,
            Some(plan()),
            10_000
        )
        .is_err());
    }
    #[test]
    fn foreign_repository_product_or_publisher_context_cannot_request_access() {
        for field in ["repository", "product", "source_sha", "publisher_job_id"] {
            let mut bad = serde_json::to_value(source().context).unwrap();
            bad[field] = if field == "publisher_job_id" {
                json!(0)
            } else {
                json!("foreign")
            };
            assert!(request(
                &policy(),
                serde_json::from_value(bad).unwrap(),
                Phase::Source,
                None,
                10_000
            )
            .is_err());
        }
    }
    #[test]
    fn exchange_rejects_foreign_binding_expiry_scope_and_role() {
        let request = source();
        let correct = response(&request);
        assert!(
            validate_response(&serde_json::to_vec(&correct).unwrap(), &request, 10_000).is_ok()
        );
        for (field, value) in [
            ("request_sha256", json!("b".repeat(64))),
            ("expires_ms", json!(10_000)),
            ("expires_ms", json!(request.expires_ms + 1)),
            ("role", json!("publisher")),
            ("prefixes", json!(["research/"])),
            (
                "prefixes",
                json!([format!("research/sources/{}/", "b".repeat(40))]),
            ),
        ] {
            let mut bad = correct.clone();
            bad[field] = value;
            assert!(
                validate_response(&serde_json::to_vec(&bad).unwrap(), &request, 10_000).is_err(),
                "{field}"
            );
        }
        let mut other_run = source();
        other_run.context.publisher_run_attempt += 1;
        assert!(
            validate_response(&serde_json::to_vec(&correct).unwrap(), &other_run, 10_000).is_err()
        );
    }
    #[test]
    fn renewal_is_per_request_and_never_grants_a_future_lifetime() {
        let mut renewed = source();
        renewed.expires_ms += 1000;
        let old = serde_json::to_vec(&response(&source())).unwrap();
        assert!(validate_response(&old, &renewed, 11_000).is_err());
        assert!(validate_response(
            &serde_json::to_vec(&response(&renewed)).unwrap(),
            &renewed,
            11_000
        )
        .is_ok());
    }
    #[test]
    fn invalid_response_never_discloses_the_token() {
        let mut bad = response(&source());
        bad["unexpected"] = json!("secret-must-not-appear");
        let error = validate_response(&serde_json::to_vec(&bad).unwrap(), &source(), 10_000)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret-must-not-appear"));
        assert!(!error.contains("ephemeral-test-only-token"));
        assert!(validate_response(&vec![b'x'; MAX_RESPONSE + 1], &source(), 10_000).is_err());
    }
    #[test]
    fn endpoints_reject_plaintext_redirect_targets_and_embedded_secrets() {
        for url in [
            "http://example.com/",
            "https://user:password@example.com/",
            "https://example.com/?token=x",
            "https://example.com/#token",
        ] {
            assert!(endpoint(url).is_err());
        }
        assert!(endpoint("https://example.com/broker").is_ok());
    }
    #[test]
    fn token_file_is_private_and_cannot_replace_a_symlink_or_existing_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = dir.path().canonicalize().unwrap();
        let target = root.join("token");
        write_token(&target, "test-only").unwrap();
        assert_eq!(
            target.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_token(&target, "replacement").is_err());
        symlink(&target, root.join("link")).unwrap();
        assert!(write_token(&root.join("link"), "replacement").is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "test-only");
    }
}
