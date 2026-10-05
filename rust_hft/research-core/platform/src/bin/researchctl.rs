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
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
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
        ["render-foundation",path,output] => {
            let inventory: hft_research_platform::foundation::Inventory = read(path)?;
            let assets = inventory.render()?;
            let directory = std::path::Path::new(output);
            anyhow::ensure!(directory.is_absolute(), "absolute new output directory required");
            std::fs::create_dir(directory)?;
            for (name,contents) in assets {
                use std::io::Write;
                std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join(name))?.write_all(contents.as_bytes())?;
            }
            println!("rendered paused offline assets");
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
        _ => bail!("usage: researchctl render-foundation INVENTORY OUTPUT_DIRECTORY | plan-build BUILD | register-build ARTIFACT SIGNED_RELEASE | subscribe TENANT SESSION RUN | tool ENDPOINT TOKEN_FILE REQUEST | register-experiment TENANT FILE | register-run TENANT FILE | register-session TENANT FILE | snapshot-session TENANT FILE | validate TASK | submit TENANT KEY TASK | register-plan PLAN | view SPEC | cancel ID | status ID"),
    }
    Ok(())
}
