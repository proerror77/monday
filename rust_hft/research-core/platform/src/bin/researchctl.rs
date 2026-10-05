use anyhow::{bail, Context, Result};
use hft_research_platform::{
    orchestrator::TaskSpec,
    postgres::Ledger,
    preparation::PreparationPlan,
    research::{Experiment, ResearchTool, Run, Session, SessionSnapshot},
};

async fn ledger() -> Result<Ledger> {
    Ledger::connect(
        &std::env::var("MONDAY_RESEARCH_DATABASE_URL").context("PG database URL required")?,
    )
    .await
}

fn read<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    read_bounded(path, 1024 * 1024)
}
fn read_bounded<T: serde::de::DeserializeOwned>(path: &str, max_bytes: u64) -> Result<T> {
    use rustix::fs::{open, Mode, OFlags};
    use std::io::Read;
    let file = std::fs::File::from(open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= max_bytes,
        "input must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("input exceeds bound");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.iter().skip(1).map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["tool",endpoint,token_path,path] => {
            let tool: ResearchTool = read(path)?;
            let tls = hft_research_platform::transport::TlsConfig {
                ca_file: std::env::var_os("MONDAY_RESEARCH_TOOL_CA_FILE").map(Into::into),
                identity_file: std::env::var_os("MONDAY_RESEARCH_TOOL_IDENTITY_FILE").map(Into::into),
            };
            let client = hft_research_platform::session::ResearchClient::from_file(endpoint, token_path.into(), &tls)?;
            println!("{}", client.execute(&tool).await?);
        }
        ["plan-build",path]=>{let build:hft_research_platform::build::BuildSpec=read(path)?;println!("{}",serde_json::to_string(&build.cargo_arguments()?)?);}
        ["register-build",path,release]=>{
            let value:hft_research_platform::build::BuildArtifact=read(path)?;
            let signed:hft_research_platform::release::SignedBuildRelease=read(release)?;
            let trust_path=std::env::var("MONDAY_RESEARCH_BUILD_TRUST_FILE").context("operator release trust file required")?;
            let trust:hft_research_platform::release::BuildReleaseTrust=read(&trust_path)?;
            let verified=trust.verify(&value,&signed)?;
            println!("{}",ledger().await?.register_build(&verified).await?);
        }
        ["register-native-admission",path]=>{
            let signed:hft_research_platform::admission::SignedNativeAdmission=read(path)?;
            let trust_path=std::env::var("MONDAY_RESEARCH_NATIVE_ADMISSION_TRUST_FILE").context("operator native reservation trust file required")?;
            let trust:hft_research_platform::admission::NativeAdmissionTrust=read(&trust_path)?;
            let verified=trust.verify(&signed)?;
            println!("{}",ledger().await?.register_native_admission(&verified).await?);
        }
        ["register-native-campaign-inputs",signed_path,manifest_path,blocks_path]=>{
            let signed:hft_research_platform::admission::SignedNativeAdmission=read(signed_path)?;
            let trust_path=std::env::var("MONDAY_RESEARCH_NATIVE_ADMISSION_TRUST_FILE").context("operator native reservation trust file required")?;
            let trust:hft_research_platform::admission::NativeAdmissionTrust=read(&trust_path)?;
            let native=trust.verify(&signed)?;
            let manifest:hft_cex_research_input::campaign::CampaignPreparedInputsV1=read_bounded(manifest_path,64*1024*1024)?;
            let mut source=hft_cex_research_input::prepared::SharedFiles::new(std::path::Path::new(blocks_path))?;
            let max_bytes=u64::from(native.evidence().admission.task_spec.profile.memory_mib)*1024*1024/2;
            let verified=hft_research_platform::campaign::verify_inputs(&native,manifest,&mut source,max_bytes)?;
            println!("{}",ledger().await?.register_campaign_inputs(&verified,&native).await?);
        }
        ["register-native-request-revocation",path]=>{
            let signed:hft_research_platform::revocation::SignedNativeRequestRevocation=read(path)?;
            let trust_path=std::env::var("MONDAY_RESEARCH_NATIVE_ADMISSION_TRUST_FILE").context("operator native reservation trust file required")?;
            let trust:hft_research_platform::admission::NativeAdmissionTrust=read(&trust_path)?;
            let verified=trust.verify_revocation(&signed)?;
            println!("{}",ledger().await?.register_native_request_revocation(&verified).await?);
        }
        ["subscribe",tenant,session,run]=>{ledger().await?.subscribe(tenant,session,run).await?;println!("subscribed");}
        ["register-experiment",tenant,path]=>{let value:Experiment=read(path)?;println!("{}",ledger().await?.register_experiment(tenant,&value).await?);}
        ["register-run",tenant,path]=>{let value:Run=read(path)?;println!("{}",ledger().await?.register_run(tenant,&value).await?);}
        ["register-session",tenant,path]=>{let value:Session=read(path)?;println!("{}",ledger().await?.register_session(tenant,&value).await?);}
        ["snapshot-session",tenant,path]=>{let value:SessionSnapshot=read(path)?;println!("{}",ledger().await?.snapshot_session(tenant,&value).await?);}
        ["validate", path] => { let task: TaskSpec = read(path)?; println!("{}", task.id()?); }
        ["submit", tenant, key, path] => { let task: TaskSpec = read(path)?; println!("{}", ledger().await?.submit(tenant, key, task).await?); }
        ["register-plan", path] => { let plan: PreparationPlan = read(path)?; println!("{}", ledger().await?.register_plan(&plan).await?); }
        ["view",path] => { let spec: hft_cex_research_input::data::DataViewSpec=read(path)?; println!("{}",serde_json::to_string(&ledger().await?.find_view(&spec).await?.context("view has not been published")?)?); }
        ["cancel", id] => { ledger().await?.cancel(id).await?; println!("cancel_requested"); }
        ["status", id] => { println!("{}", serde_json::to_string(&ledger().await?.read(id).await?)?); }
        _ => bail!("usage: researchctl plan-build BUILD | register-build ARTIFACT SIGNED_RELEASE | register-native-admission SIGNED_NATIVE_RESERVATION | register-native-campaign-inputs SIGNED_NATIVE_RESERVATION COLLECTION BLOCK_DIRECTORY | register-native-request-revocation SIGNED_NATIVE_REVOCATION | subscribe TENANT SESSION RUN | tool ENDPOINT TOKEN_FILE REQUEST | register-experiment TENANT FILE | register-run TENANT FILE | register-session TENANT FILE | snapshot-session TENANT FILE | validate TASK | submit TENANT KEY TASK | register-plan PLAN | view SPEC | cancel ID | status ID"),
    }
    Ok(())
}
