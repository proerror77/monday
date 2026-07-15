use std::fs;
use std::path::Path;
use std::process::Command;

const RETIRED_SOURCE_PATHS: &[&str] = &[
    "apps/ploy-runner",
    "deployment/aws",
    "deploy-docker.sh",
    "docker-compose.prod.yml",
    "docker-compose.yml",
    "nginx.conf",
    "start.sh",
    "stop.sh",
    "tools/polymarket-account-ops",
    "tools/predict-fun-account-ops",
    "ploy-openclaw",
    "src/CLAUDE.md",
    "src/account",
    "src/adapters",
    "src/agent_runtime.rs",
    "src/agents",
    "src/ai_clients",
    "src/analysis",
    "src/api",
    "src/cli",
    "src/collector",
    "src/config",
    "src/config.rs",
    "src/control_plane",
    "src/control_plane.rs",
    "src/coordination",
    "src/coordinator",
    "src/data_plane",
    "src/domain",
    "src/error.rs",
    "src/exchange",
    "src/main_agent_mode",
    "src/main_agent_mode.rs",
    "src/main_commands",
    "src/main_dispatch.rs",
    "src/main_modes",
    "src/main_modes.rs",
    "src/main_runtime.rs",
    "src/ml",
    "src/persistence",
    "src/platform",
    "src/plugins",
    "src/rl",
    "src/safety",
    "src/services",
    "src/signing",
    "src/strategy",
    "src/supervisor",
    "src/tui",
    "src/validation.rs",
];

const RETIRED_TEST_TARGETS: &[&str] = &[
    "examples/api_server.rs",
    "examples/backtest_gamma_scalping.rs",
    "examples/staggered_grid_backtest.rs",
    "examples/test_grok_agent.rs",
    "examples/test_winprob.rs",
    "tests/architecture_gateway_only.rs",
    "tests/engine_store_pg.rs",
    "tests/legacy_live_gate.rs",
    "tests/native_async_traits.rs",
    "tests/staging_workflow.rs",
    "tests/strategy_evaluations_and_deployment_gate.rs",
    "tests/workflow_migrations.rs",
];

#[test]
fn workspace_root_keeps_only_the_shim_surface() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut still_present = Vec::new();
    for relative_path in RETIRED_SOURCE_PATHS
        .iter()
        .chain(RETIRED_TEST_TARGETS.iter())
    {
        if repo_root.join(relative_path).exists() {
            still_present.push(relative_path.to_string());
        }
    }

    assert!(
        still_present.is_empty(),
        "legacy root runtime paths still present:\n{}",
        still_present.join("\n")
    );
}

#[test]
fn openclaw_compatibility_example_rejects_remote_mutations() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rpc = repo_root.join("examples/openclaw/skill-ploy-rpc/bin/ployrpc");
    let ctl = repo_root.join("examples/openclaw/skill-ploy-rpc/bin/ployctl");

    let rpc_output = Command::new("bash")
        .arg(&rpc)
        .arg("pm.submit_limit")
        .arg("{}")
        .env_remove("PLOY_TRADING_HOST")
        .env_remove("PLOY_TRADING_SSH_OPTS")
        .output()
        .expect("run read-only RPC wrapper");
    assert!(!rpc_output.status.success());
    assert!(String::from_utf8_lossy(&rpc_output.stderr).contains("disabled in Monday"));

    let ctl_output = Command::new("bash")
        .arg(&ctl)
        .arg("stop")
        .env_remove("PLOY_TRADING_HOST")
        .env_remove("PLOY_TRADING_SSH_OPTS")
        .output()
        .expect("run read-only control wrapper");
    assert!(!ctl_output.status.success());
    assert!(String::from_utf8_lossy(&ctl_output.stderr).contains("disabled in Monday"));

    for relative in [
        "examples/openclaw/README.md",
        "examples/openclaw/skill-ploy-rpc/SKILL.md",
        "examples/openclaw/skill-ploy-rpc/prompts/autonomous_event_trader.md",
        "examples/openclaw/skill-ploy-rpc/prompts/autonomous_multi_source_trader.md",
        "docs/OPENCLAW_INTEGRATION.md",
    ] {
        let body = fs::read_to_string(repo_root.join(relative)).expect("read OpenClaw document");
        for forbidden in [
            "PLOY_RPC_WRITE_ENABLED",
            "pm.submit_limit",
            "pm.cancel_order",
        ] {
            assert!(
                !body.contains(forbidden),
                "{relative} still advertises retired write surface {forbidden}"
            );
        }
    }
}

fn collect_markdown_files(root: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(root).expect("read documentation directory") {
        let path = entry.expect("documentation entry").path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|value| value.to_str()),
                Some("archive" | "dist" | "node_modules" | "target")
            ) {
                continue;
            }
            collect_markdown_files(&path, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            files.push(path);
        }
    }
}

#[test]
fn standalone_operational_docs_are_explicitly_marked_historical() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for relative_root in [
        "docs",
        "config",
        "examples",
        "ploy-frontend",
        "apps/ploy-agent-sidecar",
        "tasks",
    ] {
        collect_markdown_files(&repo_root.join(relative_root), &mut files);
    }

    let markers = [
        ".github/workflows/deploy-",
        ".github/workflows/approve-",
        ".github/workflows/release-platform",
        "/opt/ploy",
        "tango-1-1",
        "ploy-trade-1",
        "systemctl",
        "PLOY_RPC_WRITE_ENABLED",
        "pm.submit_limit",
        "pm.cancel_order",
        "vercel --prod",
        "git remote add origin",
    ];
    let current_exceptions = ["docs/operations/data-jobs-inventory.md"];
    let mut unmarked = Vec::new();

    for path in files {
        let relative = path
            .strip_prefix(repo_root)
            .expect("documentation path under repository root")
            .to_string_lossy()
            .replace('\\', "/");
        if relative.starts_with("docs/archive/")
            || relative.starts_with("docs/plans/")
            || relative.starts_with("docs/reviews/")
            || relative.starts_with("docs/superpowers/")
            || relative.starts_with("tasks/research_evidence/")
            || (relative.starts_with("tasks/") && relative.contains("_audit_"))
            || current_exceptions.contains(&relative.as_str())
        {
            continue;
        }

        let body = fs::read_to_string(&path).expect("read operational document");
        if markers.iter().any(|marker| body.contains(marker))
            && !body.contains("Historical standalone PLOY")
        {
            unmarked.push(relative);
        }
    }

    assert!(
        unmarked.is_empty(),
        "standalone operational docs lack the historical marker:\n{}",
        unmarked.join("\n")
    );
}

#[test]
fn default_config_does_not_advertise_live_enablement() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let body = fs::read_to_string(repo_root.join("config/default.toml"))
        .expect("read default configuration");

    for retired_instruction in [
        "To enable any live order path",
        "complete the explicit deployment checklist",
    ] {
        assert!(
            !body.contains(retired_instruction),
            "default config still advertises retired live enablement: {retired_instruction}"
        );
    }
    assert!(
        body.contains("separate reviewed Monday change"),
        "default config must name the reviewed Monday authority gate"
    );
}

#[test]
fn monday_polymarket_data_service_is_read_only_and_fail_closed() {
    let ploy_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let monday_root = ploy_root
        .parent()
        .and_then(Path::parent)
        .expect("products/ploy must live inside Monday");
    let config =
        fs::read_to_string(monday_root.join("deployment/aliyun/polymarket-market-tape.toml"))
            .expect("read Polymarket market-data config");
    let service =
        fs::read_to_string(monday_root.join("deployment/aliyun/polymarket-market-tape.service"))
            .expect("read Polymarket market-data service");
    let reference_service = fs::read_to_string(
        monday_root.join("deployment/aliyun/polymarket-reference-collector.service"),
    )
    .expect("read Polymarket reference service");

    for required in [
        "mode = \"dryrun\"",
        "strategy_variant = \"noop\"",
        "market_data_source = \"external_direct\"",
        "record_market_updates_to = \"/data/monday/spool/polymarket/market-updates.ndjson\"",
        "record_market_updates_include_kinds = [\"quote\", \"event_discovered\", \"event_expired\", \"reference_price\"]",
        "record_market_updates_quote_sample_ms = 1000",
        "record_market_updates_event_scoped_quotes = true",
        "symbols = [\"BTCUSDT\", \"ETHUSDT\", \"SOLUSDT\", \"XRPUSDT\", \"DOGEUSDT\", \"HYPEUSDT\", \"BNBUSDT\"]",
    ] {
        assert!(config.contains(required), "config missing {required}");
    }
    for required in [
        "User=hftcollector",
        "--dry-run",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ReadWritePaths=/data/monday/spool/polymarket",
    ] {
        assert!(service.contains(required), "service missing {required}");
    }
    for forbidden in ["live-execution", "PRIVATE_KEY", "EnvironmentFile="] {
        assert!(!service.contains(forbidden), "service contains {forbidden}");
        assert!(
            !reference_service.contains(forbidden),
            "reference service contains {forbidden}"
        );
    }
    for forbidden in ["record_market_updates_quote_depth_levels"] {
        assert!(
            !config.contains(forbidden),
            "full-depth collector config contains {forbidden}"
        );
    }
    for required in [
        "User=hftcollector",
        "polymarket_reference_collector.py",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ReadWritePaths=/data/monday/spool/polymarket-reference",
    ] {
        assert!(
            reference_service.contains(required),
            "reference service missing {required}"
        );
    }
}
