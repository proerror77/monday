//! Exchange job identity for a bounded source/Build capability. Never mint one.
use anyhow::{bail, ensure, Context, Result};
use hft_research_platform::{
    build::pinned_image,
    identity,
    release_publisher::{read_json, PublisherPolicy},
    transport::TlsConfig,
};
use std::{
    collections::BTreeSet,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LIFETIME_MS: u64 = 60 * 60 * 1000;
const MAX_RESPONSE: usize = 64 * 1024;

#[path = "../release_capability_contract.rs"]
mod contract;
use contract::{ContextBinding, Phase, Plan, Request, Response};

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

async fn github_oidc(audience: &str) -> Result<String> {
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
        .append_pair("audience", audience);
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
    Ok(oidc_token.to_owned())
}
async fn exchange(request: &Request, broker: &reqwest::Url, tls: &TlsConfig) -> Result<String> {
    let oidc_token = github_oidc(broker.as_str()).await?;
    let client = tls.client(Duration::from_secs(180), true)?;
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

fn oss_identity(
    token: &str,
    request: &Request,
    config: &hft_research_platform::release_oss::OssConfig,
    job: &serde_json::Value,
) -> Result<()> {
    use base64::Engine;
    // RAM verifies the signature. This check narrows the exact job before exchange.
    let payload = token.split('.').nth(1).context("OIDC payload missing")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| anyhow::anyhow!("invalid OIDC payload"))?;
    let c: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid OIDC claims"))?;
    let r = &request.context;
    ensure!(
        c["iss"] == "https://token.actions.githubusercontent.com"
            && c["aud"] == config.audience
            && c["sub"] == config.subject
            && c["repository"] == r.repository
            && c["repository_id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                == Some(config.repository_id)
            && c["repository_owner_id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                == Some(config.owner_id)
            && c["sha"] == r.source_sha
            && c["ref"] == "refs/heads/main"
            && c["workflow_ref"]
                == format!(
                    "{}/.github/workflows/acr-publish.yml@refs/heads/main",
                    r.repository
                )
            && c["run_id"].as_str().and_then(|s| s.parse::<u64>().ok()) == Some(r.publisher_run_id)
            && c["run_attempt"]
                .as_str()
                .and_then(|s| s.parse::<u32>().ok())
                == Some(r.publisher_run_attempt)
            && job["id"] == r.publisher_job_id
            && job["run_id"] == r.publisher_run_id
            && job["run_attempt"] == r.publisher_run_attempt
            && job["head_sha"] == r.source_sha
            && job["status"] == "in_progress"
            && job["conclusion"].is_null()
            && job["name"]
                == format!(
                    "Publish {}",
                    r.image_repository.rsplit('/').next().unwrap_or_default()
                )
            && c["check_run_id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|id| id > 0
                    && job["check_run_url"]
                        == format!(
                            "https://api.github.com/repos/{}/check-runs/{id}",
                            r.repository
                        ))
            && c["exp"]
                .as_u64()
                .is_some_and(|exp| exp > now_ms().unwrap_or(u64::MAX) / 1000),
        "OIDC does not bind the exact approved repository/workflow/job"
    );
    Ok(())
}

async fn oss_exchange(
    request: &Request,
    config: &hft_research_platform::release_oss::OssConfig,
) -> Result<String> {
    use hft_research_platform::release_oss::{session_policy, Session};
    let publisher = request.phase == Phase::Publish;
    let policy = session_policy(config, &request.publisher_prefixes, publisher)?;
    ensure!(policy.len() <= 2048, "STS session policy exceeds RAM bound");
    let oidc = github_oidc(&config.audience).await?;
    let output = std::process::Command::new("gh")
        .args([
            "api",
            &format!(
                "repos/{}/actions/jobs/{}",
                request.context.repository, request.context.publisher_job_id
            ),
        ])
        .stderr(std::process::Stdio::null())
        .output()
        .context("GitHub job reader unavailable")?;
    ensure!(
        output.status.success() && output.stdout.len() <= 65536,
        "GitHub job read rejected"
    );
    let job: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("invalid GitHub job response")?;
    oss_identity(&oidc, request, config, &job)?;
    let client = TlsConfig::default().client(Duration::from_secs(30), true)?;
    let fields = [
        ("Action", "AssumeRoleWithOIDC".to_owned()),
        ("Version", "2015-04-01".to_owned()),
        ("Format", "JSON".to_owned()),
        ("RoleArn", config.role_arn.clone()),
        ("OIDCProviderArn", config.oidc_provider_arn.clone()),
        ("OIDCToken", oidc),
        (
            "RoleSessionName",
            format!("monday-{}", &identity(request)?[..40]),
        ),
        ("DurationSeconds", "900".to_owned()),
        ("Policy", policy),
    ];
    let form = reqwest::Url::parse_with_params("https://sts.aliyuncs.com/", &fields)?
        .query()
        .context("STS request missing")?
        .to_owned();
    let response = client
        .post("https://sts.aliyuncs.com/")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("RAM OIDC exchange unavailable"))?;
    let body: serde_json::Value = serde_json::from_slice(&bounded_body(response).await?)
        .map_err(|_| anyhow::anyhow!("invalid STS response"))?;
    ensure!(
        body["OIDCTokenInfo"]["Subject"] == config.subject
            && body["OIDCTokenInfo"]["Issuer"] == "https://token.actions.githubusercontent.com"
            && body["OIDCTokenInfo"]["ClientIds"] == config.audience
            && body["OIDCTokenInfo"]["VerificationInfo"] == "Success",
        "RAM did not confirm the approved OIDC identity"
    );
    let c = &body["Credentials"];
    let field = |name: &str| {
        c[name]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .context("STS credential missing")
    };
    let expiration = chrono::DateTime::parse_from_rfc3339(&field("Expiration")?)
        .map_err(|_| anyhow::anyhow!("invalid STS expiration"))?
        .timestamp_millis();
    let session = Session {
        access_key_id: field("AccessKeyId")?,
        access_key_secret: field("AccessKeySecret")?,
        security_token: field("SecurityToken")?,
        expires_ms: expiration,
        publisher,
        prefixes: request.publisher_prefixes.clone(),
        versions: Default::default(),
    };
    session.validate(i64::try_from(now_ms()?)?)?;
    Ok(serde_json::to_string(&session)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("arguments require UTF-8"))
        })
        .collect::<Result<_>>()?;
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    if let [mode @ ("oss-source" | "oss-publish"), policy, context, plan, output] = words.as_slice()
    {
        let policy: PublisherPolicy = read_json(Path::new(policy))?;
        let phase = if *mode == "oss-source" {
            Phase::Source
        } else {
            Phase::Publish
        };
        let plan = if phase == Phase::Source {
            ensure!(*plan == "-", "source cannot use Build plan");
            None
        } else {
            Some(read_json(Path::new(plan))?)
        };
        let request = request(
            &policy,
            read_json(Path::new(context))?,
            phase,
            plan,
            now_ms()?,
        )?;
        return write_token(
            Path::new(output),
            &oss_exchange(
                &request,
                policy.oss.as_ref().context("OSS policy required")?,
            )
            .await?,
        );
    }
    let (phase, policy, context, plan, broker, gateway, output) = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["source", policy, context, broker, gateway, output] => (Phase::Source, *policy, *context, None, *broker, *gateway, *output),
        ["publish", policy, context, plan, broker, gateway, output] => (Phase::Publish, *policy, *context, Some(*plan), *broker, *gateway, *output),
        ["read", policy, context, plan, broker, gateway, output] => (Phase::Read, *policy, *context, Some(*plan), *broker, *gateway, *output),
        _ => bail!("usage: research-release-capability oss-source POLICY CONTEXT - SESSION_FILE | oss-publish POLICY CONTEXT PLAN SESSION_FILE | source POLICY CONTEXT HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE | publish|read POLICY CONTEXT PLAN HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE"),
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
    use hft_research_platform::{build::BuildSpec, release::SourceArchive};
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
    #[test]
    fn oss_oidc_rejects_wrong_workflow_source_attempt_owner_expiry_and_job() {
        use base64::Engine;
        let config = hft_research_platform::release_oss::OssConfig {
            bucket: "fixture".into(),
            region: "cn-hangzhou".into(),
            endpoint: "https://fixture.oss-cn-hangzhou.aliyuncs.com/".into(),
            role_arn: "acs:ram::1:role/test".into(),
            oidc_provider_arn: "acs:ram::1:oidc-provider/test".into(),
            audience: "test".into(),
            subject: "repo:owner/repo:environment:research-controller".into(),
            publication_namespaces: vec!["research/builds/".into(), "research/sources/".into()],
            repository_id: 1,
            owner_id: 2,
        };
        let request = request(
            &policy(),
            source().context,
            Phase::Source,
            None,
            now_ms().unwrap(),
        )
        .unwrap();
        let c = &request.context;
        let claims = json!({"iss":"https://token.actions.githubusercontent.com","aud":"test","sub":config.subject,"repository":c.repository,"repository_id":"1","repository_owner_id":"2","sha":c.source_sha,"ref":"refs/heads/main","workflow_ref":format!("{}/.github/workflows/acr-publish.yml@refs/heads/main",c.repository),"run_id":c.publisher_run_id.to_string(),"run_attempt":c.publisher_run_attempt.to_string(),"check_run_id":"99","exp":now_ms().unwrap()/1000+60});
        let job = json!({"id":c.publisher_job_id,"run_id":c.publisher_run_id,"run_attempt":c.publisher_run_attempt,"head_sha":c.source_sha,"status":"in_progress","conclusion":null,"name":"Publish controller","check_run_url":format!("https://api.github.com/repos/{}/check-runs/99",c.repository)});
        let token = |v: &serde_json::Value| {
            format!(
                "test.{}.test",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(v).unwrap())
            )
        };
        assert!(oss_identity(&token(&claims), &request, &config, &job).is_ok());
        let mut immutable = config.clone();
        immutable.subject = "repo:owner@2/repo@1:environment:research-controller".into();
        let mut immutable_claims = claims.clone();
        immutable_claims["sub"] = json!(immutable.subject);
        assert!(oss_identity(&token(&immutable_claims), &request, &immutable, &job).is_ok());
        assert!(oss_identity(&token(&immutable_claims), &request, &config, &job).is_err());
        for (key, value) in [
            ("iss", json!("https://foreign")),
            ("aud", json!("foreign")),
            ("repository", json!("foreign/repo")),
            ("repository_id", json!("99")),
            ("run_id", json!("99")),
            ("ref", json!("refs/heads/foreign")),
            ("workflow_ref", json!("foreign")),
            ("sub", json!("foreign")),
            ("sub", json!(null)),
            ("sha", json!("b".repeat(40))),
            ("run_attempt", json!("999")),
            ("repository_owner_id", json!("3")),
            ("exp", json!(0)),
            ("check_run_id", json!("98")),
            ("check_run_id", json!(99)),
            ("check_run_id", json!(null)),
            ("check_run_id", json!("0")),
            ("check_run_id", json!("malformed")),
        ] {
            let mut bad = claims.clone();
            bad[key] = value;
            assert!(oss_identity(&token(&bad), &request, &config, &job).is_err());
        }
        let mut cancelled = job.clone();
        cancelled["status"] = json!("completed");
        cancelled["conclusion"] = json!("cancelled");
        assert!(oss_identity(&token(&claims), &request, &config, &cancelled).is_err());
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
    fn consecutive_native_releases_do_not_reuse_previous_session_prefixes() {
        let first = request(
            &policy(),
            source().context,
            Phase::Publish,
            Some(plan()),
            10_000,
        )
        .unwrap();
        let mut next_plan = plan();
        next_plan.source.code_commit = "b".repeat(40);
        next_plan.source.archive.key = format!("research/sources/{}/source.tar", "b".repeat(40));
        next_plan.builds[0].code_commit = next_plan.source.code_commit.clone();
        next_plan.builds[0].source_manifest_sha256 = identity(&next_plan.source).unwrap();
        next_plan.publisher_prefixes = vec![
            format!("research/builds/{}/", next_plan.builds[0].id().unwrap()),
            format!("research/sources/{}/", next_plan.source.code_commit),
        ];
        let mut context = source().context;
        context.source_sha = next_plan.source.code_commit.clone();
        let mut stale_plan: Plan =
            serde_json::from_value(serde_json::to_value(&next_plan).unwrap()).unwrap();
        let next = request(
            &policy(),
            context.clone(),
            Phase::Publish,
            Some(next_plan),
            10_000,
        )
        .unwrap();
        assert!(next
            .publisher_prefixes
            .iter()
            .all(|p| !first.publisher_prefixes.contains(p)));
        stale_plan.publisher_prefixes = first.publisher_prefixes;
        assert!(request(&policy(), context, Phase::Publish, Some(stale_plan), 10_000).is_err());
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
