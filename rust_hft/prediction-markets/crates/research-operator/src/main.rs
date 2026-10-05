use clap::Parser;

fn main() -> anyhow::Result<()> {
    hft_prediction_research_operator::run(hft_prediction_research_operator::Cli::parse())
}
