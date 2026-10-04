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
            let url=reqwest::Url::parse(endpoint)?;
            anyhow::ensure!(url.scheme()=="http" && matches!(url.host_str(),Some("127.0.0.1"|"::1")) && url.path()=="/research" && url.query().is_none() && url.username().is_empty() && url.password().is_none(),"tool client requires explicit local endpoint");
            let tool:ResearchTool=read(path)?;tool.validate()?;
            let token=hft_research_platform::service::read_secret(token_path)?;
            let mut response=reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build()?.post(url).bearer_auth(token).json(&tool).send().await.map_err(|_|anyhow::anyhow!("research tool API unavailable"))?.error_for_status().map_err(|_|anyhow::anyhow!("research tool request rejected"))?;
            let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await?{anyhow::ensure!(bytes.len()+chunk.len()<=1024*1024,"tool response too large");bytes.extend_from_slice(&chunk);}
            println!("{}",std::str::from_utf8(&bytes)?);
        }
        ["plan-build",path]=>{let build:hft_research_platform::build::BuildSpec=read(path)?;println!("{}",serde_json::to_string(&build.cargo_arguments()?)?);}
        ["register-build",path]=>{let value:hft_research_platform::build::BuildArtifact=read(path)?;println!("{}",ledger().await?.register_build(&value).await?);}
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
        _ => bail!("usage: researchctl plan-build BUILD | register-build ARTIFACT | subscribe TENANT SESSION RUN | tool ENDPOINT TOKEN_FILE REQUEST | register-experiment TENANT FILE | register-run TENANT FILE | register-session TENANT FILE | snapshot-session TENANT FILE | validate TASK | submit TENANT KEY TASK | register-plan PLAN | view SPEC | cancel ID | status ID"),
    }
    Ok(())
}
