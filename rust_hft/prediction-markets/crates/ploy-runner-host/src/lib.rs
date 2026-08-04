#[cfg(feature = "ops")]
mod ops;
mod run;

pub fn print_usage() {
    print_usage_for("ploy-runner");
}

fn print_usage_for(program: &str) {
    eprintln!("Usage: {program} [COMMAND] [OPTIONS]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  run                       Run the strategy (default)");
    #[cfg(feature = "ops")]
    eprintln!("  check-db                  Check database data completeness");
    #[cfg(feature = "ops")]
    eprintln!("  collect-markets           Discover Polymarket markets into the local DB catalog");
    #[cfg(feature = "ops")]
    eprintln!(
        "  collect-quotes            Collect orderbook quotes from Polymarket CLOB WebSocket"
    );
    #[cfg(feature = "ops")]
    eprintln!("  collect-pm-trades         Collect public Polymarket trade prints from Data API");
    #[cfg(feature = "ops")]
    eprintln!("  collect-predict-fun       Collect Predict.fun markets and normalized order books");
    #[cfg(feature = "ops")]
    eprintln!("  collect-binance-lob       Collect Binance L2 orderbook depth snapshots");
    #[cfg(feature = "ops")]
    eprintln!("  collect-binance-price     Collect Binance spot trade prices");
    #[cfg(feature = "ops")]
    eprintln!("  collect-binance-aggtrade  Collect Binance aggregated trades");
    #[cfg(feature = "ops")]
    eprintln!("  collect-deribit-iv        Collect Deribit option IV ticks (HTTP poll)");
    #[cfg(feature = "ops")]
    eprintln!("  collect-deribit-greeks    Collect Deribit ATM option greeks (HTTP poll)");
    #[cfg(feature = "ops")]
    eprintln!(
        "  collect-cex-public        Collect Binance Futures and OKX/Bybit/Coinbase/Kraken L2"
    );
    eprintln!();
    eprintln!("Options for 'run':");
    eprintln!("  --config <path>          Unified TOML config file (required)");
    eprintln!("  --deployment-id <id>     Platform deployment identity for order attribution");
    eprintln!("  --output-json <path>     Write a machine-readable runtime evaluation");
    eprintln!("  --dry-run                Force dry-run mode (simulated execution)");
    eprintln!("  --control-generation <generation>  Canonical daemon control generation");
    #[cfg(feature = "ops")]
    ops::print_usage();
}

fn print_mode_usage(program: &str) {
    eprintln!("Usage: {program} --config <path> [--deployment-id <id>] [--dry-run] [--control-generation <generation>]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --config <path>          Unified TOML config file (required)");
    eprintln!("  --deployment-id <id>     Platform deployment identity for order attribution");
    eprintln!("  --output-json <path>     Write a machine-readable runtime evaluation");
    eprintln!("  --dry-run                Force dry-run mode (simulated execution)");
    eprintln!("  --control-generation <generation>  Canonical daemon control generation");
}

fn program_name(args: &[String]) -> String {
    args.first()
        .and_then(|arg| std::path::Path::new(arg).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("ploy-runner")
        .to_string()
}

pub async fn run_with_args(args: Vec<String>) {
    install_tls_crypto_provider();
    init_tracing();

    let command = args.get(1).map(|s| s.as_str());
    match command {
        Some("check-db") => {
            #[cfg(feature = "ops")]
            ops::run_check_db(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The check-db command requires the full/ops runner build");
        }
        Some("collect-quotes") => {
            #[cfg(feature = "ops")]
            ops::run_collect_quotes(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-quotes command requires the full/ops runner build");
        }
        Some("collect-markets") => {
            #[cfg(feature = "ops")]
            ops::run_collect_markets(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-markets command requires the full/ops runner build");
        }
        Some("collect-pm-trades") => {
            #[cfg(feature = "ops")]
            ops::run_collect_pm_trades(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-pm-trades command requires the full/ops runner build");
        }
        Some("collect-predict-fun") => {
            #[cfg(feature = "ops")]
            ops::run_collect_predict_fun(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-predict-fun command requires the full/ops runner build");
        }
        // --- New Binance/Deribit collectors ---
        Some("collect-binance-lob") => {
            #[cfg(feature = "ops")]
            ops::run_collect_binance_lob(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-binance-lob command requires the full/ops runner build");
        }
        Some("collect-binance-price") => {
            #[cfg(feature = "ops")]
            ops::run_collect_binance_price(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-binance-price command requires the full/ops runner build");
        }
        Some("collect-binance-aggtrade") => {
            #[cfg(feature = "ops")]
            ops::run_collect_binance_aggtrade(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-binance-aggtrade command requires the full/ops runner build");
        }
        Some("collect-deribit-iv") => {
            #[cfg(feature = "ops")]
            ops::run_collect_deribit_iv(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-deribit-iv command requires the full/ops runner build");
        }
        Some("collect-deribit-greeks") => {
            #[cfg(feature = "ops")]
            ops::run_collect_deribit_greeks(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-deribit-greeks command requires the full/ops runner build");
        }
        Some("collect-cex-public") => {
            #[cfg(feature = "ops")]
            ops::run_collect_cex_public(&args).await;
            #[cfg(not(feature = "ops"))]
            eprintln!("The collect-cex-public command requires the full/ops runner build");
        }
        // --- End new collectors ---
        Some("run") | None => {
            run::run_command(&args, command).await;
        }
        Some("--help") | Some("-h") => {
            print_usage();
        }
        Some(other) => {
            eprintln!("Unknown command: {other}");
            print_usage();
            std::process::exit(1);
        }
    }
}

pub async fn run_mode_binary(args: Vec<String>) {
    if matches!(args.get(1).map(String::as_str), Some("--help" | "-h")) {
        print_mode_usage(&program_name(&args));
        return;
    }

    run_with_args(normalize_mode_args(args)).await;
}

pub async fn run_with_implicit_run_args(args: Vec<String>) {
    run_with_args(normalize_mode_args(args)).await;
}

fn normalize_mode_args(mut args: Vec<String>) -> Vec<String> {
    match args.get(1).map(String::as_str) {
        None
        | Some(
            "--config" | "--deployment-id" | "--output-json" | "--dry-run" | "--control-generation",
        ) => {
            args.insert(1, "run".to_string());
        }
        _ => {}
    }
    args
}

fn install_tls_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        "failed to install a process-level Rustls CryptoProvider"
    );
}

fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            "info,hyper_util=off,hyper=off,reqwest=off,h2=off,rustls=off"
                .parse()
                .unwrap()
        })
        .add_directive(
            "polymarket_client_sdk::serde_helpers=error"
                .parse()
                .unwrap(),
        );
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init();
}

#[cfg(test)]
mod tls_tests {
    #[test]
    fn runner_installs_a_process_tls_provider() {
        super::install_tls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
