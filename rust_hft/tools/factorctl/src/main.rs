use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "factorctl")]
#[command(about = "Factor Bank operator helper")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the Factor Bank promotion statuses.
    Statuses,
}

fn main() {
    let args = Args::parse();
    match args.command {
        Command::Statuses => {
            for (status, label) in [
                (hft_factor_bank::FactorStatus::Generated, "generated"),
                (
                    hft_factor_bank::FactorStatus::QuickTestPassed,
                    "quick_test_passed",
                ),
                (
                    hft_factor_bank::FactorStatus::FullBacktestPassed,
                    "full_backtest_passed",
                ),
                (hft_factor_bank::FactorStatus::PaperTrading, "paper_trading"),
                (hft_factor_bank::FactorStatus::LiveShadow, "live_shadow"),
                (
                    hft_factor_bank::FactorStatus::LiveSmallPendingApproval,
                    "live_small_pending_approval",
                ),
                (hft_factor_bank::FactorStatus::LiveSmall, "live_small"),
                (
                    hft_factor_bank::FactorStatus::LiveFullCandidate,
                    "live_full_candidate",
                ),
                (hft_factor_bank::FactorStatus::Decayed, "decayed"),
                (hft_factor_bank::FactorStatus::Retired, "retired"),
                (hft_factor_bank::FactorStatus::Rejected, "rejected"),
            ] {
                let executable = status.executable_in_mvp();
                println!("{label}\texecutable_in_mvp={executable}");
            }
        }
    }
}
