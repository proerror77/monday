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
            let store=ReleaseGateway::oss(policy.oss.as_ref().context("OSS policy required")?,Path::new(session))?;
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
        _=>bail!("usage: research-release-publisher --version | sign-import-admission POLICY ADMISSION KEY_ID EXISTING_PRIVATE_KEY_FILE | set-import-admission POLICY SIGNED_ADMISSION | oss-check-config POLICY KEY SOFTWARE_MANIFEST REPOSITORY PRODUCT IMAGE_REPOSITORY SOURCE SESSION | oss-publish ROOT REQUEST POLICY KEY SESSION | oss-import ROOT BUILD OCI PROOF POLICY READER_SESSION ADMISSION | check-config POLICY PRIVATE_KEY_FILE SOFTWARE_MANIFEST REPOSITORY PRODUCT IMAGE_REPOSITORY HTTPS_GATEWAY TOKEN_FILE | plan SOURCE_ROOT REQUEST POLICY | publish SOURCE_ROOT REQUEST POLICY PRIVATE_KEY_FILE HTTPS_GATEWAY TOKEN_FILE"),
    }
    Ok(())
}
