use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ploy_research::{
    canonical_event_ml_architecture, event_ml_architecture_markdown, gate_matrix,
    EVENT_ML_ARCHITECTURE_VERSION,
};

fn main() -> Result<()> {
    let config = Config::parse(env::args().skip(1))?;
    let architecture = canonical_event_ml_architecture();
    let markdown = event_ml_architecture_markdown(&architecture);

    if let Some(output_dir) = config.output_dir.as_ref() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("create output dir {}", output_dir.display()))?;

        let json_path = output_dir.join("event_ml_architecture.json");
        let markdown_path = output_dir.join("event_ml_architecture.md");
        let gate_matrix_path = output_dir.join("event_ml_gate_matrix.json");

        fs::write(
            &json_path,
            serde_json::to_string_pretty(&architecture)
                .context("serialize event ML architecture JSON")?,
        )
        .with_context(|| format!("write {}", json_path.display()))?;
        fs::write(&markdown_path, &markdown)
            .with_context(|| format!("write {}", markdown_path.display()))?;
        fs::write(
            &gate_matrix_path,
            serde_json::to_string_pretty(&gate_matrix(&architecture))
                .context("serialize event ML gate matrix JSON")?,
        )
        .with_context(|| format!("write {}", gate_matrix_path.display()))?;

        eprintln!("artifact_event_ml_architecture={}", json_path.display());
        eprintln!(
            "artifact_event_ml_architecture_md={}",
            markdown_path.display()
        );
        eprintln!(
            "artifact_event_ml_gate_matrix={}",
            gate_matrix_path.display()
        );
    }

    if config.print_markdown {
        print!("{markdown}");
    } else if config.output_dir.is_none() {
        println!(
            "{}",
            serde_json::to_string_pretty(&architecture)
                .context("serialize event ML architecture JSON")?
        );
    }

    eprintln!("event_ml_architecture_version={EVENT_ML_ARCHITECTURE_VERSION}");
    eprintln!("event_ml_architecture_status=ready");
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct Config {
    output_dir: Option<PathBuf>,
    print_markdown: bool,
}

impl Config {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut config = Config::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--output-dir" => {
                    let dir = args.next().context("--output-dir requires a path")?;
                    config.output_dir = Some(PathBuf::from(dir));
                }
                "--print-markdown" => {
                    config.print_markdown = true;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }

        Ok(config)
    }
}

fn print_help() {
    println!(
        r#"Event ML foundation architecture artifact writer

Usage:
  cargo run -p ploy-research --example event_ml_architecture -- [options]

Options:
  --output-dir <dir>   Write event_ml_architecture.json, event_ml_architecture.md,
                       and event_ml_gate_matrix.json into <dir>.
  --print-markdown     Print Markdown instead of JSON to stdout.
  -h, --help           Show this help.
"#
    );
}
