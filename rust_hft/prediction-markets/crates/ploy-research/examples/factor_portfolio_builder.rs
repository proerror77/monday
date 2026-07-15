use std::path::PathBuf;

use ploy_research::research_os::portfolio::{build_factor_portfolio, FactorPortfolioInput};

fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        anyhow::bail!("usage: factor_portfolio_builder <input-json> <output-json>");
    }
    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let input: FactorPortfolioInput = serde_json::from_str(&std::fs::read_to_string(&input_path)?)?;
    let output = build_factor_portfolio(&input);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, serde_json::to_string_pretty(&output)? + "\n")?;
    Ok(())
}
