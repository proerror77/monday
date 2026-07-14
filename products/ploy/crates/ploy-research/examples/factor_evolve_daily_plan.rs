use std::path::PathBuf;

use ploy_research::research_os::manager::{
    build_research_manager_plan, validate_evidence_stage, ResearchManagerInput,
};

fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        anyhow::bail!("usage: factor_evolve_daily_plan <input-json> <output-json>");
    }
    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let input: ResearchManagerInput = serde_json::from_str(&std::fs::read_to_string(&input_path)?)?;
    let plan = build_research_manager_plan(&input);
    validate_evidence_stage(&plan.evidence_stage).map_err(anyhow::Error::msg)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, serde_json::to_string_pretty(&plan)? + "\n")?;
    Ok(())
}
