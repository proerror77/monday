use anyhow::{bail, Context, Result};
use hft_research_platform::release_publisher::{
    self, PublicationRequest, PublisherPolicy, ReleaseGateway,
};
use std::path::Path;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.iter().skip(1).map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--version"]=>println!("research-release-publisher {}", option_env!("MONDAY_SOURCE_REVISION").unwrap_or("unknown")),
        ["budget-plan",root,context,products,policy]=>{
            let policy=release_publisher::read_json(Path::new(policy))?;
            let binding=release_publisher::read_json(Path::new(context))?;
            println!("{}",serde_json::to_string(&release_publisher::budget_plan(Path::new(root),&binding,products,&policy)?)?);
        }
        ["budget-init",policy,context,plan,envelope,ledger]=>{
            let policy=release_publisher::read_json(Path::new(policy))?;
            let binding=release_publisher::read_json(Path::new(context))?;
            let plan=release_publisher::read_json(Path::new(plan))?;
            let envelope=release_publisher::read_json(Path::new(envelope))?;
            let ledger=release_publisher::initialize_budget(&std::env::current_dir()?,&policy,binding,plan,envelope,Path::new(ledger))?;
            println!("{}",serde_json::to_string(&ledger.status()?)?);
        }
        ["budget-summary",ledger]=>{
            let ledger=hft_research_platform::release_budget::Ledger::from_path(Path::new(ledger))?;
            println!("{}",serde_json::to_string(&ledger.status()?)?);
        }
        ["sign-import-admission",policy,admission,key_id,key]=>{
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let admission:release_publisher::ImportAdmission=release_publisher::read_json(Path::new(admission))?;
            let key=release_publisher::read_signing_key(Path::new(key))?;
            let signed=release_publisher::SignedImportAdmission::sign(admission,(*key_id).to_owned(),&key,&policy,chrono::Utc::now().timestamp_millis())?;
            println!("{}",serde_json::to_string(&signed)?);
        }
        ["set-import-admission",policy,admission]=>{
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let admission=release_publisher::read_import_admission(Path::new(admission),&policy)?;
            let ledger=hft_research_platform::postgres::Ledger::connect(&std::env::var("MONDAY_RESEARCH_DATABASE_URL").context("independent admission owner PG URL required")?).await.map_err(|_|anyhow::anyhow!("admission owner PG unavailable"))?;
            ledger.set_build_import_admission(&admission).await?;
            println!("independent Build import admission projected");
        }
        ["oss-check-config",policy,key,manifest,repository,product,image_repository,source,session]=>{
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let key=release_publisher::read_signing_key(Path::new(key))?;
            release_publisher::check_publication_configuration(&policy,&key,Path::new(manifest),repository,product,image_repository)?;
            release_publisher::check_source_authority(&std::env::current_dir()?,repository,source)?;
            ReleaseGateway::oss(policy.oss.as_ref().context("OSS policy required")?,Path::new(session))?.check_oss(source).await?;
            println!("release issuer and OSS configuration verified");
        }
        ["oss-publish",root,request,policy,key,session]=>{
            let request:PublicationRequest=release_publisher::read_json(Path::new(request))?;
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let key=release_publisher::read_signing_key(Path::new(key))?;
            let store=ReleaseGateway::oss(policy.oss.as_ref().context("OSS policy required")?,Path::new(session))?;
            println!("{}",serde_json::to_string(&release_publisher::publish(Path::new(root),&request,&policy,&key,&store).await?)?);
        }
        ["oss-import",root,build,oci,proof,policy,session,admission]=>{
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let store=ReleaseGateway::oss_reader(policy.oss.as_ref().context("OSS policy required")?,Path::new(session))?;
            let ledger=hft_research_platform::postgres::Ledger::connect(&std::env::var("MONDAY_RESEARCH_DATABASE_URL").context("ACK importer PG URL required")?).await.map_err(|_|anyhow::anyhow!("ACK importer PG unavailable"))?;
            println!("{}", release_publisher::import_oss_build(Path::new(root),build,oci,proof,&policy,&store,&ledger,Path::new(admission)).await?);
        }
        ["check-config",policy,key,manifest,repository,product,image_repository,endpoint,token]=>{
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let key=release_publisher::read_signing_key(Path::new(key))?;
            release_publisher::check_publication_configuration(&policy,&key,Path::new(manifest),repository,product,image_repository)?;
            ReleaseGateway::with_tls(endpoint,hft_research_platform::service::read_secret(token)?,&policy.tls)?.check_transport().await?;
            println!("release issuer configuration verified");
        }
        ["scope-plan",root,source,software_run,software_products,product,policy]=>{
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            println!("{}",serde_json::to_string(&release_publisher::scope_plan(Path::new(root),source,software_run.parse()?,software_products,product,&policy)?)?);
        }
        ["plan",root,request,policy]=>{
            let request:PublicationRequest=release_publisher::read_json(Path::new(request))?;
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            println!("{}",serde_json::to_string(&release_publisher::plan(Path::new(root),&request,&policy)?)?);
        }
        ["publish",root,request,policy,key,endpoint,token]=>{
            let request:PublicationRequest=release_publisher::read_json(Path::new(request))?;
            let policy:PublisherPolicy=release_publisher::read_json(Path::new(policy))?;
            let key=release_publisher::read_signing_key(Path::new(key))?;
            let token=hft_research_platform::service::read_secret(token)?;
            let gateway=ReleaseGateway::with_tls(endpoint,token,&policy.tls)?;
            println!("{}",serde_json::to_string(&release_publisher::publish(Path::new(root),&request,&policy,&key,&gateway).await?)?);
        }
        _=>bail!("usage: research-release-publisher --version | budget-plan ROOT CONTEXT SOFTWARE_PRODUCTS POLICY | budget-init POLICY CONTEXT BUDGET_PLAN ENVELOPE LEDGER | budget-summary LEDGER | sign-import-admission POLICY ADMISSION KEY_ID EXISTING_PRIVATE_KEY_FILE | set-import-admission POLICY SIGNED_ADMISSION | oss-check-config POLICY KEY SOFTWARE_MANIFEST REPOSITORY PRODUCT IMAGE_REPOSITORY SOURCE SESSION | oss-publish ROOT REQUEST POLICY KEY SESSION | oss-import ROOT BUILD OCI PROOF POLICY READER_SESSION ADMISSION | check-config POLICY PRIVATE_KEY_FILE SOFTWARE_MANIFEST REPOSITORY PRODUCT IMAGE_REPOSITORY HTTPS_GATEWAY TOKEN_FILE | scope-plan ROOT SOURCE SOFTWARE_RUN SOFTWARE_PRODUCTS PRODUCT POLICY | plan SOURCE_ROOT REQUEST POLICY | publish SOURCE_ROOT REQUEST POLICY PRIVATE_KEY_FILE HTTPS_GATEWAY TOKEN_FILE"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{os::unix::fs::PermissionsExt, process::Command};

    fn executable(path: &Path, body: &str) -> Result<()> {
        std::fs::write(path, body)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
    #[test]
    fn wrapper_reuses_one_ledger_and_reports_failed_or_interrupted_consumption() -> Result<()> {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()?;
        for failure in ["none", "preflight", "term", "summary"] {
            let temp = tempfile::tempdir()?;
            let root = temp.path().canonicalize()?;
            let scripts = root.join(".github/scripts");
            std::fs::create_dir_all(&scripts)?;
            for name in [
                "publish-research-build-release.sh",
                "select-research-oss-policy.jq",
            ] {
                std::fs::copy(
                    source_root.join(".github/scripts").join(name),
                    scripts.join(name),
                )?;
            }
            let runner = root.join("runner");
            let issuer_dir = runner.join("research-release-issuer-target/debug");
            std::fs::create_dir_all(&issuer_dir)?;
            let fakebin = root.join("bin");
            std::fs::create_dir(&fakebin)?;
            executable(&fakebin.join("gh"),"#!/bin/bash\nprintf '%s\\n' '[{\"jobs\":[{\"id\":3,\"name\":\"Publish controller\"}]}]'\n")?;
            executable(
                &issuer_dir.join("research-release-publisher"),
                r#"#!/bin/bash
set -eu
printf '%s\n' "$1" >>"$RUNNER_TEMP/calls"
case "$1" in
budget-plan) test "$4" = controller; printf '%s\n' '{}' ;;
budget-init) test "$5" = "$RUNNER_TEMP/research-publication-native-budget.json"; (set -o noclobber; printf '%s\n' burned >"$6"); printf '%s\n' '{}' ;;
budget-summary) test -f "$2"; [[ $MONDAY_TEST_FAIL != summary ]]; printf '%s\n' '{"schema":"monday.oss-publication-budget-usage.v1","reserved":{"requests":4},"claims":["preflight"]}' ;;
oss-check-config) case "$MONDAY_TEST_FAIL" in preflight) exit 23;; term) kill -TERM "$PPID";exit 23;; esac ;;
plan) printf '%s\n' '{}' ;;
oss-publish) printf '%s\n' '[]' ;;
*) exit 91;;
esac
"#,
            )?;
            executable(
                &issuer_dir.join("research-release-capability"),
                r#"#!/bin/bash
set -eu
test "$#" = 6
test "$5" = "$RUNNER_TEMP/research-oss-budget/2/1/controller/ledger.jsonl"
test -f "$5"
printf '%s\n' "$1" >>"$RUNNER_TEMP/calls"
printf '%s\n' '{}' >"$6"
"#,
            )?;
            let policy = json!({"trust":{"schema":1,"producer_workflow_path":".github/workflows/acr-publish.yml","keys":{"ci":"b".repeat(64)}},
                "key_id":"ci","builder_image":format!("builder@sha256:{}","a".repeat(64)),"image_repositories":{"controller":"registry/controller"},
                "oss_by_product":{"controller":{"bucket":"fixture","region":"cn-hangzhou","endpoint":"https://fixture.oss-cn-hangzhou.aliyuncs.com/",
                    "role_arn":"acs:ram::1:role/fixture","oidc_provider_arn":"acs:ram::1:oidc-provider/fixture","audience":"fixture","subject":"fixture",
                    "publication_namespaces":["research/builds/","research/sources/"],"repository_id":1,"owner_id":2}}});
            let run = |mode: &str| -> Result<std::process::Output> {
                Ok(Command::new("bash")
                    .arg(scripts.join("publish-research-build-release.sh"))
                    .arg(mode)
                    .env(
                        "PATH",
                        format!("{}:{}", fakebin.display(), std::env::var("PATH")?),
                    )
                    .env("RUNNER_TEMP", &runner)
                    .env("MONDAY_RELEASE_POLICY_JSON", policy.to_string())
                    .env("MONDAY_RELEASE_SIGNING_KEY", "c".repeat(64)) // Deliberately synthetic fixture.
                    .env("PRODUCT", "controller")
                    .env("SOFTWARE_PRODUCTS", "controller")
                    .env("PUBLISH_IMAGE_REPOSITORY", "registry/controller")
                    .env("IMAGE_REPOSITORY", "controller")
                    .env(
                        "IMAGE",
                        format!("registry/controller@sha256:{}", "d".repeat(64)),
                    )
                    .env("SOURCE_REVISION", "a".repeat(40))
                    .env("PRODUCER_RUN", "1")
                    .env("GITHUB_REPOSITORY", "owner/repo")
                    .env("GITHUB_RUN_ID", "2")
                    .env("GITHUB_RUN_ATTEMPT", "1")
                    .env("MONDAY_TEST_FAIL", failure)
                    .output()?)
            };
            let output = run("check-config")?;
            assert_eq!(
                output.status.success(),
                failure == "none",
                "unexpected wrapper result: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(
                runner.join("research-publication-budget-usage.json"),
            )?)?;
            if failure == "summary" {
                assert_eq!(summary["usage_known"], false);
            } else {
                assert_eq!(summary["reserved"]["requests"], 4);
            }
            assert!(!summary.to_string().contains(&"c".repeat(64)));
            if failure == "none" {
                assert!(run("publish")?.status.success());
                let calls = std::fs::read_to_string(runner.join("calls"))?;
                assert_eq!(calls.matches("budget-init").count(), 1);
                assert_eq!(calls.matches("budget-summary").count(), 2);
                assert!(calls.contains("oss-source") && calls.contains("oss-publish"));
                assert!(!run("check-config")?.status.success());
            }
        }
        Ok(())
    }
}
