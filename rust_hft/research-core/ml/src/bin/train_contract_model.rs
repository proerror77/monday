use clap::{ArgGroup, Parser};
use hft_cex_research_input::data::{BlockRef, BlockSource, Exit, PublishedView, VerifiedCache};
use hft_research_ml::{
    shared_input::train_shared_contract_model, train_contract_model, SealedTrainingRequest,
    Sha256Digest,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Parser)]
#[command(about = "Train a point-in-time continuous-contract model with Burn")]
#[command(group(ArgGroup::new("input").required(true).args(["rows_json", "prepared_view"])))]
struct Args {
    #[arg(long)]
    rows_json: Option<PathBuf>,
    #[arg(long, requires = "prepared_root")]
    prepared_view: Option<PathBuf>,
    #[arg(long, requires = "prepared_view")]
    prepared_root: Option<PathBuf>,
    /// Maximum encoded/decoded input bytes. Account training tensors separately.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    input_memory_bytes: u64,
    #[arg(long)]
    request_json: PathBuf,
    #[arg(long)]
    expected_request_sha256: Sha256Digest,
    #[arg(long)]
    output_dir: PathBuf,
}

fn read_regular(path: &Path, maximum: u64) -> anyhow::Result<Vec<u8>> {
    use rustix::fs::{open, Mode, OFlags};
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = fs::File::from(fd);
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file() && meta.len() <= maximum,
        "input must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= maximum, "input grew beyond its limit");
    Ok(bytes)
}

struct PreparedFiles(PathBuf);
impl BlockSource for PreparedFiles {
    fn read(&mut self, block: &BlockRef) -> anyhow::Result<Vec<u8>> {
        // VerifiedCache owns decoding/integrity admission; the transport reads
        // only the manifest's digest basename without adding another hash pass.
        read_regular(
            &self.0.join(format!("{}.mondaybin", block.sha256)),
            block.bytes,
        )
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        (1..=512 * 1024 * 1024).contains(&args.input_memory_bytes),
        "input memory must be bounded"
    );
    let request_artifact = read_regular(&args.request_json, 1024 * 1024)?;
    let request =
        SealedTrainingRequest::from_bytes(&request_artifact, &args.expected_request_sha256)?;
    let trained = match (args.rows_json, args.prepared_view, args.prepared_root) {
        (Some(path), None, None) => {
            train_contract_model(&read_regular(&path, args.input_memory_bytes)?, &request)?
        }
        (None, Some(view_path), Some(root)) => {
            anyhow::ensure!(
                root.is_absolute() && root.canonicalize()? == root && root.is_dir(),
                "prepared root must be canonical"
            );
            let view: PublishedView =
                serde_json::from_slice(&read_regular(&view_path, 1024 * 1024)?)?;
            let input = VerifiedCache::new(args.input_memory_bytes)?.load(
                &view,
                request.request().rows_artifact_sha256().as_str(),
                Exit::Training,
                &mut PreparedFiles(root),
            )?;
            train_shared_contract_model(&input, &request)?
        }
        _ => anyhow::bail!("select exactly one complete input source"),
    };
    let saved = trained.save_bundle(args.output_dir)?;
    println!("{}", serde_json::to_string_pretty(&saved)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_cli_requires_one_complete_source_and_refuses_aliased_files() -> anyhow::Result<()> {
        let base = [
            "train-contract-model",
            "--request-json",
            "request.json",
            "--expected-request-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--output-dir",
            "output",
        ];
        for source in [
            vec!["--prepared-view", "view.json"],
            vec![
                "--rows-json",
                "rows.json",
                "--prepared-view",
                "view.json",
                "--prepared-root",
                "/prepared",
            ],
        ] {
            assert!(Args::try_parse_from(base.into_iter().chain(source)).is_err());
        }
        assert!(Args::try_parse_from(base.into_iter().chain([
            "--prepared-view",
            "view.json",
            "--prepared-root",
            "/prepared"
        ]))
        .is_ok());
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("rows");
        fs::write(&file, b"bytes")?;
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&file, &alias)?;
        assert!(read_regular(&alias, 10).is_err());
        assert!(read_regular(&file, 4).is_err());
        assert_eq!(read_regular(&file, 5)?, b"bytes");
        Ok(())
    }
}
