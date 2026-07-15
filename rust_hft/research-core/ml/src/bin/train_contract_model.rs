use clap::Parser;
use hft_research_ml::{train_contract_model, SealedTrainingRequest, Sha256Digest};
use std::{fs, path::PathBuf};

#[derive(Debug, Parser)]
#[command(about = "Train a point-in-time continuous-contract model with Burn")]
struct Args {
    #[arg(long)]
    rows_json: PathBuf,
    #[arg(long)]
    request_json: PathBuf,
    #[arg(long)]
    expected_request_sha256: Sha256Digest,
    #[arg(long)]
    output_dir: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let rows_artifact = fs::read(args.rows_json)?;
    let request_artifact = fs::read(args.request_json)?;
    let request =
        SealedTrainingRequest::from_bytes(&request_artifact, &args.expected_request_sha256)?;
    let saved = train_contract_model(&rows_artifact, &request)?.save_bundle(args.output_dir)?;
    println!("{}", serde_json::to_string_pretty(&saved)?);
    Ok(())
}
